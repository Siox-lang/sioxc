//! Indexing: operand types, constant bounds, width fit, and custom `Index`
//! implementations.

use super::*;

impl<'a> Checker<'a> {
    /// A constant that cannot be represented in `width` bits is an error
    /// wherever it meets a sized vector — a conversion or a comparison. No
    /// signedness: a literal fits an N-bit vector if it lands in the union of
    /// the unsigned (`0..2^N`) and signed (`-2^(N-1)..`) ranges.
    pub(super) fn check_fits_width(&mut self, v: i64, width: u32, span: Span) {
        if !(1..=64).contains(&width) {
            return;
        }
        // Both bounds are computed in `i64` and so saturate near its top:
        // `1i64 << 63` is already `i64::MIN`, which makes `- 1` overflow at
        // width 63 and the negation overflow at width 64. Those widths are
        // exactly where a bit vector meets a plain integer literal, so this is
        // reachable from any `a == 5` on a 63- or 64-bit signal.
        let hi = if width >= 63 {
            i64::MAX
        } else {
            (1i64 << width) - 1
        };
        let lo = if width >= 64 {
            i64::MIN
        } else {
            -(1i64 << (width - 1))
        };
        if v < lo || v > hi {
            self.error(
                codes::TYPE_MISMATCH,
                span,
                format!("`{v}` does not fit in a {width}-bit vector"),
            );
        }
    }

    /// Const-fold a literal arithmetic expression (`3`, `0 - 3`). `None` once
    /// any operand is a runtime value.
    pub(super) fn const_literal(e: &Expr) -> Option<i64> {
        match e {
            Expr::Binary { op, lhs, rhs, .. } => {
                let (a, b) = (Self::const_literal(lhs)?, Self::const_literal(rhs)?);
                Some(match op {
                    BinOp::Add => a.checked_add(b)?,
                    BinOp::Sub => a.checked_sub(b)?,
                    BinOp::Mul => a.checked_mul(b)?,
                    BinOp::Div if b != 0 => a / b,
                    _ => return None,
                })
            }
            _ => signed_lit(e),
        }
    }

    /// Whether a type may be used as an index. `Ty::Error` is admitted so one
    /// bad type does not cascade.
    pub(super) fn is_integer_like_index(ty: &Ty) -> bool {
        matches!(ty, Ty::Integer | Ty::Error)
            || matches!(
                ty,
                Ty::Array {
                    family: Some(_),
                    ..
                }
            )
    }

    /// Check an index value's type, naming `description` in any diagnostic.
    pub(super) fn check_index_value(
        &mut self,
        value: &Expr,
        sym: &HashMap<String, Ty>,
        description: &str,
    ) {
        let ty = self.type_of(value, sym);
        if Self::is_integer_like_index(&ty) {
            return;
        }
        self.error_with_help(
            codes::TYPE_MISMATCH,
            expr_span(value),
            format!(
                "{description} must be an integer or packed numeric value, found {}",
                self.ty_display(&ty)
            ),
            "convert it explicitly to `integer` or a packed numeric type".to_string(),
        );
    }

    /// Validate intrinsic array subscripts and every range endpoint. A custom
    /// scalar `Index<I, _>` may choose another input type, but `Range` itself
    /// always stores integer left/right bounds.
    pub(super) fn check_index_operand(
        &mut self,
        base: &Expr,
        index: &Expr,
        sym: &HashMap<String, Ty>,
    ) {
        match index {
            Expr::Range { lo, hi, .. } => {
                self.check_index_value(lo, sym, "range bound");
                self.check_index_value(hi, sym, "range bound");
            }
            Expr::PartialRange { lo, hi, .. } => {
                if let Some(lo) = lo {
                    self.check_index_value(lo, sym, "range bound");
                }
                if let Some(hi) = hi {
                    self.check_index_value(hi, sym, "range bound");
                }
            }
            _ if matches!(self.type_of(base, sym), Ty::Array { .. }) => {
                self.check_index_value(index, sym, "array index");
            }
            _ => {}
        }
    }

