//! Companion-plane reconstruction without canonical-to-expression round trips.

use super::*;

impl SourceValues {
    /// Move dependencies once, then append any new guards after their operands.
    /// Unchanged nodes retain their entire format, including non-digital forms.
    pub(in crate::ir::lower) fn reconstruct_metavalues(
        &mut self,
        meta_of: &HashMap<u32, u32>,
        elems: &HashMap<u32, u32>,
        encodings: &HashMap<u32, LogicEncoding>,
    ) -> Vec<ProcessValueId> {
        let SourceValues {
            ir: mut old,
            explicit_meta,
            raw_bits,
            ..
        } = std::mem::take(self);
        let mut mapped = Vec::with_capacity(old.values.len());
        for (index, mut value) in old.values.into_iter().enumerate() {
            crate::ir::process::remap_process_value_dependencies(&mut value.kind, |child| {
                assert!(
                    (child.0 as usize) < index,
                    "source operands precede their users"
                );
                mapped[child.0 as usize]
            });
            let layout = old.value_layouts.get_mut(index).and_then(Option::take);
            let span = value.span;
            if let ProcessValueKind::MetaCompare {
                not_equal,
                operands,
                inner,
            } = &value.kind
            {
                let unknown = self.comparison_unknown(operands, span, meta_of, elems, encodings);
                value.kind = match unknown {
                    Some(unknown) if *not_equal => ProcessValueKind::Binary {
                        operation: ProcessBinaryOp::Or,
                        left: *inner,
                        right: unknown,
                    },
                    Some(unknown) => {
                        let definite = self.meta_not(unknown, span);
                        ProcessValueKind::Binary {
                            operation: ProcessBinaryOp::And,
                            left: *inner,
                            right: definite,
                        }
                    }
                    None => ProcessValueKind::RawResize { operand: *inner },
                };
                mapped.push(self.push_node(value, layout));
                continue;
            }
            let original = self.push_node(value, layout);
            let raw_bit = raw_bits.contains(&ProcessValueId(index as u32));
            if raw_bit {
                self.raw_bits.insert(original);
            }
            let replacement = match self.ir.values[original.0 as usize].kind {
                ProcessValueKind::Binary {
                    operation:
                        ProcessBinaryOp::Eq
                        | ProcessBinaryOp::Ne
                        | ProcessBinaryOp::Lt
                        | ProcessBinaryOp::Le
                        | ProcessBinaryOp::Gt
                        | ProcessBinaryOp::Ge
                        | ProcessBinaryOp::SignedLt
                        | ProcessBinaryOp::SignedLe
                        | ProcessBinaryOp::SignedGt
                        | ProcessBinaryOp::SignedGe,
                    left,
                    right,
                } => self
                    .comparison_unknown(&[left, right], span, meta_of, elems, encodings)
                    .map(|condition| {
                        let zero = self.meta_number(0, span);
                        ProcessValueKind::Select {
                            condition,
                            then_value: zero,
                            else_value: original,
                        }
                    }),
                ProcessValueKind::BitSlice { base, high, low } if high == low && !raw_bit => {
                    // Static and checked-shift reads share the same reconstruction.
                    // A RawResize between slice and shift remains a typed boundary.
                    let (signal, offset) = match self.ir.values[base.0 as usize].kind {
                        ProcessValueKind::Binary {
                            operation: ProcessBinaryOp::Shr,
                            left,
                            right,
                        } if low == 0 => (left, Some(right)),
                        _ => (base, None),
                    };
                    self.meta_companion(signal, span, meta_of)
                        .and_then(|(companion, mut meta)| {
                            let encoding = encodings.get(&companion)?;
                            if let Some(offset) = offset {
                                let stride = self.meta_number(4, span);
                                let offset =
                                    self.meta_binary(ProcessBinaryOp::Mul, offset, stride, span);
                                meta = self.meta_binary(ProcessBinaryOp::Shr, meta, offset, span);
                            }
                            let index = if offset.is_some() { 0 } else { low };
                            let nibble = self.meta_slice(meta, 4 * index + 3, 4 * index, span);
                            let binary = self.meta_members(nibble, &encoding.binary, span);
                            let condition = self.meta_not(binary, span);
                            let low =
                                self.meta_number(encoding.binary_value(false).unwrap_or(0), span);
                            let high =
                                self.meta_number(encoding.binary_value(true).unwrap_or(0), span);
                            let binary_value = self.meta_kind(
                                ProcessValueKind::Select {
                                    condition: original,
                                    then_value: high,
                                    else_value: low,
                                },
                                span,
                            );
                            Some(ProcessValueKind::Select {
                                condition,
                                then_value: nibble,
                                else_value: binary_value,
                            })
                        })
                }
                _ => None,
            };
            let result = if let Some(kind) = replacement {
                let node = &self.ir.values[original.0 as usize];
                let value = ProcessValue {
                    span: node.span,
                    ty: node.ty.clone(),
                    bit_width: node.bit_width,
                    kind,
                };
                let layout = self.ir.value_layouts[original.0 as usize].clone();
                self.push_node(value, layout)
            } else {
                original
            };
            mapped.push(result);
        }
        for (value, mut meta) in explicit_meta {
            self.remap_expression(&mut meta, &mapped);
            self.set_explicit_meta(mapped[value.0 as usize], meta);
        }
        mapped
    }

