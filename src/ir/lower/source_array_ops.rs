//! Element operators consume canonical aggregate operands in written order.

use super::source_operators::OperatorOperand;
use super::*;

#[derive(Clone, Copy)]
enum ArrayOperator<'a> {
    Binary(&'a ast::BinOp),
    Unary(ast::UnOp),
}

impl Lowering<'_> {
    pub(super) fn lower_source_array_operator(
        &self,
        expression: &ast::Expr,
        env: &HashMap<String, Val>,
        target: &SourceLayout,
    ) -> Val {
        let (operator, lhs, rhs) = match expression {
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                (ArrayOperator::Binary(op), lhs.as_ref(), Some(rhs.as_ref()))
            }
            ast::Expr::Unary { op, rhs, .. } => (ArrayOperator::Unary(*op), rhs.as_ref(), None),
            _ => return Val::Scalar(Expr::Unknown),
        };
        let lhs_layout = self
            .source_operand_layout(lhs, env)
            .unwrap_or_else(|| target.clone());
        let lhs_value = self.lower_shaped_source(lhs, env, &lhs_layout);
        let rhs = rhs.map(|rhs| {
            let layout = self
                .source_operand_layout(rhs, env)
                .unwrap_or_else(|| target.clone());
            let value = self.lower_shaped_source(rhs, env, &layout);
            (value, layout)
        });
        self.source_array_operator_value(
            operator,
            lhs_value,
            &lhs_layout,
            rhs,
            target,
            ast::expr_span(expression),
        )
        .unwrap_or(Val::Scalar(Expr::Unknown))
    }

    fn source_array_operator_value(
        &self,
        operator: ArrayOperator<'_>,
        lhs: Val,
        lhs_layout: &SourceLayout,
        rhs: Option<(Val, SourceLayout)>,
        target: &SourceLayout,
        span: crate::diag::Span,
    ) -> Option<Val> {
        if let LayoutKind::Array {
            range: Some(range),
            element,
        } = &target.kind
        {
            let LayoutKind::Array {
                range: Some(lhs_range),
                element: lhs_element,
            } = &lhs_layout.kind
            else {
                return None;
            };
            if range.len()? != lhs_range.len()? {
                return None;
            }
            let lhs = array_elements(lhs, *lhs_range)?;
            let rhs = match rhs {
                Some((value, layout)) => {
                    let LayoutKind::Array {
                        range: Some(rhs_range),
                        element: rhs_element,
                    } = layout.kind
                    else {
                        return None;
                    };
                    if range.len()? != rhs_range.len()? {
                        return None;
                    }
                    Some((array_elements(value, rhs_range)?, *rhs_element))
                }
                None => None,
            };
            let mut rhs_values = rhs.as_ref().map(|(values, _)| values.iter());
            let mut fields = Vec::new();
            for (label, lhs) in loop_range(range.left, range.right).into_iter().zip(lhs) {
                let rhs = match (&rhs, &mut rhs_values) {
                    (Some((_, layout)), Some(values)) => {
                        Some((values.next()?.clone(), layout.clone()))
                    }
                    _ => None,
                };
                let value = self.source_array_operator_value(
                    operator,
                    lhs,
                    lhs_element,
                    rhs,
                    element,
                    span,
                )?;
                Self::prefix_block_value(&format!("[{label}]"), value, &mut fields);
            }
            return Some(self.bind_value_layout(Val::Fields(fields), target.clone(), span));
        }
        let operand = |value, layout: &SourceLayout| OperatorOperand {
            value,
            family: Self::source_layout_family(layout),
            width: layout
                .bit_width()
                .and_then(|width| u32::try_from(width).ok())
                .unwrap_or(1),
            range: layout.index_range().map(|range| (range.left, range.right)),
            layout: Some(layout.clone()),
        };
        let lhs = operand(lhs, lhs_layout);
        let value = match operator {
            ArrayOperator::Binary(op) => {
                let (rhs, layout) = rhs?;
                let rhs = operand(rhs, &layout);
                let spelling = crate::syntax::pretty::bin_op(op);
                if let Some(value) = self.native_bound_vector_logical(spelling, &lhs, &rhs) {
                    Val::Scalar(value)
                } else if let Some(function) = lhs.family.as_deref().and_then(|family| {
                    self.source_binary_operator(spelling, family, rhs.family.as_deref(), span)
                }) {
                    self.inline_source_operator(function, lhs, Some(rhs))?
                } else {
                    let integer = lhs.family.as_deref() == Some("integer")
                        || rhs.family.as_deref() == Some("integer");
                    let (Val::Scalar(lhs), Val::Scalar(rhs)) = (lhs.value, rhs.value) else {
                        return None;
                    };
                    Val::Scalar(self.make_binary(op.clone(), lhs, rhs, integer, integer))
                }
            }
            ArrayOperator::Unary(op) => {
                let spelling = match op {
                    ast::UnOp::Neg => "-",
                    ast::UnOp::Not => "not",
                };
                if let Some(function) = lhs
                    .family
                    .as_deref()
                    .and_then(|family| self.source_unary_operator(spelling, family))
                {
                    self.inline_source_operator(function, lhs, None)?
                } else {
                    let Val::Scalar(value) = lhs.value else {
                        return None;
                    };
                    let value = if op == ast::UnOp::Not
                        && matches!(lhs_layout.kind, LayoutKind::Packed { .. })
                    {
                        let mut ones = vec![0u64; (lhs.width as usize).max(1).div_ceil(64)];
                        for bit in 0..lhs.width {
                            ones[bit as usize / 64] |= 1u64 << (bit % 64);
                        }
                        Expr::Binary {
                            op: BinOp::Xor,
                            lhs: Box::new(value),
                            rhs: Box::new(words_const(ones)),
                        }
                    } else {
                        self.make_unary(op, value)
                    };
                    Val::Scalar(value)
                }
            }
        };
        Some(self.bind_value_layout(value, target.clone(), span))
    }
}

