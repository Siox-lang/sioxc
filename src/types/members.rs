//! Field and method access: existence, visibility, and argument checks.

use super::*;

impl<'a> Checker<'a> {
    /// `p.nosuch` on a struct/entity: the field access lowered to `Unknown`, so
    /// the driver silently carried no value. Struct receivers are checked, as
    /// are entity ports and bus ports through their view's backing struct; the
    /// check walks a struct derivation chain so an inherited field counts as
    /// present.
    pub(super) fn check_field_exists(
        &mut self,
        base: &Expr,
        field: &Ident,
        sym: &HashMap<String, Ty>,
    ) {
        let base_ty = self.type_of(base, sym);
        if let Ty::Tuple(elems) = &base_ty {
            if !field
                .text
                .parse::<usize>()
                .is_ok_and(|index| index < elems.len())
            {
                self.error_with_help(
                    codes::UNKNOWN_NAME,
                    field.span,
                    format!(
                        "`{}` has no position `{}`",
                        self.ty_display(&base_ty),
                        field.text
                    ),
                    format!(
                        "a tuple's positions are `.0` to `.{}`",
                        elems.len().saturating_sub(1)
                    ),
                );
            }
            return;
        }
        // A scalar, an enum value or an array has no fields: `x.foo` on an
        // `integer` passed and did not lower (#259).
        let fieldless = match &base_ty {
            Ty::Integer | Ty::Real | Ty::Char => true,
            Ty::Array { family: None, .. } => true,
            Ty::Named(id) => self
                .definition_key(*id)
                .is_some_and(|key| self.enum_variants.contains_key(&key)),
            _ => false,
        };
        if fieldless {
            self.error(
                codes::UNKNOWN_NAME,
                field.span,
                format!(
                    "`{}` has no field `{}`",
                    self.ty_display(&base_ty),
                    field.text
                ),
            );
            return;
        }
        let Some(head) = self.ty_head(&base_ty) else {
            return;
        };
        // `s.ready()` parses as a call over a *field* node, so a method name
        // reaches here too — it is not a missing field.
        if self
            .methods
            .contains_key(&(head.clone(), field.text.clone()))
        {
            return;
        }
        // An entity value exposes ports, not the storage declarations in its
        // implementation. Previously every non-port field returned silently
        // here because entities are not structs, so `instance.hidden` could
        // survive semantic checking even though lowering had no such member.
        if let Some(ports) = self.entities.get(&head) {
            if ports.iter().any(|port| port.name == field.text) {
                return;
            }
            if let Some(declaration) = self
                .private_entity_members
                .get(&(head.clone(), field.text.clone()))
                .copied()
            {
                self.sink.emit(
                    Diagnostic::error(format!(
                        "implementation member `{}::{}` is private to the entity",
                        self.key_leaf(&head),
                        field.text
                    ))
                    .with_code(codes::PRIVATE_MEMBER)
                    .at(field.span)
                    .label(declaration, "private implementation member declared here")
                    .help("expose entity behavior through ports"),
                );
            } else {
                self.error(
                    codes::UNKNOWN_NAME,
                    field.span,
                    format!(
                        "entity `{}` has no port or method `{}`",
                        self.key_leaf(&head),
                        field.text
                    ),
                );
            }
            return;
        }
        // A bus port (`bus: Stream Source`) types as the *view*, which owns no
        // fields, so the walk below found no struct and returned silently —
        // leaving every field access through a bus unchecked.
        let through_view = self.view_backing(&head);
        let head = through_view.clone().unwrap_or(head);
        // The backing struct's own methods are callable through the bus, and
        // reach here as field nodes just as the view's own methods do.
        if self
            .methods
            .contains_key(&(head.clone(), field.text.clone()))
        {
            return;
        }
        // Walk `struct B : A` so an inherited field counts as present.
        let mut seen = HashSet::new();
        let mut cur = Some(head.clone());
        while let Some(name) = cur {
            if !seen.insert(name.clone()) {
                return; // cyclic derivation: already diagnosed elsewhere
            }
            let Some((base_ty, fields)) = self.structs.get(&name) else {
                // A newtype over a scalar (`struct Number(integer)`) ends
                // the walk with no such field: `n.xyz` passed (#221). Only
                // an unknown starting type is someone else's to report.
                if name == head {
                    return;
                }
                break;
            };
            if fields.contains(&field.text) {
                // Applying a view is the explicit structural interface for
                // its backing storage. It exposes the fields it names without
                // making raw `Struct.field` access public everywhere.
                if through_view.is_none() {
                    self.check_field_visibility(&name, field);
                }
                return;
            }
            cur = base_ty.as_ref().and_then(|ty| self.type_key(ty));
        }
        let fields = self
            .structs
            .get(&head)
            .map(|(_, f)| f.clone())
            .unwrap_or_default();
        let mut d = Diagnostic::error(format!(
            "`{}` has no field `{}`",
            self.key_leaf(&head),
            field.text
        ))
        .with_code(codes::UNKNOWN_NAME)
        .at(field.span);
        if !fields.is_empty() {
            d = d.help(format!("it has: {}", fields.join(", ")));
        }
        self.sink.emit(d);
    }

