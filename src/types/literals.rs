//! Struct literals and declared value ranges.

use super::*;

impl<'a> Checker<'a> {
    /// Field names and values in a struct literal, against the declared type.
    /// Instance construction reuses `Construct`, so this only fires when the
    /// target names a known struct — an entity is not in `structs`.
    pub(super) fn check_struct_literal_fields(&mut self, l: &LetDecl, sym: &HashMap<String, Ty>) {
        let Some(ty) = &l.ty else { return };
        let Some(value) = &l.value else { return };
        self.check_struct_literal_value(ty, value, sym);
    }

    /// Check a struct literal against its declared type. Privacy constrains
    /// constructing the representation, not every use of the value.
    pub(super) fn check_struct_literal_value(
        &mut self,
        declared: &Type,
        value: &Expr,
        sym: &HashMap<String, Ty>,
    ) {
        // Privacy constrains representation construction, not every value
        // expression assigned to a struct-typed slot. A call, operator result,
        // or another variable already denotes a constructed value.
        if !matches!(value, Expr::Construct { .. } | Expr::Concat { .. }) {
            return;
        }
        let Some(ty) = self.resolve_alias_type(declared) else {
            return;
        };
        let Some(head) = self.type_key(&ty) else {
            return;
        };
        if !self.structs.contains_key(&head) {
            return;
        }
        let expected = self.ast_ty(&ty);
        self.check_struct_literal_for_head(&expected, &head, value, sym);
    }

    /// Validate a contextual struct literal when only the consumer's semantic
    /// type is available (assignments, returns, and call arguments). Returns
    /// true when this was a struct-literal context, so the caller does not also
    /// emit a generic mismatch for the literal's intentionally context-free
    /// `Ty::Error`.
    pub(super) fn check_struct_literal_for_ty(
        &mut self,
        expected: &Ty,
        value: &Expr,
        sym: &HashMap<String, Ty>,
    ) -> bool {
        if !matches!(value, Expr::Construct { .. } | Expr::Concat { .. }) {
            return false;
        }
        let Ty::Named(id) = expected else {
            return false;
        };
        let Some(head) = self.definition_key(*id) else {
            return false;
        };
        if !self.structs.contains_key(&head) {
            return false;
        }
        self.check_struct_literal_for_head(expected, &head, value, sym);
        true
    }

    /// Check a struct literal against a known struct head, reporting missing,
    /// unknown or ill-typed fields.
    pub(super) fn check_struct_literal_for_head(
        &mut self,
        expected: &Ty,
        head: &str,
        value: &Expr,
        sym: &HashMap<String, Ty>,
    ) {
        let fields = self.base_struct_fields_named(head);
        if fields.is_empty() {
            return;
        }

        // Expected-vs-explicit type belongs to the consumer relationship, not
        // to the literal itself. Run it before the single-shot guard so an
        // expression walk may validate `B { .. }` and a later assignment may
        // still diagnose that `A` was required.
        if let Expr::Construct {
            ty: Some(actual),
            span,
            ..
        } = value
        {
            let actual = self.ast_ty(actual);
            if !compatible(expected, &actual) {
                self.error(
                    codes::TYPE_MISMATCH,
                    *span,
                    format!(
                        "cannot construct {} where {} is required",
                        self.ty_display(&actual),
                        self.ty_display(expected)
                    ),
                );
                return;
            }
        }

        let literal_span = expr_span(value);
        if !self
            .checked_struct_literals
            .borrow_mut()
            .insert(literal_span)
        {
            return;
        }

        if let Some(private) = fields.iter().find_map(|field| {
            let visibility = self.field_visibility_for(head, field)?;
            (!visibility.is_pub && !self.member_access_allowed(&visibility, expr_span(value)))
                .then_some(visibility)
        }) {
            self.sink.emit(
                Diagnostic::error(format!(
                    "cannot construct `{head}` here because it has private fields"
                ))
                .with_code(codes::PRIVATE_MEMBER)
                .at(expr_span(value))
                .label(private.span, "private field declared here")
                .help(format!(
                    "construct it in `impl {head}` and expose a `pub` constructor, or make its representation fields `pub`"
                )),
            );
            return;
        }

        let (args, spread, span) = match value {
            Expr::Construct {
                args, spread, span, ..
            } => (args.as_slice(), spread.as_deref(), *span),
            // A name-less positional literal (`{ a, b }`) is represented as
            // concatenation until its declared struct type supplies context.
            Expr::Concat { parts, span } => {
                if parts.len() > fields.len() {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        format!(
                            "literal for `{head}` has {} values but only {} fields",
                            parts.len(),
                            fields.len()
                        ),
                    );
                }
                for (field, value) in fields.iter().zip(parts) {
                    self.check_struct_field_value(head, field, value, sym);
                }
                return;
            }
            _ => return,
        };