/// Split a flattened aggregate once. Labels are source coordinates; pairing
/// across operands happens by position, never by matching numeric labels.
fn array_elements(value: Val, range: LayoutRange) -> Option<Vec<Val>> {
    let Val::Fields(fields) = value else {
        return None;
    };
    let mut groups: HashMap<i64, Vec<(String, Expr)>> = HashMap::new();
    for (name, value) in fields {
        let end = name.find(']')?;
        let label = name
            .strip_prefix('[')?
            .get(..end - 1)?
            .parse::<i64>()
            .ok()?;
        let suffix = name
            .get(end + 1..)?
            .strip_prefix('.')
            .unwrap_or(&name[end + 1..]);
        groups
            .entry(label)
            .or_default()
            .push((suffix.to_owned(), value));
    }
    let mut values = Vec::new();
    for label in loop_range(range.left, range.right) {
        let mut fields = groups.remove(&label)?;
        values.push(if fields.len() == 1 && fields[0].0.is_empty() {
            Val::Scalar(fields.pop()?.1)
        } else {
            if fields.iter().any(|(name, _)| name.is_empty()) {
                return None;
            }
            Val::Fields(fields)
        });
    }
    groups.is_empty().then_some(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_negative_descending_nested_elements_without_rebuilding_values() {
        let value = Val::Fields(vec![
            (
                "[-2][4].a".to_owned(),
                Expr::Canonical {
                    value: ProcessValueId(7),
                    reads: std::sync::Arc::from([]),
                },
            ),
            (
                "[-1][4].a".to_owned(),
                Expr::Canonical {
                    value: ProcessValueId(3),
                    reads: std::sync::Arc::from([]),
                },
            ),
        ]);
        let values = array_elements(
            value,
            LayoutRange {
                left: -1,
                right: -2,
            },
        )
        .unwrap();
        for (value, expected) in values.into_iter().zip([3, 7]) {
            let Val::Fields(fields) = value else {
                panic!("nested element expected");
            };
            assert_eq!(fields[0].0, "[4].a");
            assert!(
                matches!(fields[0].1, Expr::Canonical { value: ProcessValueId(id), .. } if id == expected)
            );
        }
    }

    #[test]
    fn split_rejects_missing_extra_or_mixed_leaves() {
        let range = LayoutRange { left: 0, right: 1 };
        assert!(
            array_elements(Val::Fields(vec![("[0]".to_owned(), Expr::Const(1))]), range).is_none()
        );
        assert!(array_elements(
            Val::Fields(vec![
                ("[0]".to_owned(), Expr::Const(1)),
                ("[1]".to_owned(), Expr::Const(2)),
                ("[2]".to_owned(), Expr::Const(3))
            ]),
            range
        )
        .is_none());
        assert!(array_elements(
            Val::Fields(vec![
                ("[0]".to_owned(), Expr::Const(1)),
                ("[0].a".to_owned(), Expr::Const(2)),
                ("[1]".to_owned(), Expr::Const(3))
            ]),
            range
        )
        .is_none());
    }
}
