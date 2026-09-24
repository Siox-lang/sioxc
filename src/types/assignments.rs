//! Assignment targets and initializers, `let`/`const` rules, attribute
//! values, and generic-bound satisfaction.

use super::*;

impl<'a> Checker<'a> {
    /// Spec 3.18: flag a write to an `in` port. Three shapes are illegal: the
    /// bare port (`a = ..`), an `in` bus-mode leaf (`bus.ready = ..`), and any
    /// field/index of a plain (non-bus) `in` port (`a[3] = ..`, `p.f = ..`).
    pub(super) fn check_write_target(&mut self, target: &Expr, dirs: &PortDirs) {
        // The exact name for the bare / bus-leaf case.
        let exact = match target {
            Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
            Expr::Field { .. } => path_string(target),
            _ => None,
        };
        // The root name for a field/index write into a plain `in` port.
        let root = match target {
            Expr::Field { .. } | Expr::Index { .. } => target_root_name(target),
            _ => None,
        };
        let bad = exact
            .as_deref()
            .filter(|n| dirs.illegal.contains(*n))
            .map(str::to_string)
            .or_else(|| root.filter(|r| dirs.plain_in_roots.contains(r)));
        if let Some(root) = target_root_name(target) {
            if dirs.consts.contains(&root) {
                self.error_with_help(
                    codes::INVALID_ASSIGN_TARGET,
                    expr_span(target),
                    format!("cannot assign to `{root}`, which is a `const`"),
                    "a `const` is fixed at elaboration; declare it as a `let` \
                     if it needs to be driven"
                        .to_string(),
                );
                return;
            }
        }
        if let Some(name) = bad {
            self.sink.emit(
                Diagnostic::error(format!("cannot assign to input port `{name}`"))
                    .with_code(codes::WRITE_TO_INPUT_PORT)
                    .at(expr_span(target))
                    .help("input ports are read-only inside the entity; drive it from the instantiating scope"),
            );
        }
    }

    /// Check a custom indexed write. Returns true when `target` is a
    /// non-intrinsic index operation, so ordinary lvalue assignment checking
    /// must not treat the `Index` read result as storage.
    pub(super) fn check_index_assign(
        &mut self,
        target: &Expr,
        value: &Expr,
        sym: &HashMap<String, Ty>,
    ) -> bool {
        let Expr::Index { base, index, span } = target else {
            return false;
        };
        let base_ty = self.type_of(base, sym);
        if matches!(base_ty, Ty::Array { .. }) {
            return false;
        }
        let Some(owner) = self.type_kind_name(&base_ty) else {
            return false;
        };
        if matches!(index.as_ref(), Expr::PartialRange { .. }) {
            self.error(
                codes::TYPE_MISMATCH,
                *span,
                format!(
                    "partial range indexing on `{owner}` has no declared bounds; use an explicit `left..right` range"
                ),
            );
            return true;
        }
        let input = if matches!(
            index.as_ref(),
            Expr::Range { .. } | Expr::PartialRange { .. }
        ) {
            self.type_kind_name(&self.ty_from_head("Range"))
        } else {
            self.type_kind_name(&self.type_of(index, sym))
        };
        let value_ty = self.type_kind_name(&self.type_of(value, sym));
        let found = self
            .index_sigs
            .get(&("IndexAssign".to_string(), owner.clone()))
            .is_some_and(|sigs| {
                sigs.iter().any(|(i, v)| {
                    Self::index_contract_type_matches(i, input.as_deref(), &owner)
                        && Self::index_contract_type_matches(v, value_ty.as_deref(), &owner)
                })
            });
        if !found {
            self.error(
                codes::TYPE_MISMATCH,
                *span,
                format!(
                    "indexed assignment on `{owner}` needs `impl IndexAssign<{}, {}> for {owner}`",
                    input.as_deref().unwrap_or("_"),
                    value_ty.as_deref().unwrap_or("_"),
                ),
            );
        }
        true
    }

    /// Whether an index contract's declared element type matches the actual one,
    /// treating an absent declaration as a match.
    pub(super) fn index_contract_type_matches(
        declared: &Option<String>,
        actual: Option<&str>,
        owner: &str,
    ) -> bool {
        declared.is_none()
            || declared.as_deref() == actual
            || (declared.as_deref() == Some("Self") && actual == Some(owner))
    }