    /// Reject access to a private field from outside the owning type's module
    /// (E-P024).
    pub(super) fn check_field_visibility(&mut self, owner: &str, field: &Ident) {
        let Some(visibility) = self
            .field_visibility
            .get(&(owner.to_string(), field.text.clone()))
            .cloned()
        else {
            return;
        };
        if visibility.is_pub || self.member_access_allowed(&visibility, field.span) {
            return;
        }
        self.sink.emit(
            Diagnostic::error(format!(
                "field `{}.{}` is private to its owning type",
                owner, field.text
            ))
            .with_code(codes::PRIVATE_MEMBER)
            .at(field.span)
            .label(visibility.span, "declared private here")
            .help(format!(
                "mark the field `pub`, or expose the operation through a `pub fn` in `impl {}`",
                visibility.owner
            )),
        );
    }

    /// A method call whose name no impl provides for the receiver's type.
    /// It used to lower to `Unknown` — silently producing a driver with an
    /// unknown value, or (worse) an unknown *condition*, so an `if
    /// clk.typo()` block quietly became combinational.
    ///
    /// Deliberately conservative: only a receiver whose type head is known
    /// *and* which has at least one method recorded is checked, so a type
    /// whose methods this stage never collected can't false-positive.
    pub(super) fn check_method_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        sym: &HashMap<String, Ty>,
    ) {
        let Expr::Field { base, field, span } = callee else {
            return;
        };
        let recv = self.type_of(base, sym);
        // A runtime array (`T[]`) grows by `push`, as a Rust `Vec` does.
        if let Ty::Array {
            elem,
            len: 0,
            family: None,
            ..
        } = &recv
        {
            if !matches!(elem.as_ref(), Ty::Char) {
                match (field.text.as_str(), args) {
                    ("push", [value]) => {
                        if !self.assignable(elem, value, sym) {
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr_span(value),
                                format!(
                                    "cannot push {} onto {}",
                                    self.ty_display(&self.type_of(value, sym)),
                                    self.ty_display(&recv)
                                ),
                            );
                        }
                    }
                    _ => self.error_with_help(
                        codes::INVALID_METHOD_CALL,
                        *span,
                        format!("a runtime array has no method `{}`", field.text),
                        "a runtime array has `push(value)`; its length is `xs'length`".to_string(),
                    ),
                }
                return;
            }
        }
        // A fixed array has only its blanket methods, and a tuple none:
        // `xs.push(4)` and `p.foo()` passed and did not lower (#205).
        let fixed_array = matches!(recv, Ty::Array { family: None, len, .. } if len > 0);
        if (fixed_array && !self.array_methods.contains(&field.text))
            || matches!(recv, Ty::Tuple(_))
        {
            self.error_with_help(
                codes::INVALID_METHOD_CALL,
                *span,
                format!(
                    "`{}` has no method `{}`",
                    self.ty_display(&recv),
                    field.text
                ),
                if fixed_array {
                    "a fixed-size array cannot grow; its length is `xs'length`".to_string()
                } else {
                    "a tuple has fields `.0`, `.1`, ..., not methods".to_string()
                },
            );
            return;
        }
        let Some(head) = self.ty_head(&recv) else {
            return;
        };
        let key = (head.clone(), field.text.clone());
        if self.methods.contains_key(&key) {
            if self.entity_names.contains(&head) && !is_self_value(base) {
                self.error_with_help(
                    codes::PRIVATE_MEMBER,
                    *span,
                    format!(
                        "entity method `{}::{}` cannot be called through an instance yet",
                        self.key_leaf(&head),
                        field.text
                    ),
                    "expose behavior through ports; cross-hierarchy method calls do not yet have defined hardware semantics".to_string(),
                );
                return;
            }
            if !self.check_method_visibility(&key, field.span) {
                return;
            }
            if !self.method_has_self.get(&key).copied().unwrap_or(false) {
                self.error_with_help(
                    codes::INVALID_METHOD_CALL,
                    *span,
                    format!(
                        "associated function `{}::{}` has no `self` receiver",
                        head, field.text
                    ),
                    format!("call it as `{}::{}(...)`", head, field.text),
                );
                return;
            }
            self.check_collected_method_args(&head, &field.text, *span, args, sym);
            return;
        }
        // A view receiver may use inherent methods of its backing struct.
        if let Some(backing) = self.view_backing(&head) {
            let backing_key = (backing.clone(), field.text.clone());
            if self.methods.contains_key(&backing_key) {
                if !self.check_method_visibility(&backing_key, field.span) {
                    return;
                }
                if !self
                    .method_has_self
                    .get(&backing_key)
                    .copied()
                    .unwrap_or(false)
                {
                    self.error_with_help(
                        codes::INVALID_METHOD_CALL,
                        *span,
                        format!(
                            "associated function `{}::{}` has no `self` receiver",
                            backing, field.text
                        ),
                        format!("call it as `{}::{}(...)`", backing, field.text),
                    );
                    return;
                }
                self.check_collected_method_args(&backing, &field.text, *span, args, sym);
                return;
            }
        }
        // Only complain about a type we know all the methods of: one with
        // methods, or a declared struct or enum, which has none without an
        // impl (`p.foo()` on a bare struct passed and did not lower, #222).
        if !self.methods.keys().any(|(h, _)| *h == head)
            && !self.structs.contains_key(&head)
            && !self.enum_variants.contains_key(&head)
            && !matches!(head.as_str(), "integer" | "natural" | "real" | "Char")
        {
            return;
        }
        let mut known: Vec<&str> = self
            .methods
            .keys()
            .filter(|(h, _)| *h == head)
            .map(|(_, m)| m.as_str())
            .collect();
        known.sort();
        let mut d = Diagnostic::error(format!("`{head}` has no method `{}`", field.text))
            .with_code(codes::INVALID_METHOD_CALL)
            .at(*span);
        if !known.is_empty() {
            d = d.help(format!("it has: {}", known.join(", ")));
        }
        self.sink.emit(d);
    }

    /// Check `Type::function(args)` against the same collected impl signature
    /// as receiver syntax, while enforcing that this declaration has no
    /// `self` parameter.
    pub(super) fn check_associated_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        sym: &HashMap<String, Ty>,
    ) {
        let Expr::Path(path) = callee else { return };
        if path.segments.len() < 2 {
            return;
        }
        let owner_name = &path.segments[path.segments.len() - 2];
        let owner = self
            .resolved
            .resolved(owner_name.span)
            .and_then(|id| self.definition_key(id))
            .unwrap_or_else(|| owner_name.text.clone());
        let name = &path.segments[path.segments.len() - 1].text;
        // A type's own `fn default(..)` replaces the derived `T::default()`.
        let declared = self.methods.contains_key(&(owner.clone(), name.clone()));
        if name == "default" && !declared && self.is_conversion_name(&owner) {
            if !args.is_empty() {
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(callee),
                    format!(
                        "`{owner}::default` takes no arguments, but {} were given",
                        args.len()
                    ),
                );
            }
            return;
        }
        // `T::new()` was the default's old name; `new` is now an ordinary
        // constructor name a type declares itself.
        if name == "new" && !declared && self.is_conversion_name(&owner) {
            if args.is_empty() {
                self.warn(
                    codes::RENAMED_DEFAULT,
                    expr_span(callee),
                    format!("`{0}::new()` is now `{0}::default()`", owner_name.text),
                    "the default constructor was renamed after Rust's `Default`; `T()` \
                     still builds it, and `new` is free for a constructor a type declares",
                );
            } else {
                self.error_with_help(
                    codes::UNKNOWN_NAME,
                    expr_span(callee),
                    format!("`{owner}` has no function `new`"),
                    format!(
                        "the default constructor is `{0}::default()`, or `{0}()`",
                        owner_name.text
                    ),
                );
            }
            return;
        }
        let key = (owner.clone(), name.clone());
        if !self.methods.contains_key(&key) {
            return;
        }
        if !self.check_method_visibility(&key, expr_span(callee)) {
            return;
        }
        if self.method_has_self.get(&key).copied().unwrap_or(false) {
            self.error_with_help(
                codes::INVALID_METHOD_CALL,
                expr_span(callee),
                format!("method `{owner}.{name}` needs a `self` receiver"),
                format!("call it on a `{owner}` value, e.g. `value.{name}(...)`"),
            );
            return;
        }
        self.check_collected_method_args(&owner, name, expr_span(callee), args, sym);
    }

    /// Reject a call to a private method from outside its module. Returns
    /// whether the call is allowed.
    pub(super) fn check_method_visibility(
        &mut self,
        key: &(String, String),
        use_span: Span,
    ) -> bool {
        let Some(visibility) = self.method_visibility.get(key).cloned() else {
            return true;
        };
        if visibility.is_pub || self.member_access_allowed(&visibility, use_span) {
            return true;
        }
        let boundary = if visibility.type_private {
            "its owning type".to_string()
        } else {
            format!("module `{}`", visibility.module)
        };
        self.sink.emit(
            Diagnostic::error(format!(
                "method `{}::{}` is private to {boundary}",
                key.0, key.1,
            ))
            .with_code(codes::PRIVATE_MEMBER)
            .at(use_span)
            .label(visibility.span, "declared private here")
            .help("mark the inherent method `pub` to include it in the type's API"),
        );
        false
    }

    /// Whether a member is reachable from `use_span`. A private representation
    /// member belongs to the nominal type's module, not to whoever holds a value
    /// of it.
    pub(super) fn member_access_allowed(
        &self,
        visibility: &MemberVisibility,
        use_span: Span,
    ) -> bool {
        // A private representation member belongs to the nominal type, not to
        // every declaration that happens to share its module. The module
        // check keeps a foreign trait impl from acquiring private access just
        // by targeting the type; the owner check excludes module functions and
        // impls of neighboring types.
        if self.module_of(use_span) != visibility.module {
            return false;
        }
        if !visibility.type_private {
            return true;
        }
        let self_owner = self
            .current_self_ty
            .borrow()
            .as_ref()
            .and_then(|ty| self.ty_head(ty));
        self_owner.as_deref() == Some(visibility.owner.as_str())
            || self.current_impl_owner.borrow().as_deref() == Some(visibility.owner.as_str())
    }

    /// Check a method call's arguments against the declared signature.
    pub(super) fn check_collected_method_args(
        &mut self,
        owner: &str,
        name: &str,
        span: Span,
        args: &[Expr],
        sym: &HashMap<String, Ty>,
    ) {
        let key = (owner.to_string(), name.to_string());
        let Some(params) = self.method_param_types.get(&key).cloned() else {
            return;
        };
        if args.len() != params.len() {
            self.error(
                codes::TYPE_MISMATCH,
                span,
                format!(
                    "`{owner}::{name}` takes {} argument(s) but {} were given",
                    params.len(),
                    args.len()
                ),
            );
            return;
        }
        let owner_ty = self.ty_from_head(owner);
        // Another overload of this name that takes these arguments
        // (`regs.index(2)` beside `Index<Range, _>`'s `index`).
        let overloads = self.method_overloads.get(&key).cloned().unwrap_or_default();
        if overloads.len() > 1
            && overloads.iter().any(|overload| {
                overload.len() == args.len()
                    && args.iter().zip(overload).all(|(argument, declared)| {
                        declared.as_ref().is_none_or(|declared| {
                            let expected = self.ast_ty_for_owner(declared, &owner_ty);
                            matches!(expected, Ty::Error)
                                || self.assignable(&expected, argument, sym)
                        })
                    })
            })
        {
            return;
        }
        for (argument, declared) in args.iter().zip(params.iter()) {
            let Some(declared) = declared else { continue };
            let expected = self.ast_ty_for_owner(declared, &owner_ty);
            if self.check_struct_literal_for_ty(&expected, argument, sym)
                || matches!(expected, Ty::Error)
                || self.assignable(&expected, argument, sym)
            {
                continue;
            }
            let actual = self.type_of(argument, sym);
            if matches!(actual, Ty::Error) {
                continue;
            }
            self.error_with_help(
                codes::TYPE_MISMATCH,
                expr_span(argument),
                format!(
                    "cannot pass {} to the {} parameter of `{owner}::{name}`",
                    self.ty_display(&actual),
                    self.ty_display(&expected)
                ),
                format!(
                    "wrap it in a conversion, e.g. `{}(...)`",
                    self.ty_display(&expected)
                ),
            );
        }
    }
}