    fn meta_kind(&mut self, kind: ProcessValueKind, span: crate::diag::Span) -> ProcessValueId {
        self.push_node(
            ProcessValue {
                span,
                ty: None,
                bit_width: None,
                kind,
            },
            None,
        )
    }

    fn meta_number(&mut self, number: u64, span: crate::diag::Span) -> ProcessValueId {
        self.meta_kind(
            ProcessValueKind::Number(ProcessNumber::Integer(vec![number])),
            span,
        )
    }

    fn meta_binary(
        &mut self,
        operation: ProcessBinaryOp,
        left: ProcessValueId,
        right: ProcessValueId,
        span: crate::diag::Span,
    ) -> ProcessValueId {
        self.meta_kind(
            ProcessValueKind::Binary {
                operation,
                left,
                right,
            },
            span,
        )
    }

    fn meta_slice(
        &mut self,
        base: ProcessValueId,
        high: u32,
        low: u32,
        span: crate::diag::Span,
    ) -> ProcessValueId {
        self.meta_kind(ProcessValueKind::BitSlice { base, high, low }, span)
    }

    fn meta_not(&mut self, value: ProcessValueId, span: crate::diag::Span) -> ProcessValueId {
        let zero = self.meta_number(0, span);
        self.meta_binary(ProcessBinaryOp::Eq, value, zero, span)
    }

    fn meta_members(
        &mut self,
        value: ProcessValueId,
        members: &HashSet<u64>,
        span: crate::diag::Span,
    ) -> ProcessValueId {
        let mut members = members.iter().copied().collect::<Vec<_>>();
        members.sort_unstable();
        let mut result = None;
        for member in members {
            let member = self.meta_number(member, span);
            let equal = self.meta_binary(ProcessBinaryOp::Eq, value, member, span);
            result = Some(match result {
                Some(previous) => self.meta_binary(ProcessBinaryOp::Or, previous, equal, span),
                None => equal,
            });
        }
        result.unwrap_or_else(|| self.meta_number(0, span))
    }

    fn meta_companion(
        &mut self,
        value: ProcessValueId,
        span: crate::diag::Span,
        meta_of: &HashMap<u32, u32>,
    ) -> Option<(u32, ProcessValueId)> {
        let ProcessValueKind::Signal { signals, state } = &self.ir.values[value.0 as usize].kind
        else {
            return None;
        };
        if !matches!(state, ProcessSignalState::Current | ProcessSignalState::Old) {
            return None;
        }
        let [signal] = signals.as_slice() else {
            return None;
        };
        let companion = *meta_of.get(&signal.0)?;
        let kind = ProcessValueKind::Signal {
            signals: vec![SignalId(companion)],
            state: *state,
        };
        Some((companion, self.meta_kind(kind, span)))
    }