    /// Spec 3.5: an attribute's value must match the type its declaration gives.
    pub(super) fn check_attr_value(&mut self, a: &Attr) {
        let name = a
            .name
            .segments
            .last()
            .map(|s| s.text.as_str())
            .unwrap_or("");
        let Some(value) = &a.value else {
            // A declared attribute with a value type needs one. A bare `#[speed]`
            // on `attr speed: integer` used to pass unexamined and was carried
            // through elaboration into `--emit tree` as `#[speed]`, so a
            // synthesis or constraint backend reading it found an attribute with
            // no number in it. `Bool` is exempt: a bare flag reads as `true`, the
            // way Bool flag attributes such as `#[test]` do.
            match self.attr_value_kinds.get(name).copied() {
                Some(AttrValueTy::Integer) => self.error_with_help(
                    codes::INVALID_ATTR_VALUE_TYPE,
                    a.name.span,
                    format!("attribute `{name}` needs an integer value"),
                    format!(
                        "write `#[{name} = <n>]`; it is declared with a value type, \
                             so a bare `#[{name}]` carries nothing to the backend"
                    ),
                ),
                Some(AttrValueTy::Str) => self.error_with_help(
                    codes::INVALID_ATTR_VALUE_TYPE,
                    a.name.span,
                    format!("attribute `{name}` needs a string value"),
                    format!(
                        "write `#[{name} = \"…\"]`; it is declared with a value type, \
                             so a bare `#[{name}]` carries nothing to the backend"
                    ),
                ),
                _ => {}
            }
            return;
        };
        let expected = self.attr_value_kinds.get(name).copied();
        let ok = match expected {
            Some(AttrValueTy::Bool) => {
                matches!(value, Expr::Path(p) if p.segments.len() == 2 && p.segments[0].text == "Bool")
            }
            Some(AttrValueTy::Str) => matches!(value, Expr::StrLit { .. }),
            Some(AttrValueTy::Integer) => matches!(value, Expr::Int { .. }),
            // Unknown attribute (reported by resolve) or an `Other`-typed one.
            _ => true,
        };
        if !ok {
            let want = match expected {
                Some(AttrValueTy::Bool) => "a Bool",
                Some(AttrValueTy::Str) => "a string",
                Some(AttrValueTy::Integer) => "an integer",
                _ => "a different",
            };
            self.error(
                codes::INVALID_ATTR_VALUE_TYPE,
                expr_span(value),
                format!("attribute `{name}` expects {want} value"),
            );
        }
    }

    /// Phase 1 is type-strict: every `let` binding declares its type
    /// (`let x: T [= e]`), never inferring it from the value. A bare
    /// `let x = e` is rejected — including the old instance form
    /// `let dut = Sub { .. }`, which is now `let dut: Sub = { .. }`.
    pub(super) fn require_let_annotation(&mut self, l: &LetDecl) {
        if l.ty.is_some() {
            return;
        }
        let mut diag = Diagnostic::error(format!("`let {}` needs a type annotation", l.name.text))
            .with_code(codes::MISSING_TYPE_ANNOTATION)
            .at(l.span);
        // Point at the clean form for the common instance case.
        if let Some(Expr::Construct { ty: Some(t), .. }) = &l.value {
            if let Some(head) = type_head_name(t) {
                diag = diag.help(format!("write `let {}: {} = {{ .. }};`", l.name.text, head));
            }
        } else {
            diag = diag.help(format!("write `let {}: <type> = ...;`", l.name.text));
        }
        self.sink.emit(diag);
    }

    /// An entity is a hardware instance, not a compile-time value, so it may be
    /// declared with `let` but never `const` (`const dut: Counter = ..`).
    pub(super) fn check_const_not_entity(&mut self, c: &ConstDecl) {
        let Some(head) = self.type_key(&c.ty) else {
            return;
        };
        // Use the resolved definition so a generic parameter that shadows an
        // entity name isn't misjudged.
        let is_entity = type_head_span(&c.ty)
            .and_then(|s| self.resolved.resolved(s))
            .and_then(|id| self.resolved.def(id))
            .map(|d| d.kind == DefKind::Entity)
            .unwrap_or_else(|| self.entities.contains_key(&head));
        if is_entity {
            let shown = self.key_leaf(&head);
            self.error(
                codes::CONST_ENTITY_INSTANCE,
                c.span,
                format!("`{shown}` is an entity instance, not a constant — declare it with `let`"),
            );
        }
    }

