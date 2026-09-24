//! Operators and comparisons: operand domains, overload acceptance, and
//! assignability.

use super::*;

impl<'a> Checker<'a> {
    /// `sig == 600` with `sig: unsigned[8]`: the comparison masks both sides to the
    /// operand width, so the literal silently becomes 88 and the guard fires on
    /// the wrong value. The masking is right for a *wrapped* expression
    /// (`q == 0 - 3` really is 253), so reject the un-representable literal
    /// instead (spec 3.17/3.26).
    pub(super) fn check_comparison_fit(
        &mut self,
        lhs: &Expr,
        rhs: &Expr,
        sym: &HashMap<String, Ty>,
    ) {
        for (operand, lit) in [(lhs, rhs), (rhs, lhs)] {
            // Only flag a bare constant against a sized runtime operand.
            if Self::const_literal(operand).is_some() {
                continue;
            }
            let Some(v) = Self::const_literal(lit) else {
                continue;
            };
            if let Ty::Array {
                len,
                family: Some(_),
                ..
            } = self.type_of(operand, sym)
            {
                self.check_fits_width(v, len, expr_span(lit));
            }
        }
    }

    /// Whether two expressions can inhabit one value domain without an
    /// explicit conversion. This is symmetric because contextual literals may
    /// take the other side's type, and integer/real choose the real domain.
    pub(super) fn value_expressions_compatible(
        &self,
        left: &Expr,
        right: &Expr,
        sym: &HashMap<String, Ty>,
    ) -> bool {
        let left_ty = self.type_of(left, sym);
        let right_ty = self.type_of(right, sym);
        if matches!(left_ty, Ty::Error) || matches!(right_ty, Ty::Error) {
            return true;
        }
        if compatible(&left_ty, &right_ty) || compatible(&right_ty, &left_ty) {
            return true;
        }
        if matches!(
            (&left_ty, &right_ty),
            (Ty::Integer, Ty::Real) | (Ty::Real, Ty::Integer)
        ) {
            return true;
        }
        self.assignable(&right_ty, left, sym) || self.assignable(&left_ty, right, sym)
    }