    /// A constant bit index or slice outside a packed vector's width has no
    /// hardware meaning — it lowered to `Unknown` and surfaced much later as a
    /// generic "no engine can run this design". Both packed vectors and data
    /// arrays use their declared labels; a width/count spelling falls back to
    /// `0..len-1`.
    pub(super) fn check_index_bounds(
        &mut self,
        base: &Expr,
        index: &Expr,
        sym: &HashMap<String, Ty>,
    ) {
        // What we can bound-check, and how to name it. A packed vector or data
        // array may carry nonzero declared labels. An instance array (`let s:
        // Sub[4]`) is always declared with a plain count and remains 0-based.
        let (lo, hi, noun) = match self.type_of(base, sym) {
            Ty::Array {
                len,
                family: Some(_),
                ..
            } => declared_bounds_of(base, &self.array_bounds)
                .map(|(lo, hi)| (lo, hi, "bit"))
                .unwrap_or((0, len as i64 - 1, "bit")),
            Ty::Array {
                elem,
                len,
                family: None,
            } if self.is_entity_ty(&elem) => (0, len as i64 - 1, "instance"),
            // A data array's bounds come from its declaration, not its
            // length: `Logic[15..8]` is indexed `8..15`.
            Ty::Array { family: None, .. } => match declared_bounds_of(base, &self.array_bounds) {
                Some((lo, hi)) => (lo, hi, "element"),
                None => return,
            },
            _ => return,
        };
        if hi < lo {
            return; // parametric: not known yet
        }
        let len = hi - lo + 1;
        let mut check = |v: i64, e: &Expr| {
            if v < lo || v > hi {
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(e),
                    match noun {
                        "bit" => {
                            format!("bit {v} is outside `{lo}..{hi}` of this {len}-bit vector")
                        }
                        "instance" => format!(
                            "instance {v} is outside `{lo}..{hi}` of this {len}-instance array"
                        ),
                        _ => {
                            format!("index {v} is outside `{lo}..{hi}` of this {len}-element array")
                        }
                    },
                );
            }
        };
        match index {
            Expr::Range { lo, hi, .. } => {
                if let Some(v) = Self::const_literal(lo) {
                    check(v, lo);
                }
                if let Some(v) = Self::const_literal(hi) {
                    check(v, hi);
                }
            }
            Expr::PartialRange { lo, hi, .. } => {
                if let Some(lo) = lo {
                    if let Some(v) = Self::const_literal(lo) {
                        check(v, lo);
                    }
                }
                if let Some(hi) = hi {
                    if let Some(v) = Self::const_literal(hi) {
                        check(v, hi);
                    }
                }
            }
            _ => {
                if let Some(v) = Self::const_literal(index) {
                    check(v, index);
                }
            }
        }
    }

    /// Check indexing of a type that supplies its own index contract rather than
    /// being a built-in array.
    pub(super) fn check_custom_index(
        &mut self,
        base: &Expr,
        index: &Expr,
        sym: &HashMap<String, Ty>,
    ) {
        let base_ty = self.type_of(base, sym);
        if matches!(base_ty, Ty::Array { .. }) {
            return;
        }
        let Some(owner) = self.type_kind_name(&base_ty) else {
            return;
        };
        if matches!(index, Expr::PartialRange { .. }) {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(index),
                format!(
                    "partial range indexing on `{owner}` has no declared bounds; use an explicit `left..right` range"
                ),
            );
            return;
        }
        let input = if matches!(index, Expr::Range { .. } | Expr::PartialRange { .. }) {
            self.type_kind_name(&self.ty_from_head("Range"))
        } else {
            self.type_kind_name(&self.type_of(index, sym))
        };
        let found = self
            .index_sigs
            .get(&("Index".to_string(), owner.clone()))
            .is_some_and(|sigs| {
                sigs.iter()
                    .any(|(i, _)| Self::index_contract_type_matches(i, input.as_deref(), &owner))
            });
        if !found {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(index),
                format!(
                    "indexing `{owner}` needs `impl Index<{}, Output> for {owner}`",
                    input.as_deref().unwrap_or("_"),
                ),
            );
        }
    }
}