        if let Some(base) = spread {
            if !self.assignable(expected, base, sym) {
                let actual = self.type_of(base, sym);
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(base),
                    format!(
                        "struct spread for `{head}` has type {}, expected {}",
                        self.ty_display(&actual),
                        self.ty_display(expected)
                    ),
                );
            }
        }

        let mut seen: HashSet<String> = HashSet::new();
        let mut positional = false;
        for (position, c) in args.iter().enumerate() {
            match &c.field {
                // A misspelled name was dropped whole: the field kept its
                // default and the literal still type-checked.
                Some(f) if !fields.iter().any(|n| n == &f.text) => self.error_with_help(
                    codes::TYPE_MISMATCH,
                    f.span,
                    format!("struct `{head}` has no field `{}`", f.text),
                    format!("`{head}` has: {}", fields.join(", ")),
                ),
                Some(f) => {
                    seen.insert(f.text.clone());
                    if let Some(value) = &c.value {
                        self.check_struct_field_value(head, &f.text, value, sym);
                    }
                }
                None => {
                    positional = true;
                    if let (Some(field), Some(value)) = (fields.get(position), &c.value) {
                        self.check_struct_field_value(head, field, value, sym);
                    }
                }
            }
        }
        if positional && args.len() > fields.len() {
            self.error(
                codes::TYPE_MISMATCH,
                span,
                format!(
                    "literal for `{head}` has {} values but only {} fields",
                    args.len(),
                    fields.len()
                ),
            );
        }
        // A spread supplies the rest, and the positional form is bound by
        // ordinal elsewhere.
        if spread.is_some() || positional {
            return;
        }
        let missing: Vec<&str> = fields
            .iter()
            .filter(|f| !seen.contains(*f))
            .map(|f| f.as_str())
            .collect();
        if !missing.is_empty() {
            self.sink.emit(
                Diagnostic::warning(format!(
                    "literal for `{head}` leaves {} at the default value",
                    missing
                        .iter()
                        .map(|f| format!("`{f}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .with_code(codes::INCOMPLETE_STRUCT_LITERAL)
                .at(span)
                .help("give every field a value, or copy the rest with `{ ..base, .x = v }`"),
            );
        }
    }

    /// Check one field's value against the field's declared type.
    pub(super) fn check_struct_field_value(
        &mut self,
        owner: &str,
        field: &str,
        value: &Expr,
        sym: &HashMap<String, Ty>,
    ) {
        let Some(declared) = self.field_decl_ty(owner, field) else {
            return;
        };
        let expected = self.ast_ty(&declared);

        // A name-less nested literal gets its type from this field. Recurse so
        // every nested leaf is contextualized, instead of accepting it merely
        // because an unknown expression type suppresses compatibility checks.
        let nested_struct = self
            .resolve_alias_type(&declared)
            .and_then(|ty| self.type_key(&ty))
            .is_some_and(|head| self.structs.contains_key(&head));
        if nested_struct && matches!(value, Expr::Construct { .. } | Expr::Concat { .. }) {
            self.check_struct_literal_value(&declared, value, sym);
            return;
        }

        if !matches!(expected, Ty::Error) && !self.assignable(&expected, value, sym) {
            let actual = self.type_of(value, sym);
            self.error_with_help(
                codes::TYPE_MISMATCH,
                expr_span(value),
                format!(
                    "cannot initialize `{owner}.{field}` ({}) with {}",
                    self.ty_display(&expected),
                    self.ty_display(&actual)
                ),
                format!(
                    "convert the value explicitly to {}",
                    self.ty_display(&expected)
                ),
            );
        }
    }

    /// The inclusive labels of a packed vector or data-array declaration:
    /// `unsigned[15..8]` is `(8, 15)`, while `unsigned[8]` and
    /// `unsigned[8][4]` are `(0, 7)` and `(0, 3)`. Instance arrays and
    /// parametric bounds return `None`.
    pub(super) fn declared_index_bounds(&self, decl_ty: &Type) -> Option<(i64, i64)> {
        let resolved = self.resolve_alias_type(decl_ty)?;
        let Type::Indexed {
            index: Some(ix), ..
        } = &resolved
        else {
            return None;
        };
        match self.ast_ty(&resolved) {
            Ty::Array { elem, .. } if !self.is_entity_ty(&elem) => {}
            _ => return None,
        }
        match ix.as_ref() {
            Expr::Range { lo, hi, .. } => {
                let (a, b) = (signed_lit(lo)?, signed_lit(hi)?);
                Some((a.min(b), a.max(b)))
            }
            other => {
                let n = Self::const_literal(other)?;
                (n > 0).then_some((0, n - 1))
            }
        }
    }

    /// `y = 50` where `y: integer<0..10>`. The initializer form was checked,
    /// the assignment form was not — and the value wraps to the storage width
    /// (50 -> 2), so the runtime range assert saw an in-range value and the
    /// violation vanished.
    pub(super) fn check_assign_range(
        &mut self,
        target: &Expr,
        value: &Expr,
        ranged: &HashMap<String, (i64, i64)>,
    ) {
        let Some(name) = target_root_name(target) else {
            return;
        };
        let Some(&(lo, hi)) = ranged.get(&name) else {
            return;
        };
        let Some(v) = Self::const_literal(value) else {
            return;
        };
        if v < lo || v > hi {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(value),
                format!("value {v} is outside the range {lo}..{hi}"),
            );
        }
    }

    /// Check a value against a ranged numeric's declared domain, so an
    /// out-of-range constant is rejected at compile time.
    pub(super) fn check_value_range(&mut self, decl_ty: &Type, value: &Expr) {
        let Some(resolved) = self.resolve_alias_type(decl_ty) else {
            return;
        };
        let t = &resolved;
        let Type::Generic { base, args, .. } = t else {
            return;
        };
        let Type::Path(p) = base.as_ref() else { return };
        if p.segments.last().map(|s| s.text.as_str()) != Some("integer") {
            return;
        }
        let [GenericArg::Positional(Expr::Range { lo, hi, .. })] = args.as_slice() else {
            return;
        };
        let (Some(a), Some(b)) = (signed_lit(lo), signed_lit(hi)) else {
            return;
        };
        let (min, max) = (a.min(b), a.max(b));
        if let Some(v) = signed_lit(value) {
            if v < min || v > max {
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(value),
                    format!("value {v} is outside the range {min}..{max}"),
                );
            }
        }
    }
}