    /// The first of `values` that carries a domain of its own, which the
    /// contextual literals beside it then adopt.
    pub(super) fn value_domain_anchor<'b>(
        &self,
        values: &[&'b Expr],
        sym: &HashMap<String, Ty>,
    ) -> Option<&'b Expr> {
        values
            .iter()
            .copied()
            .find(|value| {
                !Self::is_contextual_literal(value)
                    && !matches!(self.type_of(value, sym), Ty::Error)
            })
            .or_else(|| values.first().copied())
    }

    /// Whether every value shares one domain, so a comparison or match over them
    /// is meaningful.
    pub(super) fn values_share_domain(&self, values: &[&Expr], sym: &HashMap<String, Ty>) -> bool {
        let Some(anchor) = self.value_domain_anchor(values, sym) else {
            return false;
        };
        values
            .iter()
            .all(|value| self.value_expressions_compatible(anchor, value, sym))
    }

    /// Infer the value domain selected by conditional arms rather than simply
    /// copying the first arm. In particular, integer/real joins are real and
    /// contextual literals take the concrete arm's type.
    pub(super) fn joined_value_type(&self, values: &[&Expr], sym: &HashMap<String, Ty>) -> Ty {
        if !self.values_share_domain(values, sym) {
            return Ty::Error;
        }
        let types: Vec<Ty> = values
            .iter()
            .map(|value| self.type_of(value, sym))
            .collect();
        if types.iter().any(|ty| matches!(ty, Ty::Real))
            && types.iter().all(|ty| matches!(ty, Ty::Integer | Ty::Real))
        {
            return Ty::Real;
        }
        self.value_domain_anchor(values, sym)
            .map(|value| self.type_of(value, sym))
            .unwrap_or(Ty::Error)
    }

    /// Comparisons may inspect two differently constrained arrays without
    /// assigning either into the other. Their element domains must agree, but
    /// unequal lengths are meaningful (`"abc" == "abcd"` is simply false)
    /// and must not be treated as an assignment-width error.
    pub(super) fn comparison_domains_compatible(
        &self,
        left: &Expr,
        right: &Expr,
        sym: &HashMap<String, Ty>,
    ) -> bool {
        if self.value_expressions_compatible(left, right, sym) {
            return true;
        }
        match (self.type_of(left, sym), self.type_of(right, sym)) {
            (Ty::Array { elem: left, .. }, Ty::Array { elem: right, .. }) => {
                compatible(&left, &right) || compatible(&right, &left)
            }
            _ => false,
        }
    }

    /// Find the output of the operator overload selected by these operands.
    /// `Self` is the impl owner, not a wildcard: it only matches the owner's
    /// type. A contextual literal may also take that type at the call site.
    pub(super) fn operator_output(
        &self,
        symbol: &str,
        left: &Ty,
        right: &Ty,
        coerces_to_owner: bool,
    ) -> Option<Option<String>> {
        let owner = self.ty_head(left)?;
        let input = self.ty_head(right)?;
        self.operator_sigs
            .get(&(symbol.to_string(), owner.clone()))
            .and_then(|signatures| {
                signatures.iter().find(|(declared, _)| {
                    declared.as_deref() == Some(input.as_str())
                        || (declared.as_deref() == Some("Self") && input == owner)
                        || (coerces_to_owner
                            && (declared.as_deref() == Some("Self")
                                || declared.as_deref() == Some(owner.as_str())))
                })
            })
            .map(|(_, output)| output.clone())
            .or_else(|| {
                // A nominal array newtype inherits a blanket array
                // operator from its element. Keep this fallback separate from
                // ordinary impl ownership: an unrelated overload for the same
                // owner must not make every right-hand type match.
                let same_domain = input == owner || coerces_to_owner;
                let element = self.array_elements.get(&owner)?;
                let requirement = self.blanket_array_impls.get(symbol)?;
                (same_domain
                    && self
                        .trait_impls
                        .get(requirement)
                        .is_some_and(|types| types.contains(element)))
                .then_some(Some(owner))
            })
    }

    /// Whether an operator impl accepts these operand types.
    pub(super) fn operator_accepts(
        &self,
        symbol: &str,
        left: &Ty,
        right: &Ty,
        coerces_to_owner: bool,
    ) -> bool {
        self.operator_output(symbol, left, right, coerces_to_owner)
            .is_some()
    }

    /// As [`Self::operator_accepts`], but allowing the right operand to be a
    /// contextual literal that adopts the left's type.
    pub(super) fn operator_accepts_expr(
        &self,
        symbol: &str,
        left: &Ty,
        right: &Ty,
        rhs: &Expr,
        sym: &HashMap<String, Ty>,
    ) -> bool {
        let contextual_rhs = Self::is_contextual_literal(rhs) && self.assignable(left, rhs, sym);
        let numeric_kernel_coercion = matches!(
            (left, right),
            (
                Ty::Array {
                    family: Some(_),
                    ..
                },
                Ty::Integer
            )
        );
        self.operator_accepts(
            symbol,
            left,
            right,
            contextual_rhs || numeric_kernel_coercion,
        )
    }

    /// Check comparison operands, keeping the focused character and enum
    /// diagnostics rather than a generic mismatch.
    pub(super) fn check_comparison_operands(
        &mut self,
        op: &BinOp,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
        sym: &HashMap<String, Ty>,
    ) -> bool {
        // Keep the existing focused character/numeric error and enum/integer
        // suspicious-comparison warning as the sole diagnostics for those
        // deliberately recognized source forms.
        let char_numeric = [(lhs, rhs), (rhs, lhs)].iter().any(|(literal, other)| {
            matches!(literal, Expr::CharLit { .. })
                && matches!(
                    self.type_of(other, sym),
                    Ty::Array {
                        family: Some(_),
                        ..
                    } | Ty::Integer
                        | Ty::Real
                )
        });
        let enum_integer = [(lhs, rhs), (rhs, lhs)].iter().any(|(literal, other)| {
            matches!(literal, Expr::Int { text, .. } if !text.contains('.'))
                && self.enum_operand_name(&self.type_of(other, sym)).is_some()
        });
        // Equality against an enum discriminant is deliberately a lint (the
        // warning below points users to a variant). Ordering has no analogous
        // intrinsic meaning and must continue through normal `<=>` checking.
        let suspicious_enum_equality = enum_integer && matches!(op, BinOp::Eq | BinOp::Ne);
        if char_numeric || suspicious_enum_equality {
            return true;
        }
        if self.comparison_domains_compatible(lhs, rhs, sym) {
            return false;
        }
        let left = self.type_of(lhs, sym);
        let right = self.type_of(rhs, sym);
        if self.operator_accepts_expr("<=>", &left, &right, rhs, sym) {
            return false;
        }
        self.error_with_help(
            codes::TYPE_MISMATCH,
            span,
            format!(
                "cannot compare {} and {} with `{}`",
                self.ty_display(&left),
                self.ty_display(&right),
                crate::syntax::pretty::bin_op(op)
            ),
            "convert one operand to the other's type, or implement `Operator<\"<=>\", Input, Ordering>`"
                .to_string(),
        );
        true
    }

    /// Check the operands of a built-in arithmetic or shift operator.
    pub(super) fn check_intrinsic_binary_operands(
        &mut self,
        op: &BinOp,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
        sym: &HashMap<String, Ty>,
    ) {
        if !matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Shl | BinOp::Shr
        ) {
            return;
        }
        let left = self.type_of(lhs, sym);
        let right = self.type_of(rhs, sym);
        if matches!(left, Ty::Error) || matches!(right, Ty::Error) {
            return;
        }
        let symbol = crate::syntax::pretty::bin_op(op);
        if self.operator_accepts_expr(symbol, &left, &right, rhs, sym) {
            return;
        }
        // Nominal left operands are diagnosed by the trait-specific check
        // below, which can offer the exact impl spelling without duplicating
        // this intrinsic-domain diagnostic.
        if self.named_operand_name(lhs, sym).is_some() {
            return;
        }
        let packed = |ty: &Ty| {
            matches!(
                ty,
                Ty::Array {
                    family: Some(_),
                    ..
                }
            )
        };
        let valid = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                matches!(
                    (&left, &right),
                    (Ty::Integer, Ty::Integer)
                        | (Ty::Integer, Ty::Real)
                        | (Ty::Real, Ty::Integer)
                        | (Ty::Real, Ty::Real)
                ) || (matches!(left, Ty::Integer) && packed(&right))
                    || (packed(&left) && matches!(right, Ty::Integer))
            }
            BinOp::Shl | BinOp::Shr => {
                matches!((&left, &right), (Ty::Integer, Ty::Integer))
                    || (matches!(left, Ty::Integer) && packed(&right))
                    || (packed(&left) && matches!(right, Ty::Integer))
            }
            _ => true,
        };
        if !valid {
            self.error_with_help(
                codes::TYPE_MISMATCH,
                span,
                format!(
                    "cannot apply `{symbol}` to {} and {}",
                    self.ty_display(&left),
                    self.ty_display(&right)
                ),
                format!(
                    "use numeric operands, convert explicitly, or implement `Operator<\"{symbol}\", Input, Output>`"
                ),
            );
        }
    }

    /// Whether `value` may be assigned to `lhs`. Widths are strict, but a
    /// contextual literal adopts the target's type.
    pub(super) fn assignable(&self, lhs: &Ty, value: &Expr, sym: &HashMap<String, Ty>) -> bool {
        match value {
            // A decimal literal is already `real`; narrowing it to integer or
            // packed bits requires an explicit conversion.
            Expr::Int { text, .. } if text.contains('.') => {
                matches!(lhs, Ty::Real | Ty::Error)
            }
            // An integer literal also initialises `real` (`.re = 10` is 10.0)
            // and is contextual for packed numeric families.
            Expr::Int { .. } => {
                matches!(
                    lhs,
                    Ty::Array {
                        family: Some(_),
                        ..
                    } | Ty::Integer
                        | Ty::Real
                        | Ty::Error
                )
            }
            Expr::CharLit { ch, .. } => {
                // A character literal reads through its context type (spec:
                // type kernel): builtin scalars, `Char`, or a user enum with
                // a matching character variant (e.g. ULogic's 'Z').
                if let Ty::Named(id) = lhs {
                    return self.enum_has_char_variant(*id, *ch);
                }
                matches!(lhs, Ty::Char | Ty::Error)
            }
            // An if-expression is assignable if both branches are — so char
            // literals in the branches read through the target type
            // (`b: Bit = if c { '1' } else { '0' }`).
            Expr::IfExpr { then, els, .. } => {
                // The expression walk owns an incompatible-branch error. Once
                // it has done so, do not blame the enclosing assignment too.
                !self.value_expressions_compatible(then, els, sym)
                    || (self.assignable(lhs, then, sym) && self.assignable(lhs, els, sym))
            }
            // A match expression has the assignment context of its consumer
            // on every arm, just like an if-expression has it on both
            // branches. Looking only at `type_of` used the first arm and let a
            // later incompatible value be reinterpreted silently.
            Expr::Match { arms, .. } => {
                let values: Vec<&Expr> = arms.iter().filter_map(MatchArm::value_expr).collect();
                let common = self.values_share_domain(&values, sym);
                !values.is_empty()
                    && (!common || values.iter().all(|value| self.assignable(lhs, value, sym)))
            }
            // `[a, b, c]` fills an array target: length must match and every
            // element must be assignable to the element type (element literals
            // read through it, as in an initialiser).
            Expr::Array { elems, .. } => match lhs {
                Ty::Array {
                    elem,
                    len,
                    family: None,
                } => {
                    elems.len() as u32 == *len
                        && elems.iter().all(|e| self.assignable(elem, e, sym))
                }
                Ty::Error => true,
                _ => false,
            },
            // A name-less positional struct literal lexes as concatenation.
            // It is structurally assignable to a named struct when its arity
            // matches; field-value checks are contextualized during lowering.
            Expr::Concat { parts, .. } => match lhs {
                Ty::Named(id) => self.struct_field_count(*id) == Some(parts.len()),
                Ty::Error => true,
                _ => compatible(lhs, &self.type_of(value, sym)),
            },
            // A string is a sequence of characters: assigned to a `Logic`-vector
            // it fills each element with the matching `std_ulogic` (like `b"…"`),
            // and assigned to an array of a char-enum each character is a variant
            // — a string of logic values *is* a logic array, no prefix needed.
            Expr::StrLit { text, .. } => {
                let n = text.chars().count() as u32;
                match lhs {
                    Ty::Array {
                        len,
                        family: Some(_),
                        ..
                    } => (*len == 0 || n == *len) && text.chars().all(|c| "01ZXUWLH-".contains(c)),
                    // A char-enum array (`Color[3] = "rgb"`): each char a variant.
                    Ty::Array {
                        elem,
                        len,
                        family: None,
                    } if matches!(elem.as_ref(), Ty::Named(_)) => {
                        let Ty::Named(id) = elem.as_ref() else {
                            unreachable!()
                        };
                        (*len == 0 || n == *len)
                            && text.chars().all(|c| self.enum_has_char_variant(*id, c))
                    }
                    // `Char[]` (a `string`) and everything else keep the existing
                    // structural check.
                    _ => compatible(lhs, &self.type_of(value, sym)),
                }
            }
            // Kernel integers widen exactly into the real domain. This is the
            // same promotion used by mixed real arithmetic and by std's
            // `Complex::from(integer)`; the reverse direction remains an
            // explicit `integer(value)` conversion because it truncates.
            _ if matches!(lhs, Ty::Real) && matches!(self.type_of(value, sym), Ty::Integer) => true,
            _ => compatible(lhs, &self.type_of(value, sym)),
        }
    }
}
