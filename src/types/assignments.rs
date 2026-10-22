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
        // A leaf of an array-of-views port (`down[2].ready`) is checked by its
        // index-free name (`down.ready`), the view applying per element.
        let element_leaf = match target {
            Expr::Field { .. } => index_free_path(target),
            _ => None,
        };
        let bad = exact
            .as_deref()
            .filter(|n| dirs.illegal.contains(*n))
            .map(str::to_string)
            .or_else(|| {
                element_leaf
                    .filter(|n| dirs.illegal.contains(n))
                    .map(|_| crate::syntax::pretty::expr_string(target))
            })
            .or_else(|| root.filter(|r| dirs.plain_in_roots.contains(r)));
        // A module-level `const` resolves as one; an impl's is collected.
        let mut base = target;
        while let Expr::Field { base: inner, .. } | Expr::Index { base: inner, .. } = base {
            base = inner;
        }
        let module_const = matches!(base, Expr::Path(path)
            if self.resolved.resolved(path.span).and_then(|definition| self.resolved.kind_of(definition))
                == Some(DefKind::Const));
        if let Some(root) = target_root_name(target) {
            if dirs.loop_vars.contains(&root) {
                self.error_with_help(
                    codes::INVALID_ASSIGN_TARGET,
                    expr_span(target),
                    format!("cannot assign to `{root}`, which is a `for` loop variable"),
                    "each iteration binds the loop variable afresh; copy it into a \
                     `let` to compute with it"
                        .to_string(),
                );
                return;
            }
            if dirs.consts.contains(&root) || module_const {
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

    /// An attribute declaration's own checks: its name may not shadow a
    /// system attribute, since `x'length` must keep meaning the system query,
    /// and its default must be a value of its type.
    pub(super) fn check_attr_default(&mut self, a: &AttrDecl) {
        let name = a.name.text.as_str();
        if SYS_ATTRS.contains(&name) || PHASE2_ATTRS.contains(&name) {
            self.error(
                codes::DUPLICATE_ITEM,
                a.name.span,
                format!(
                    "`{name}` is a system attribute (`x'{name}`); a declared attribute \
                     cannot share its name"
                ),
            );
        }
        if let Some(default) = &a.default {
            self.check_attr_value(&Attr {
                name: Path {
                    segments: vec![a.name.clone()],
                    span: a.name.span,
                },
                value: Some(default.clone()),
                span: a.span,
                directive: false,
            });
        }
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
        if l.span == l.name.span {
            // A position of a tuple `let`.
            diag = diag.help(format!(
                "write `{}: <type>` in the tuple, or give the whole tuple a type: \
                 `let (..): (A, B) = ..;`",
                l.name.text
            ));
        } else if let Some(Expr::Construct { ty: Some(t), .. }) = &l.value {
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
        // `{}` is a construction for a named type, and no bits for a vector.
        if matches!(lhs, Ty::Array { .. })
            && matches!(value, Expr::Concat { parts, .. } if parts.is_empty())
        {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(value),
                "an empty concatenation has no bits to initialize a vector with".to_string(),
            );
            return;
        }
        if self.report_bitless_concat(&lhs, value, sym) {
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
        self.check_literal_fit(&lhs, value);
    }

    /// A constant that a sized vector cannot hold is an error in an
    /// initializer or assignment, as it already is in a conversion or a
    /// comparison: `let k: unsigned[8] = 300;` stored 44 in silence.
    pub(super) fn check_literal_fit(&mut self, lhs: &Ty, value: &Expr) {
        match (lhs, value) {
            (
                Ty::Array {
                    family: Some(_),
                    len,
                    ..
                },
                _,
            ) => {
                if let Some(constant) = self.constant_value(value) {
                    self.check_fits_width(constant, *len, expr_span(value));
                }
            }
            // Each element of an array literal, too: `[1, 20]` into
            // `unsigned[4][2]` stored 4 in silence.
            (
                Ty::Array {
                    elem, family: None, ..
                },
                Expr::Array { elems, .. },
            ) => {
                for element in elems {
                    self.check_literal_fit(elem, element);
                }
            }
            _ => {}
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
        if self.report_bitless_concat(&lhs, value, sym) {
            return;
        }
        if !matches!(lhs, Ty::Error) && !self.assignable(&lhs, value, sym) {
            let rhs = self.type_of(value, sym);
            // Suggest `T(...)` only where that conversion is defined: a named
            // type without a route from the value's type would reject it.
            let route = match (self.type_kind_name(&lhs), self.type_kind_name(&rhs)) {
                (Some(target), Some(source))
                    if self.structs.contains_key(&target)
                        || self.enum_variants.contains_key(&target) =>
                {
                    self.conversion_exists(&target, &source)
                }
                _ => true,
            };
            let help = strlit_help(&lhs, value).unwrap_or_else(|| {
                // A tuple or an array has no conversion to wrap it in: it is
                // built from converted parts.
                if matches!(lhs, Ty::Tuple(_)) {
                    "build the tuple from converted parts, e.g. `(T(a), U(b))`".to_string()
                } else if matches!(lhs, Ty::Array { family: None, len, .. } if len > 0) {
                    "build the array from converted elements, e.g. `[T(a), T(b)]`".to_string()
                } else if route {
                    format!(
                        "wrap it in a conversion, e.g. `{}(...)`",
                        self.ty_display(&lhs)
                    )
                } else {
                    format!(
                        "no `From<{}> for {}` exists; add one, or produce a {} another way",
                        self.ty_display(&rhs),
                        self.ty_display(&lhs),
                        self.ty_display(&lhs)
                    )
                }
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
        self.check_literal_fit(&lhs, value);
    }

    /// Whether `value` may be assigned to a target of type `lhs` without an
    /// explicit conversion. Integer and logic *literals* are polymorphic; an
    /// `Error` type on either side suppresses the check.
    /// Whether `id` is an enum declaring the character variant `ch`.
    /// An enum whose every variant is a character literal (`Bit`, `Logic`).
    pub(super) fn is_char_enum(&self, id: crate::resolve::DefId) -> bool {
        let Some(key) = self.definition_key(id) else {
            return false;
        };
        self.enum_variants.get(&key).is_some_and(|vars| {
            !vars.is_empty()
                && vars
                    .iter()
                    .all(|v| v.len() == 3 && v.starts_with('\'') && v.ends_with('\''))
        })
    }

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
        self.check_instantiation(function, &generics, &params, args, sym);
    }

    /// Check a generic function's body for this call's type arguments: the
    /// spec checks "each call", but no call did, so `x.no_such_method()` in a
    /// generic body failed only in lowering (#241). Each distinct set of
    /// arguments is checked once; the tables lowering reads are left as the
    /// abstract body had them.
    fn check_instantiation(
        &mut self,
        function: DefId,
        generics: &[Param],
        params: &[Option<Type>],
        args: &[Expr],
        sym: &HashMap<String, Ty>,
    ) {
        let Some(declaration) = self.generic_bodies.get(&function).copied() else {
            return;
        };
        let Some(body) = declaration.body.as_ref() else {
            return;
        };
        if self.instantiating.borrow().contains(&function) {
            return;
        }
        let mut bindings = HashMap::new();
        for gp in generics {
            let value_parameter = gp
                .bound
                .as_ref()
                .and_then(type_head_name)
                .is_some_and(|head| head == "integer");
            if value_parameter {
                continue;
            }
            let bound = params.iter().zip(args).find_map(|(declared, argument)| {
                Self::bind_type_param(
                    declared.as_ref()?,
                    &self.type_of(argument, sym),
                    &gp.name.text,
                )
            });
            match bound {
                Some(ty) if !matches!(ty, Ty::Error) => {
                    bindings.insert(gp.name.text.clone(), ty);
                }
                _ => return,
            }
        }
        if bindings.is_empty() {
            return;
        }
        let mut shown: Vec<String> = bindings
            .iter()
            .map(|(name, ty)| format!("{name}={}", self.ty_display(ty)))
            .collect();
        shown.sort();
        if !self
            .instantiated
            .borrow_mut()
            .insert(format!("{function:?}:{}", shown.join(",")))
        {
            return;
        }
        let expr_types = self.expr_types.borrow().clone();
        let promotions = self.promotions.clone();
        let type_attr_reads = self.type_attr_reads.clone();
        let covered = self.covered_tuple_matches.clone();
        let saved = self.type_bindings.replace(bindings);
        self.instantiating.borrow_mut().push(function);
        self.check_function_block(declaration, body, None, false);
        self.instantiating.borrow_mut().pop();
        self.type_bindings.replace(saved);
        self.expr_types.replace(expr_types);
        self.promotions = promotions;
        self.type_attr_reads = type_attr_reads;
        self.covered_tuple_matches = covered;
    }

    /// Whether a type is exactly the bare parameter `name`, rather than
    /// something merely mentioning it.
    pub(super) fn is_direct_type_param(ty: &Type, name: &str) -> bool {
        matches!(ty, Type::Path(path) if path.segments.len() == 1 && path.segments[0].text == name)
    }

    /// Whether an expression is a contextual literal, which takes its type from
    /// the surrounding context rather than carrying one.
    /// Whether a value of `ty` is bits a concatenation can join: a packed
    /// vector, a single-bit enum (`Bit`, `Logic`), or an array of those.
    pub(super) fn concatenable(&self, ty: &Ty) -> bool {
        let bit = |ty: &Ty| match ty {
            Ty::Named(id) => self
                .definition_key(*id)
                .is_some_and(|key| self.has_impl("LogicEncoding", &key)),
            _ => false,
        };
        match ty {
            Ty::Array {
                family: Some(_), ..
            } => true,
            Ty::Array {
                family: None, elem, ..
            } => bit(elem),
            other => bit(other),
        }
    }

    /// A concatenation into a vector joins bits: `{ 1.5, 2.5 }` and
    /// `{ points, points }` were sized by element counts and did not lower
    /// (#197, #215). Reports each part with no bits; true when one was.
    pub(super) fn report_bitless_concat(
        &mut self,
        lhs: &Ty,
        value: &Expr,
        sym: &HashMap<String, Ty>,
    ) -> bool {
        let (Ty::Array { .. }, Expr::Concat { parts, .. }) = (lhs, value) else {
            return false;
        };
        let mut reported = false;
        for part in parts {
            // A literal takes its bits from context.
            if Self::is_contextual_literal(part) {
                continue;
            }
            let ty = self.type_of(part, sym);
            if matches!(ty, Ty::Error) || self.concatenable(&ty) {
                continue;
            }
            self.error_with_help(
                codes::TYPE_MISMATCH,
                expr_span(part),
                format!(
                    "a concatenation joins bits, and `{}` is a `{}`",
                    crate::syntax::pretty::expr_string(part),
                    self.ty_display(&ty)
                ),
                "convert it to a bit vector first, e.g. `unsigned[8](x)`".to_string(),
            );
            reported = true;
        }
        reported
    }

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
                // scalar / vector has the built-in capabilities of core and
                // std traits, but not a user trait's (#231).
                !matches!(ty, Ty::Named(_)) && Self::builtin_trait(trait_name)
            }
            None => Self::builtin_trait(trait_name),
        }
    }

    /// A compiler, core or std trait, whose capabilities kernel types have
    /// without a source impl. `run_double(10)` with `T: Doubler` passed and
    /// called a method `integer` does not have (#231).
    fn builtin_trait(trait_name: &str) -> bool {
        !trait_name.contains("::")
            || trait_name.starts_with("core::")
            || trait_name.starts_with("std::")
    }
}

/// A place's dotted name with its element indices dropped: `down[2].ready` is
/// `down.ready`.
fn index_free_path(e: &Expr) -> Option<String> {
    match e {
        Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        Expr::Field { base, field, .. } => {
            Some(format!("{}.{}", index_free_path(base)?, field.text))
        }
        Expr::Index { base, .. } => index_free_path(base),
        _ => None,
    }
}
