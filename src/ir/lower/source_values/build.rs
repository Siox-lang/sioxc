//! Source selection constructors: keep operands in the canonical arena.

use super::*;

impl SourceValues {
    pub(in crate::ir::lower) fn signal(
        &mut self,
        signal: SignalId,
        state: ProcessSignalState,
        span: crate::diag::Span,
    ) -> Expr {
        let value = self.push_node(
            ProcessValue {
                span,
                ty: None,
                bit_width: None,
                kind: ProcessValueKind::Signal {
                    signals: vec![signal],
                    state,
                },
            },
            None,
        );
        self.reference(value)
    }

    pub(in crate::ir::lower) fn slice(
        &mut self,
        base: &Expr,
        high: u32,
        low: u32,
        span: crate::diag::Span,
    ) -> Expr {
        let base = self.append(base, span, None);
        let value = self.push_node(
            ProcessValue {
                span,
                ty: None,
                bit_width: None,
                kind: ProcessValueKind::BitSlice { base, high, low },
            },
            None,
        );
        self.reference(value)
    }

    pub(in crate::ir::lower) fn checked_index(
        &mut self,
        index: &Expr,
        valid: &Expr,
        left: i64,
        right: i64,
        span: crate::diag::Span,
    ) -> Expr {
        let index = self.append(index, span, None);
        let valid = self.append(valid, span, None);
        let value = self.push_node(
            ProcessValue {
                span,
                ty: None,
                bit_width: None,
                kind: ProcessValueKind::CheckedIndex {
                    index,
                    valid,
                    left,
                    right,
                    span,
                },
            },
            None,
        );
        self.reference(value)
    }

    pub(in crate::ir::lower) fn select(
        &mut self,
        condition: &Expr,
        then_value: &Expr,
        else_value: &Expr,
        span: crate::diag::Span,
    ) -> Expr {
        let condition = self.append(condition, span, None);
        let then_value = self.append(then_value, span, None);
        let else_value = self.append(else_value, span, None);
        let value = self.push_node(
            ProcessValue {
                span,
                ty: None,
                bit_width: None,
                kind: ProcessValueKind::Select {
                    condition,
                    then_value,
                    else_value,
                },
            },
            None,
        );
        self.reference(value)
    }
}