    /// Spec 3.17: a `let name: T = e` initializer must be assignable to `T`.
    pub(super) fn check_init(
        &mut self,
        decl_ty: Option<&Type>,
        value: &Expr,
        sym: &HashMap<String, Ty>,
    ) {
        let Some(t) = decl_ty else { return };
        self.check_value_range(t, value);
        let lhs = self.ast_ty(t);
        // `read<T>` constructs either one `T`, a fixed array of `T`, or a
        // string. Its dynamic/context-sized result deliberately has no
        // standalone `Ty`, so validate it against the declared destination
        // here rather than letting `Ty::Error` silently accept every shape.
        if let Some(requested_type) = read_call_type(value) {
            let requested = self.ast_ty(requested_type);
            let text = matches!(
                requested,
                Ty::Array {
                    ref elem,
                    family: None,
                    ..
                } if matches!(elem.as_ref(), Ty::Char)
            );
            let compatible = if text {
                matches!(
                    lhs,
                    Ty::Array {
                        ref elem,
                        family: None,
                        ..
                    } if matches!(elem.as_ref(), Ty::Char)
                )
            } else {
                lhs == requested
                    || matches!(
                        lhs,
                        Ty::Array {
                            ref elem,
                            family: None,
                            ..
                        } if elem.as_ref() == &requested
                    )
            };
            if !compatible {
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(value),
                    format!(
                        "`read<{}>` cannot initialize {}; declare one value or an array of the requested type",
                        crate::syntax::pretty::type_str(requested_type),
                        self.ty_display(&lhs)
                    ),
                );
            }
            return;
        }
        // `let x: Named = { .. }` is a construction (instance/struct literal),
        // not a data assignment: a positional/empty block lexes as a concat,
        // and a dotted one as a name-less construct. Either way it is checked
        // structurally by elaboration, not by initializer compatibility.
        if matches!(lhs, Ty::Named(_))
            && matches!(value, Expr::Construct { .. } | Expr::Concat { .. })
        {
            return;
        }
        if !matches!(lhs, Ty::Error) && !self.assignable(&lhs, value, sym) {
            let rhs = self.type_of(value, sym);
            let mut diag = Diagnostic::error(format!(
                "cannot initialize {} with {} without an explicit conversion",
                self.ty_display(&lhs),
                self.ty_display(&rhs)
            ))
            .with_code(codes::TYPE_MISMATCH)
            .at(expr_span(value));
            if let Some(h) = strlit_help(&lhs, value) {
                diag = diag.help(h);
            }
            self.sink.emit(diag);
        }
    }

    /// Spec 3.17: the right-hand side of `target = value` must be assignable to
    /// the target's type. Only fires when the target type is known.
    pub(super) fn check_assignment(
        &mut self,
        target: &Expr,
        value: &Expr,
        sym: &HashMap<String, Ty>,
    ) {
        let lhs = self.type_of(target, sym);
        if self.check_struct_literal_for_ty(&lhs, value, sym) {
            return;
        }
        if !matches!(lhs, Ty::Error) && !self.assignable(&lhs, value, sym) {
            let rhs = self.type_of(value, sym);
            let help = strlit_help(&lhs, value).unwrap_or_else(|| {
                format!(
                    "wrap it in a conversion, e.g. `{}(...)`",
                    self.ty_display(&lhs)
                )
            });
            self.sink.emit(
                Diagnostic::error(format!(
                    "cannot assign {} to {} without an explicit conversion",
                    self.ty_display(&rhs),
                    self.ty_display(&lhs)
                ))
                .with_code(codes::TYPE_MISMATCH)
                .at(expr_span(value))
                .help(help),
            );
        }
    }

    /// Whether `value` may be assigned to a target of type `lhs` without an
    /// explicit conversion. Integer and logic *literals* are polymorphic; an
    /// `Error` type on either side suppresses the check.
    /// Whether `id` is an enum declaring the character variant `ch`.
    pub(super) fn enum_has_char_variant(&self, id: crate::resolve::DefId, ch: char) -> bool {
        let Some(key) = self.definition_key(id) else {
            return false;
        };
        self.enum_variants
            .get(&key)
            .is_some_and(|vars| vars.iter().any(|v| v.trim_matches('\'') == ch.to_string()))
    }

    /// Enforce a generic fn's type contracts at the call site (spec: generic
    /// bounds). Each type parameter is inferred from value parameters whose
    /// declared type names it. Repeated uses must agree, and a bound `T: Tr`
    /// requires the inferred type to satisfy `Tr`. Fns inline, so the call
    /// *is* the monomorphization — checking here gives an early, clear error
    /// instead of a post-inline one.
    pub(super) fn check_generic_bounds(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        sym: &HashMap<String, Ty>,
    ) {
        let Expr::Path(p) = callee else { return };
        let Some(name) = p.segments.last() else {
            return;
        };
        if self
            .associated_owner_key(p)
            .is_some_and(|owner| self.methods.contains_key(&(owner, name.text.clone())))
        {
            return;
        }
        let Some(function) = self.function_id(p) else {
            return;
        };
        let Some((generics, params)) = self.generic_fns.get(&function).cloned() else {
            return;
        };
        for gp in &generics {
            let occurrences: Vec<&Expr> = params
                .iter()
                .zip(args)
                .filter_map(|(declared, argument)| {
                    declared
                        .as_ref()
                        .filter(|ty| Self::is_direct_type_param(ty, &gp.name.text))
                        .map(|_| argument)
                })
                .collect();

            // Infer from the first non-literal value. Integer/character/bit
            // literals are contextual and may adopt that inferred type.
            let inferred_expr = occurrences
                .iter()
                .copied()
                .find(|argument| !Self::is_contextual_literal(argument))
                .or_else(|| occurrences.first().copied());
            let inferred = inferred_expr.map(|argument| self.type_of(argument, sym));

            if let Some(expected) = inferred.as_ref().filter(|ty| !matches!(ty, Ty::Error)) {
                for argument in &occurrences {
                    let actual = self.type_of(argument, sym);
                    if matches!(actual, Ty::Error)
                        || if Self::is_contextual_literal(argument) {
                            self.assignable(expected, argument, sym)
                        } else {
                            &actual == expected
                        }
                    {
                        continue;
                    }
                    self.error_with_help(
                        codes::TYPE_MISMATCH,
                        expr_span(argument),
                        format!(
                            "generic parameter `{}` was inferred as {}, but this argument is {}",
                            gp.name.text,
                            self.ty_display(expected),
                            self.ty_display(&actual)
                        ),
                        format!(
                            "pass one consistent type for every `{}` parameter, or convert this argument explicitly",
                            gp.name.text
                        ),
                    );
                }
            }

            let Some(bound) = &gp.bound else { continue };
            let Some(trait_name) = self.trait_type_key(bound) else {
                continue;
            };
            let Some(ty) = inferred else { continue };
            if !self.satisfies(&ty, &trait_name) {
                let name = self.ty_display(&ty);
                let shown_trait = self.key_leaf(&trait_name);
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(callee),
                    format!(
                        "`{name}` does not satisfy the bound `{}: {shown_trait}`",
                        gp.name.text
                    ),
                );
            }
        }
    }

    /// Whether a type is exactly the bare parameter `name`, rather than
    /// something merely mentioning it.
    pub(super) fn is_direct_type_param(ty: &Type, name: &str) -> bool {
        matches!(ty, Type::Path(path) if path.segments.len() == 1 && path.segments[0].text == name)
    }

    /// Whether an expression is a contextual literal, which takes its type from
    /// the surrounding context rather than carrying one.
    pub(super) fn is_contextual_literal(expression: &Expr) -> bool {
        matches!(
            expression,
            Expr::Int { text, .. } if !text.contains('.')
        ) || matches!(
            expression,
            Expr::CharLit { .. } | Expr::BitStrLit { .. } | Expr::StrLit { .. }
        )
    }

    /// Whether `ty` satisfies trait bound `trait_name`. A named struct/enum
    /// must have an explicit `impl Tr for it`; kernel scalars and vectors are
    /// assumed to carry the built-in capabilities (arithmetic, comparison), so
    /// they are accepted leniently — this catches a custom type missing the
    /// impl without false-flagging unsigned/signed/etc.
    pub(super) fn satisfies(&self, ty: &Ty, trait_name: &str) -> bool {
        match self.type_kind_name(ty) {
            Some(kind) => {
                if self.has_impl(trait_name, &kind)
                    || self
                        .trait_impls
                        .get(trait_name)
                        .is_some_and(|implementors| {
                            implementors.contains(&kind)
                            // Applied views are stored by their full nominal
                            // identity (`Controller@Spi`), while expression
                            // typing names the visible view (`Controller`).
                            // Either matching applied identity satisfies a
                            // capability bound on that view.
                            || implementors
                                .iter()
                                .any(|name| name.starts_with(&format!("{kind}@")))
                        })
                {
                    return true;
                }
                // A named (struct/enum) type without the impl fails; a kernel
                // scalar / vector is accepted (built-in capability).
                !matches!(ty, Ty::Named(_))
            }
            None => true,
        }
    }
}