    fn comparison_unknown(
        &mut self,
        operands: &[ProcessValueId],
        span: crate::diag::Span,
        meta_of: &HashMap<u32, u32>,
        elems: &HashMap<u32, u32>,
        encodings: &HashMap<u32, LogicEncoding>,
    ) -> Option<ProcessValueId> {
        let mut result = None;
        for &operand in operands {
            let Some((companion, meta)) = self.meta_companion(operand, span, meta_of) else {
                continue;
            };
            let mut unknown = self.meta_number(0, span);
            if let Some(encoding) = encodings.get(&companion) {
                for index in 0..elems.get(&companion).copied().unwrap_or(0) {
                    let nibble = self.meta_slice(meta, 4 * index + 3, 4 * index, span);
                    let element = self.meta_members(nibble, &encoding.unknown, span);
                    unknown = self.meta_binary(ProcessBinaryOp::Or, unknown, element, span);
                }
            }
            result = Some(match result {
                Some(previous) => self.meta_binary(ProcessBinaryOp::Or, previous, unknown, span),
                None => unknown,
            });
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evaluate(expression: &Expr, frames: &[[u64; 3]; 2]) -> u64 {
        match expression {
            Expr::Const(value) => *value,
            Expr::Current(signal) => frames[0][signal.0 as usize],
            Expr::Old(signal) => frames[1][signal.0 as usize],
            Expr::Slice { base, hi, lo } => {
                (evaluate(base, frames) >> lo) & ((1 << (hi - lo + 1)) - 1)
            }
            Expr::Binary { op, lhs, rhs } => {
                let left = evaluate(lhs, frames);
                let right = evaluate(rhs, frames);
                match op {
                    BinOp::And => left & right,
                    BinOp::Or => left | right,
                    BinOp::Add => left.wrapping_add(right),
                    BinOp::Mul => left.wrapping_mul(right),
                    BinOp::Shl => left.checked_shl(right as u32).unwrap_or(0),
                    BinOp::Shr => left.checked_shr(right as u32).unwrap_or(0),
                    BinOp::Eq => u64::from(left == right),
                    BinOp::Ne => u64::from(left != right),
                    BinOp::SLt => u64::from((left as i64) < (right as i64)),
                    _ => panic!("unexpected test operation: {op:?}"),
                }
            }
            Expr::Select { cond, then, els } => evaluate(
                if evaluate(cond, frames) != 0 {
                    then
                } else {
                    els
                },
                frames,
            ),
            Expr::CheckedIndex { index, valid, .. } => {
                assert_ne!(evaluate(valid, frames), 0);
                evaluate(index, frames)
            }
            _ => panic!("unexpected test expression: {expression:?}"),
        }
    }

    #[test]
    fn raw_storage_bits_survive_compaction_and_reordered_read_reconstruction() {
        let span = crate::diag::Span::new(crate::diag::FileId(0), 10..20);
        let encoding = LogicEncoding {
            value_bits: [(4, false), (9, true)].into(),
            binary: [4, 9].into(),
            ..Default::default()
        };
        for state in [ProcessSignalState::Current, ProcessSignalState::Old] {
            let mut arena = SourceValues::default();
            arena.append(&Expr::Const(99), span, None); // Unreachable prefix forces remapping.
            let value = arena.signal(SignalId(0), state, span);
            let companion = arena.signal(SignalId(1), state, span);
            let raw = arena.raw_slice(&value, 0, 0, span);
            let source = arena.slice(&value, 0, 0, span);
            let element = arena.element_disc(&value, &companion, 0, &encoding, span);
            let offset =
                arena.checked_index(&Expr::Current(SignalId(2)), &Expr::Const(1), 0, 1, span);
            let shifted = arena.binary(BinOp::Shr, &value, &offset, span);
            let raw_shift = arena.raw_slice(&shifted, 0, 0, span);
            let source_shift = arena.slice(&shifted, 0, 0, span);
            let mut draft = HardwareDraft::default();
            for expr in [raw, source, element, raw_shift, source_shift] {
                draft.drivers.push(Driver {
                    target: SignalId(2),
                    cond: None,
                    expr,
                    meta: None,
                    ctx: 0,
                    span: Some(span),
                });
            }
            arena.retain_reachable(&mut draft);
            let mapped = arena.reconstruct_metavalues(
                &[(0, 1)].into(),
                &[(1, 2)].into(),
                &[(1, encoding.clone())].into(),
            );
            for driver in &mut draft.drivers {
                arena.remap_expression(&mut driver.expr, &mapped);
            }
            let roots = draft
                .drivers
                .iter()
                .map(|driver| {
                    let Expr::Canonical { value, .. } = driver.expr else {
                        unreachable!()
                    };
                    assert_eq!(arena.ir.values[value.0 as usize].span, span);
                    crate::ir::derive::materialize_digital_expression(&arena.ir, value).unwrap()
                })
                .collect::<Vec<_>>();
            for bits in 0..4 {
                for meta in [4, 9] {
                    for index in 0..2 {
                        let frames = [
                            [bits, meta | (meta << 4), index],
                            [bits ^ 3, meta | (meta << 4), index],
                        ];
                        let bits = match state {
                            ProcessSignalState::Current => bits,
                            _ => bits ^ 3,
                        };
                        let bit = bits & 1;
                        assert_eq!(
                            evaluate(&roots[0], &frames),
                            bit,
                            "raw projection stays 0/1"
                        );
                        let disc = if bit == 0 { 4 } else { 9 };
                        assert_eq!(
                            evaluate(&roots[1], &frames),
                            disc,
                            "source read decodes enum"
                        );
                        assert_eq!(
                            evaluate(&roots[2], &frames),
                            disc,
                            "metadata reads the physical bit"
                        );
                        let bit = (bits >> index) & 1;
                        assert_eq!(evaluate(&roots[3], &frames), bit, "checked raw shift");
                        assert_eq!(
                            evaluate(&roots[4], &frames),
                            if bit == 0 { 4 } else { 9 },
                            "checked source shift"
                        );
                    }
                }
            }
            assert!(arena.ir.validate(3).is_empty());
        }
    }

    #[test]
    fn canonical_unary_and_logical_companions_match_element_contracts() {
        let span = crate::diag::Span::new(crate::diag::FileId(0), 10..20);
        let encoding = LogicEncoding {
            value_bits: [(0, false), (1, true), (3, false)].into(),
            binary: [0, 1].into(),
            unknown: [3].into(),
            unary_ops: [("not".into(), [(0, 1), (1, 0), (3, 3)].into())].into(),
            binary_ops: [("and", BinOp::And), ("or", BinOp::Or), ("xor", BinOp::Xor)]
                .into_iter()
                .map(|(name, operation)| {
                    let mut table = HashMap::new();
                    for left in [0, 1, 3] {
                        for right in [0, 1, 3] {
                            let result = match operation {
                                BinOp::And if left == 0 || right == 0 => 0,
                                BinOp::Or if left == 1 || right == 1 => 1,
                                _ if left == 3 || right == 3 => 3,
                                BinOp::And => left & right,
                                BinOp::Or => left | right,
                                BinOp::Xor => left ^ right,
                                _ => unreachable!(),
                            };
                            table.insert((left, right), result);
                        }
                    }
                    (name.into(), table)
                })
                .collect(),
            ..Default::default()
        };
        for operation in [None, Some(BinOp::And), Some(BinOp::Or), Some(BinOp::Xor)] {
            let mut sink = DiagnosticSink::new();
            let resolved = Resolved::default();
            let mut lowering = Lowering::new(&mut sink, &resolved);
            lowering.add_signal("E", "value", 2, span);
            lowering.add_signal("E", "meta", 8, span);
            lowering.out.meta_of.insert(0, 1);
            lowering
                .logic_encodings
                .insert(DEFAULT_LOGIC_TYPE.into(), encoding.clone());
            let mut temps = MetaTemps::inline_only();
            let expression = match operation {
                None => lowering.lower_meta_ir(
                    &Expr::Unary {
                        op: UnOp::Not,
                        rhs: Box::new(Expr::Current(SignalId(0))),
                    },
                    2,
                    &mut temps,
                ),
                Some(operation) => lowering.logical_meta(
                    operation,
                    &Expr::Current(SignalId(0)),
                    &Expr::Const(1),
                    2,
                    &mut temps,
                ),
            }
            .expect("companion must be built");
            let Expr::Canonical { value: root, .. } = expression else {
                panic!("companion constructor must return a canonical root");
            };
            let arena = lowering.source_values.get_mut();
            let mapped = arena.reconstruct_metavalues(
                &[(0, 1)].into(),
                &[(1, 2)].into(),
                &[(1, encoding.clone())].into(),
            );
            let actual = crate::ir::derive::materialize_digital_expression(
                &arena.ir,
                mapped[root.0 as usize],
            )
            .unwrap();
            for first in [0, 1, 3] {
                for second in [0, 1, 3] {
                    let frames = [[
                        u64::from(first == 1) | (u64::from(second == 1) << 1),
                        first | (second << 4),
                        0,
                    ]; 2];
                    let mut expected = 0;
                    for (index, disc) in [first, second].into_iter().enumerate() {
                        let result = match operation {
                            None => encoding.unary_ops["not"][&disc],
                            Some(operation) => {
                                let name = match operation {
                                    BinOp::And => "and",
                                    BinOp::Or => "or",
                                    _ => "xor",
                                };
                                encoding.binary_ops[name][&(disc, u64::from(index == 0))]
                            }
                        };
                        if !encoding.binary.contains(&result) {
                            expected |= result << (4 * index);
                        }
                    }
                    assert_eq!(
                        evaluate(&actual, &frames),
                        expected,
                        "{operation:?}: {frames:?}"
                    );
                }
            }
            assert!(arena.ir.validate(2).is_empty());
        }
    }

    #[test]
    fn canonical_reconstruction_matches_fragment_semantics_with_reordered_encoding() {
        let span = crate::diag::Span::new(crate::diag::FileId(0), 10..20);
        // Deliberately unlike std's declaration order: no numeric interval or
        // built-in discriminant may substitute for the source-owned sets.
        let encoding = LogicEncoding {
            value_bits: [(4, false), (9, true), (2, false), (6, true)].into(),
            binary: [4, 9].into(),
            unknown: [11, 12].into(),
            ..Default::default()
        };
        let meta_of = [(0, 1)].into();
        let elems = [(1, 2)].into();
        let encodings = [(1, encoding)].into();
        let mut expressions = Vec::new();
        for value in [Expr::Current(SignalId(0)), Expr::Old(SignalId(0))] {
            expressions.push(Expr::Slice {
                base: Box::new(value.clone()),
                hi: 1,
                lo: 1,
            });
            expressions.push(Expr::Slice {
                base: Box::new(Expr::Binary {
                    op: BinOp::Shr,
                    lhs: Box::new(value.clone()),
                    rhs: Box::new(Expr::CheckedIndex {
                        index: Box::new(Expr::Current(SignalId(2))),
                        valid: Box::new(Expr::Const(1)),
                        left: 1,
                        right: 0,
                        span,
                    }),
                }),
                hi: 0,
                lo: 0,
            });
            for operation in [BinOp::Eq, BinOp::Ne, BinOp::SLt] {
                expressions.push(Expr::Binary {
                    op: operation,
                    lhs: Box::new(value.clone()),
                    rhs: Box::new(Expr::Const(0)),
                });
            }
            for ne in [false, true] {
                expressions.push(Expr::MetaCmp {
                    ne,
                    operands: vec![value.clone()],
                    inner: Box::new(Expr::Binary {
                        op: BinOp::Eq,
                        lhs: Box::new(value.clone()),
                        rhs: Box::new(Expr::Const(0)),
                    }),
                });
            }
        }
        let mut arena = SourceValues::default();
        let roots = expressions
            .iter()
            .map(|expression| arena.append(expression, span, None))
            .collect::<Vec<_>>();
        let mapped = arena.reconstruct_metavalues(&meta_of, &elems, &encodings);
        assert!(
            arena.ir.validate(3).is_empty(),
            "{:?}",
            arena.ir.validate(3)
        );
        assert_eq!(
            arena
                .ir
                .values
                .iter()
                .filter(|node| matches!(node.kind, ProcessValueKind::CheckedIndex { .. }))
                .count(),
            2
        );
        for (mut expected, root) in expressions.into_iter().zip(roots) {
            reconstruct_expr(&mut expected, &meta_of, &elems, &encodings);
            let root = mapped[root.0 as usize];
            assert_eq!(arena.ir.values[root.0 as usize].span, span);
            let actual =
                crate::ir::derive::materialize_digital_expression(&arena.ir, root).unwrap();
            for first in [2, 4, 6, 9, 11, 12] {
                for second in [2, 4, 6, 9, 11, 12] {
                    for bits in 0..4 {
                        for index in 0..2 {
                            let frames = [
                                [bits, first | (second << 4), index],
                                [bits ^ 3, second | (first << 4), index],
                            ];
                            assert_eq!(
                                evaluate(&actual, &frames),
                                evaluate(&expected, &frames),
                                "{expected:?}, frames={frames:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn reconstruction_moves_unprojectable_nodes_and_retains_full_formats() {
        let span = crate::diag::Span::new(crate::diag::FileId(0), 10..20);
        let mut arena = SourceValues::default();
        let base = arena.append(&Expr::Current(SignalId(0)), span, None);
        let index = arena.append(&Expr::Const(1), span, None);
        let value = ProcessValue {
            span,
            ty: Some(crate::types::Ty::Integer),
            bit_width: Some(4),
            kind: ProcessValueKind::Index { base, index },
        };
        let layout = SourceLayout {
            span,
            kind: LayoutKind::Scalar {
                width: 4,
                domain: ScalarDomain::Integer,
                nominal: Some("integer".into()),
                value_range: Some((-8, 7)),
            },
        };
        let root = arena.push_node(value.clone(), Some(layout.clone()));
        let before = arena.ir.values.clone();
        let mapped =
            arena.reconstruct_metavalues(&HashMap::new(), &HashMap::new(), &HashMap::new());
        assert_eq!(arena.ir.values, before);
        assert_eq!(mapped[root.0 as usize], root);
        assert_eq!(arena.ir.value_layouts[root.0 as usize], Some(layout));
        assert_eq!(arena.reads[root.0 as usize].as_ref(), &[SignalId(0)]);
        assert!(arena.ir.validate(1).is_empty());
    }

    #[test]
    fn reconstructed_reads_share_checked_calls_and_respect_resize_boundaries() {
        let span = crate::diag::Span::new(crate::diag::FileId(0), 10..20);
        let mut arena = SourceValues::default();
        let index = arena.append(
            &Expr::CheckedIndex {
                index: Box::new(Expr::CCall {
                    name: "next_index".into(),
                    args: vec![],
                    f64_args: vec![],
                    integer_args: vec![],
                    f64_ret: false,
                    integer_ret: true,
                }),
                valid: Box::new(Expr::Const(1)),
                left: 7,
                right: 0,
                span,
            },
            span,
            None,
        );
        let shifted = arena.append(
            &Expr::Binary {
                op: BinOp::Shr,
                lhs: Box::new(Expr::Old(SignalId(0))),
                rhs: Box::new(arena.reference(index)),
            },
            span,
            None,
        );
        let read = Expr::Slice {
            base: Box::new(arena.reference(shifted)),
            hi: 0,
            lo: 0,
        };
        let first = arena.append(&read, span, None);
        let second = arena.append(&read, span, None);
        let boundary = arena.bind_scalar(shifted, Some(crate::types::Ty::Integer), span);
        let bounded = arena.append(
            &Expr::Slice {
                base: Box::new(arena.reference(boundary)),
                hi: 0,
                lo: 0,
            },
            span,
            None,
        );
        let layout = SourceLayout {
            span,
            kind: LayoutKind::Scalar {
                width: 4,
                domain: ScalarDomain::Integer,
                nominal: Some("integer".into()),
                value_range: Some((-8, 7)),
            },
        };
        arena.ir.values[first.0 as usize].bit_width = Some(4);
        arena.ir.values[first.0 as usize].ty = Some(crate::types::Ty::Integer);
        arena.ir.value_layouts.resize(arena.ir.values.len(), None);
        arena.ir.value_layouts[first.0 as usize] = Some(layout.clone());
        let encoding = LogicEncoding {
            value_bits: [(4, false), (9, true)].into(),
            binary: [4, 9].into(),
            ..Default::default()
        };
        let mapped = arena.reconstruct_metavalues(
            &[(0, 1)].into(),
            &[(1, 8)].into(),
            &[(1, encoding)].into(),
        );
        for root in [first, second] {
            let root = mapped[root.0 as usize];
            assert!(matches!(
                arena.ir.values[root.0 as usize].kind,
                ProcessValueKind::Select { .. }
            ));
            assert_eq!(arena.ir.values[root.0 as usize].span, span);
            assert!(arena.reads[root.0 as usize].contains(&SignalId(1)));
        }
        let first = mapped[first.0 as usize];
        assert_eq!(arena.ir.values[first.0 as usize].bit_width, Some(4));
        assert_eq!(
            arena.ir.values[first.0 as usize].ty,
            Some(crate::types::Ty::Integer)
        );
        assert_eq!(arena.ir.value_layouts[first.0 as usize], Some(layout));
        assert!(
            matches!(arena.ir.values[mapped[bounded.0 as usize].0 as usize].kind,
            ProcessValueKind::BitSlice { base, .. } if base == mapped[boundary.0 as usize])
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
        assert_eq!(
            arena
                .ir
                .values
                .iter()
                .filter(|node| matches!(node.kind, ProcessValueKind::CheckedIndex { .. }))
                .count(),
            1
        );
        let mut checked_users = 0;
        for node in &arena.ir.values {
            match &node.kind {
                ProcessValueKind::Binary {
                    operation: ProcessBinaryOp::Mul,
                    left,
                    ..
                } => {
                    assert_eq!(*left, mapped[index.0 as usize]);
                    checked_users += 1;
                }
                ProcessValueKind::Signal { signals, state } if signals == &[SignalId(1)] => {
                    assert_eq!(*state, ProcessSignalState::Old);
                }
                _ => {}
            }
        }
        assert_eq!(checked_users, 2);
        assert!(
            arena.ir.validate(2).is_empty(),
            "{:?}",
            arena.ir.validate(2)
        );
    }
}