impl Lowering<'_> {
    /// Select aggregate leaves by name, sharing the condition's single value.
    pub(in crate::ir::lower) fn source_select_value(
        &self,
        condition: Expr,
        then_value: Val,
        else_value: Val,
        span: crate::diag::Span,
    ) -> Val {
        match (then_value, else_value) {
            (Val::Scalar(then_value), Val::Scalar(else_value)) => {
                Val::Scalar(self.source_select(&condition, &then_value, &else_value, span))
            }
            (Val::Fields(then_fields), Val::Fields(else_fields)) => {
                let condition = {
                    let mut arena = self.source_values.borrow_mut();
                    let id = arena.append(&condition, span, None);
                    arena.reference(id)
                };
                Val::Fields(
                    then_fields
                        .into_iter()
                        .map(|(name, then_value)| {
                            let else_value = else_fields
                                .iter()
                                .find(|(field, _)| *field == name)
                                .map(|(_, value)| value.clone())
                                .unwrap_or(Expr::Unknown);
                            (
                                name,
                                self.source_select(&condition, &then_value, &else_value, span),
                            )
                        })
                        .collect(),
                )
            }
            _ => Val::Scalar(Expr::Unknown),
        }
    }

    pub(in crate::ir::lower) fn source_slice(
        &self,
        base: &Expr,
        high: u32,
        low: u32,
        span: crate::diag::Span,
    ) -> Expr {
        self.source_values.borrow_mut().slice(base, high, low, span)
    }

    pub(in crate::ir::lower) fn source_select(
        &self,
        condition: &Expr,
        then_value: &Expr,
        else_value: &Expr,
        span: crate::diag::Span,
    ) -> Expr {
        self.source_values
            .borrow_mut()
            .select(condition, then_value, else_value, span)
    }

    pub(in crate::ir::lower) fn source_binary(
        &self,
        operation: BinOp,
        left: &Expr,
        right: &Expr,
        span: crate::diag::Span,
    ) -> Expr {
        self.source_values
            .borrow_mut()
            .binary(operation, left, right, span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{FileId, Span};

    fn id(expression: &Expr) -> ProcessValueId {
        let Expr::Canonical { value, .. } = expression else {
            panic!("selection constructor must return a canonical root");
        };
        *value
    }

    #[test]
    fn real_coercion_constructs_shared_arena_nodes_at_source_anchors() {
        let span = Span::new(FileId(0), 20..29);
        let call_span = Span::new(FileId(0), 21..24);
        let mut sink = DiagnosticSink::new();
        let resolved = Resolved::default();
        let lowering = Lowering::new(&mut sink, &resolved);
        let call = {
            let mut arena = lowering.source_values.borrow_mut();
            let call = arena.append(
                &Expr::CCall {
                    name: "read_real".into(),
                    args: vec![],
                    f64_args: vec![],
                    integer_args: vec![],
                    f64_ret: true,
                    integer_ret: false,
                },
                call_span,
                Some(crate::types::Ty::Real),
            );
            arena.reference(call)
        };
        let integer = lowering.source_binary(BinOp::SAdd, &call, &Expr::Const(3), span);
        let original = id(&integer);
        let converted = lowering.coerce_real(integer.clone(), call_span);
        let second = lowering.coerce_real(integer, call_span);
        assert_eq!(id(&converted), id(&second), "coercion reuses its result");
        let arena = lowering.source_values.borrow();
        let root = &arena.ir.values[id(&converted).0 as usize];
        assert_eq!(root.span, span, "canonical source anchor wins over caller");
        let ProcessValueKind::Binary {
            operation: ProcessBinaryOp::FloatAdd,
            left,
            right,
        } = root.kind
        else {
            panic!("real coercion must construct a canonical float operation");
        };
        assert_eq!(left, id(&call));
        assert!(matches!(arena.ir.values[right.0 as usize].kind,
            ProcessValueKind::Number(ProcessNumber::Real(bits)) if bits == 3.0f64.to_bits()));
        assert!(matches!(
            arena.ir.values[original.0 as usize].kind,
            ProcessValueKind::Binary {
                operation: ProcessBinaryOp::SignedAdd,
                ..
            }
        ));
        assert_eq!(arena.ir.values[left.0 as usize].span, call_span);
        assert_eq!(
            arena.ir.values[left.0 as usize].ty,
            Some(crate::types::Ty::Real)
        );
        assert_eq!(
            arena
                .ir
                .values
                .iter()
                .filter(|node| matches!(node.kind, ProcessValueKind::ForeignCall { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn real_coercion_builds_negation_and_conditional_without_tree_roots() {
        let span = Span::new(FileId(0), 20..29);
        let mut sink = DiagnosticSink::new();
        let resolved = Resolved::default();
        let lowering = Lowering::new(&mut sink, &resolved);
        let negative = lowering.coerce_real(
            Expr::Unary {
                op: UnOp::Neg,
                rhs: Box::new(Expr::Const(3)),
            },
            span,
        );
        let selected = lowering.coerce_real(
            Expr::Select {
                cond: Box::new(Expr::Const(1)),
                then: Box::new(Expr::Const(4)),
                els: Box::new(Expr::Const(7)),
            },
            span,
        );
        let arena = lowering.source_values.borrow();
        assert!(matches!(
            arena.ir.values[id(&negative).0 as usize].kind,
            ProcessValueKind::Binary {
                operation: ProcessBinaryOp::FloatSub,
                ..
            }
        ));
        let ProcessValueKind::Select {
            then_value,
            else_value,
            ..
        } = arena.ir.values[id(&selected).0 as usize].kind
        else {
            panic!("coerced conditional must have a canonical root");
        };
        for (value, expected) in [(then_value, 4.0f64), (else_value, 7.0f64)] {
            assert_eq!(arena.ir.values[value.0 as usize].span, span);
            assert!(matches!(arena.ir.values[value.0 as usize].kind,
                ProcessValueKind::Number(ProcessNumber::Real(bits)) if bits == expected.to_bits()));
        }
    }

    #[test]
    fn canonical_scalar_bindings_keep_explicit_kernel_boundaries() {
        let span = Span::new(FileId(0), 20..29);
        let mut sink = DiagnosticSink::new();
        let resolved = Resolved::default();
        let lowering = Lowering::new(&mut sink, &resolved);
        for (operation, ty) in [
            (BinOp::Sub, crate::types::Ty::Integer),
            (BinOp::FAdd, crate::types::Ty::Real),
        ] {
            let expression = lowering.source_binary(
                operation,
                &Expr::Current(SignalId(0)),
                &Expr::Const(64),
                span,
            );
            let operand = id(&expression);
            let Val::Scalar(untyped) =
                lowering.bind_source_value(Val::Scalar(expression.clone()), span, None)
            else {
                panic!("scalar expected")
            };
            assert_eq!(id(&untyped), operand, "untyped aliases retain identity");
            let Val::Scalar(inferred) =
                lowering.bind_source_value(Val::Scalar(expression), span, Some(ty.clone()))
            else {
                panic!("scalar expected")
            };
            assert_eq!(
                id(&inferred),
                operand,
                "an inferred hint is not an explicit boundary"
            );
            let bound = lowering.bind_source_scalar(inferred, span, ty.clone());
            let values = lowering.source_values.borrow();
            let node = &values.ir.values[id(&bound).0 as usize];
            assert_eq!(node.kind, ProcessValueKind::RawResize { operand });
            assert_eq!(node.ty, Some(ty));
            assert_eq!(node.span, span);
            assert_eq!(
                values.ir.values[operand.0 as usize].ty, None,
                "binding must not mutate a shared operand's format"
            );
            let Expr::Canonical { reads, .. } = bound else {
                unreachable!()
            };
            assert_eq!(reads.as_ref(), &[SignalId(0)]);
        }
    }

    #[test]
    fn aggregate_selections_bind_raw_signal_conditions_once() {
        let span = Span::new(FileId(0), 20..29);
        for condition in [
            Expr::Current(SignalId(0)),
            Expr::Old(SignalId(0)),
            Expr::Event(SignalId(0)),
        ] {
            let mut sink = DiagnosticSink::new();
            let resolved = Resolved::default();
            let lowering = Lowering::new(&mut sink, &resolved);
            let Val::Fields(fields) = lowering.source_select_value(
                condition,
                Val::Fields(vec![
                    ("a".into(), Expr::Const(1)),
                    ("b".into(), Expr::Const(2)),
                ]),
                Val::Fields(vec![
                    ("b".into(), Expr::Const(3)),
                    ("a".into(), Expr::Const(4)),
                ]),
                span,
            ) else {
                panic!("aggregate expected")
            };
            let values = lowering.source_values.borrow();
            let conditions = fields
                .iter()
                .map(|(_, expression)| {
                    let ProcessValueKind::Select { condition, .. } =
                        values.ir.values[id(expression).0 as usize].kind
                    else {
                        panic!("select expected")
                    };
                    condition
                })
                .collect::<Vec<_>>();
            assert_eq!(conditions[0], conditions[1]);
            assert_eq!(
                values
                    .ir
                    .values
                    .iter()
                    .filter(|node| matches!(node.kind, ProcessValueKind::Signal { .. }))
                    .count(),
                1
            );
            assert_eq!(
                fields
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>(),
                ["a", "b"]
            );
        }
    }

    #[test]
    fn selections_share_operands_without_projecting_their_formats() {
        let operand_span = Span::new(FileId(0), 1..8);
        let index_span = Span::new(FileId(0), 20..29);
        let mut values = SourceValues::default();
        let current = values.signal(SignalId(0), ProcessSignalState::Current, operand_span);
        let old = values.signal(SignalId(1), ProcessSignalState::Old, operand_span);
        let offset = values.push_node(
            ProcessValue {
                span: operand_span,
                ty: Some(crate::types::Ty::Integer),
                bit_width: Some(64),
                kind: ProcessValueKind::ForeignCall {
                    name: "labs".into(),
                    arguments: vec![id(&old)],
                    float_arguments: vec![false],
                    integer_arguments: vec![true],
                    float_result: false,
                    integer_result: true,
                },
            },
            None,
        );
        let layout = SourceLayout {
            span: operand_span,
            kind: LayoutKind::Packed {
                width: 128,
                family: "UserWord".into(),
                element_enum: None,
                range: Some(LayoutRange {
                    left: 127,
                    right: 0,
                }),
            },
        };
        // Index has no private digital-expression projection. The constructors
        // must retain this operand's handle and complete format anyway.
        let base = values.push_node(
            ProcessValue {
                span: operand_span,
                ty: None,
                bit_width: Some(128),
                kind: ProcessValueKind::Index {
                    base: id(&current),
                    index: offset,
                },
            },
            Some(layout.clone()),
        );
        let before = values.ir.values.len();
        let selected = values.slice(&values.reference(base), 127, 64, index_span);
        assert_eq!(values.ir.values.len(), before + 1);
        assert_eq!(
            values.ir.values[id(&selected).0 as usize].kind,
            ProcessValueKind::BitSlice {
                base,
                high: 127,
                low: 64
            }
        );
        let checked = values.checked_index(
            &values.reference(offset),
            &Expr::Const(1),
            -8,
            7,
            index_span,
        );
        let checked_id = id(&checked);
        assert!(matches!(values.ir.values[checked_id.0 as usize].kind,
            ProcessValueKind::CheckedIndex { index, left: -8, right: 7, span, .. }
                if index == offset && span == index_span));
        let before = values.ir.values.len();
        let root = values.select(&checked, &selected, &selected, index_span);
        assert_eq!(values.ir.values.len(), before + 1);
        assert_eq!(
            values.ir.values[id(&root).0 as usize].kind,
            ProcessValueKind::Select {
                condition: checked_id,
                then_value: id(&selected),
                else_value: id(&selected),
            }
        );
        assert_eq!(values.ir.values[base.0 as usize].bit_width, Some(128));
        assert_eq!(
            values.ir.value_layouts[base.0 as usize],
            Some(layout.clone())
        );
        assert_eq!(values.ir.values[base.0 as usize].span, operand_span);
        let Expr::Canonical { reads, .. } = &root else {
            unreachable!()
        };
        assert_eq!(reads.as_ref(), &[SignalId(1), SignalId(0)]);
        let mut draft = HardwareDraft::default();
        draft.drivers.push(Driver {
            target: SignalId(0),
            cond: None,
            expr: root,
            meta: None,
            ctx: 0,
            span: Some(index_span),
        });
        values.retain_reachable(&mut draft);
        assert_eq!(values.ir.value_layouts[base.0 as usize], Some(layout));
        assert_eq!(
            values
                .ir
                .values
                .iter()
                .filter(|value| matches!(value.kind, ProcessValueKind::ForeignCall { .. }))
                .count(),
            1
        );
        assert!(
            values.ir.validate(2).is_empty(),
            "{:?}",
            values.ir.validate(2)
        );
    }

    #[test]
    fn read_constructors_keep_observation_states_and_source_anchors() {
        let mut values = SourceValues::default();
        for (index, state) in [
            ProcessSignalState::Current,
            ProcessSignalState::Old,
            ProcessSignalState::Event,
        ]
        .into_iter()
        .enumerate()
        {
            let span = Span::new(FileId(0), index as u32..index as u32 + 1);
            let expression = values.signal(SignalId(7), state, span);
            let value = &values.ir.values[id(&expression).0 as usize];
            assert_eq!(value.span, span);
            assert_eq!(
                value.kind,
                ProcessValueKind::Signal {
                    signals: vec![SignalId(7)],
                    state
                }
            );
            let Expr::Canonical { reads, .. } = expression else {
                unreachable!()
            };
            assert_eq!(reads.as_ref(), &[SignalId(7)]);
        }
        assert_eq!(values.ir.values.len(), 3);
    }
}
