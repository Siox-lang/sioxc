//! Derived compatibility scheduler forms.
//!
//! Canonical [`ProcessIr`] CFGs own behavior. This module projects the
//! restricted hardware regions back into the compact [`Driver`] and
//! [`EventBlock`] forms still consumed by the native object ABI. Procedural
//! processes are deliberately ignored: they have control flow and suspension
//! semantics that cannot be represented by that compatibility view.

use std::collections::HashSet;

use super::*;

/// Replace the compatibility scheduler graph with a projection of canonical
/// Process IR hardware regions.
pub(crate) fn derive_scheduler_forms(design: &mut Design) -> Result<(), String> {
    let (drivers, event_blocks) = scheduler_forms(&design.process_ir)?;
    design.drivers = drivers;
    design.event_blocks = event_blocks;
    Ok(())
}

fn scheduler_forms(ir: &ProcessIr) -> Result<(Vec<Driver>, Vec<EventBlock>), String> {
    let mut drivers = Vec::new();
    let mut event_blocks = Vec::new();

    for process in &ir.processes {
        match process.region {
            ProcessRegion::Procedural => {}
            ProcessRegion::Combinational => {
                for assignment in region_assignments(ir, process, process.entry)? {
                    drivers.push(Driver {
                        target: assignment.target,
                        cond: assignment.guard,
                        expr: assignment.value,
                        meta: None,
                        ctx: assignment.driver_context,
                        span: Some(assignment.span),
                    });
                }
            }
            ProcessRegion::Event {
                condition,
                body,
                driver_context,
            } => {
                // The event condition and body are semantic metadata, not a
                // license to ignore executable work on the other CFG path.
                // Reject such a graph before replacing the existing view.
                let entry = process
                    .blocks
                    .get(process.entry.0 as usize)
                    .ok_or_else(|| format!("event process {:?} has no entry", process.id))?;
                let ProcessTerminator::Branch {
                    condition: entry_condition,
                    then_block,
                    else_block,
                } = &entry.terminator
                else {
                    return Err(format!(
                        "event process {:?} entry is not a branch",
                        process.id
                    ));
                };
                if !entry.instructions.is_empty()
                    || *entry_condition != condition
                    || *then_block != body
                {
                    return Err(format!(
                        "event process {:?} entry does not match its declared region",
                        process.id
                    ));
                }
                let exit = process.blocks.get(else_block.0 as usize).ok_or_else(|| {
                    format!("event process {:?} has no inactive exit", process.id)
                })?;
                if !exit.instructions.is_empty()
                    || !matches!(
                        exit.terminator,
                        ProcessTerminator::Return { value: None, .. }
                    )
                {
                    return Err(format!(
                        "event process {:?} inactive path contains behavior",
                        process.id
                    ));
                }
                let condition = digital_expr(ir, condition)?;
                let updates = region_assignments(ir, process, body)?
                    .into_iter()
                    .map(|assignment| {
                        if assignment.driver_context != driver_context {
                            return Err(format!(
                                "event process {:?} write uses driver context {}, expected {}",
                                process.id, assignment.driver_context, driver_context
                            ));
                        }
                        Ok(NextUpdate {
                            target: assignment.target,
                            cond: assignment.guard,
                            expr: assignment.value,
                            meta: None,
                            span: Some(assignment.span),
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                event_blocks.push(EventBlock {
                    condition,
                    updates,
                    ctx: driver_context,
                });
            }
        }
    }

    Ok((drivers, event_blocks))
}

/// Public designs cannot author a second executable scheduler representation.
/// Reuse the same projection used by source lowering; never import trees back
/// into the arena to repair an inconsistent API input. Legacy backend fixtures
/// are deliberately permitted only in unit-test builds.
#[cfg(not(test))]
pub(super) fn validate_scheduler_forms(design: &Design) -> Result<(), String> {
    fn root_matches(left: &Expr, right: &Expr) -> bool {
        matches!((left, right),
            (Expr::Canonical { value: a, reads: ar }, Expr::Canonical { value: b, reads: br })
                if a == b && ar == br)
    }
    fn guard_matches(left: Option<&Expr>, right: Option<&Expr>) -> bool {
        match (left, right) {
            (None, None) => true,
            (Some(a), Some(b)) => root_matches(a, b),
            _ => false,
        }
    }
    let (drivers, blocks) = scheduler_forms(&design.process_ir)?;
    if drivers.len() != design.drivers.len() || blocks.len() != design.event_blocks.len() {
        return Err("scheduler view count disagrees with canonical Process CFGs".into());
    }
    for (index, (expected, actual)) in drivers.iter().zip(&design.drivers).enumerate() {
        if expected.target != actual.target
            || expected.ctx != actual.ctx
            || expected.span != actual.span
            || !guard_matches(expected.cond.as_ref(), actual.cond.as_ref())
            || !root_matches(&expected.expr, &actual.expr)
        {
            return Err(format!(
                "scheduler driver {index} disagrees with canonical Process CFGs"
            ));
        }
    }
    for (index, (expected, actual)) in blocks.iter().zip(&design.event_blocks).enumerate() {
        if expected.ctx != actual.ctx
            || !root_matches(&expected.condition, &actual.condition)
            || expected.updates.len() != actual.updates.len()
            || expected.updates.iter().zip(&actual.updates).any(|(a, b)| {
                a.target != b.target
                    || a.span != b.span
                    || !guard_matches(a.cond.as_ref(), b.cond.as_ref())
                    || !root_matches(&a.expr, &b.expr)
            })
        {
            return Err(format!(
                "scheduler event block {index} disagrees with canonical Process CFGs"
            ));
        }
    }
    Ok(())
}

/// One hardware assignment after decoding its Process place and value.
struct DerivedAssignment {
    target: SignalId,
    guard: Option<Expr>,
    value: Expr,
    driver_context: u32,
    span: crate::diag::Span,
}

/// Decode the forward, structured CFG emitted for a hardware region.
///
/// A conditional hardware write has exactly this shape:
///
/// ```text
/// current --condition--> assignment --+--> continuation
///         `-------------false----------'
/// ```
///
/// The join is visited once, so later writes are not duplicated under both
/// paths. General procedural CFGs never enter this projection.
fn region_assignments(
    ir: &ProcessIr,
    process: &ProcessCfg,
    start: ProcessBlockId,
) -> Result<Vec<DerivedAssignment>, String> {
    let mut assignments = Vec::new();
    let mut current = start;
    let mut visited = HashSet::new();

    loop {
        if !visited.insert(current) {
            return Err(format!(
                "hardware process {:?} contains a scheduler-derivation cycle at {:?}",
                process.id, current
            ));
        }
        let block = process.blocks.get(current.0 as usize).ok_or_else(|| {
            format!(
                "hardware process {:?} references missing block {:?}",
                process.id, current
            )
        })?;
        append_assignments(ir, process, &block.instructions, None, &mut assignments)?;

        match &block.terminator {
            ProcessTerminator::Return { value: None, .. } => break,
            ProcessTerminator::Goto(next) => current = *next,
            ProcessTerminator::Branch {
                condition,
                then_block,
                else_block,
            } => {
                let arm = process.blocks.get(then_block.0 as usize).ok_or_else(|| {
                    format!(
                        "hardware process {:?} references missing guarded block {:?}",
                        process.id, then_block
                    )
                })?;
                if !visited.insert(*then_block) {
                    return Err(format!(
                        "hardware process {:?} reuses guarded block {:?}",
                        process.id, then_block
                    ));
                }
                if !matches!(arm.terminator, ProcessTerminator::Goto(join) if join == *else_block) {
                    return Err(format!(
                        "hardware process {:?} branch {:?} is not a single guarded-write arm",
                        process.id, block.id
                    ));
                }
                let guard = digital_expr(ir, *condition)?;
                append_assignments(
                    ir,
                    process,
                    &arm.instructions,
                    Some(guard),
                    &mut assignments,
                )?;
                current = *else_block;
            }
            other => {
                return Err(format!(
                    "hardware process {:?} block {:?} uses procedural terminator {other:?}",
                    process.id, block.id
                ));
            }
        }
    }

    Ok(assignments)
}

fn append_assignments(
    ir: &ProcessIr,
    process: &ProcessCfg,
    instructions: &[ProcessInstruction],
    guard: Option<Expr>,
    output: &mut Vec<DerivedAssignment>,
) -> Result<(), String> {
    for instruction in instructions {
        let ProcessInstruction::Assign {
            semantics: ProcessAssignment::StagedSignal,
            driver_context,
            target,
            value,
            span,
        } = instruction
        else {
            return Err(format!(
                "hardware process {:?} contains non-signal assignment instruction {instruction:?}",
                process.id
            ));
        };
        let target_value = ir.values.get(target.0 as usize).ok_or_else(|| {
            format!(
                "hardware process {:?} references missing target {:?}",
                process.id, target
            )
        })?;
        let ProcessValueKind::Signal {
            signals,
            state: ProcessSignalState::Current,
        } = &target_value.kind
        else {
            return Err(format!(
                "hardware process {:?} target {:?} is not current signal storage",
                process.id, target
            ));
        };
        let [target] = signals.as_slice() else {
            return Err(format!(
                "hardware process {:?} target {:?} is not one scalar signal",
                process.id, target
            ));
        };
        output.push(DerivedAssignment {
            target: *target,
            guard: guard.clone(),
            value: digital_expr(ir, *value)?,
            driver_context: driver_context.unwrap_or(process.id.0),
            span: *span,
        });
    }
    Ok(())
}

/// Retain a canonical root and its checked sensitivity, without reconstructing
/// expression trees. Only representation-neutral hardware nodes are accepted;
/// frontend/procedural values fail closed. Shared dependencies are visited once.
fn digital_expr(ir: &ProcessIr, id: ProcessValueId) -> Result<Expr, String> {
    let mut pending = vec![id];
    let mut visited = HashSet::new();
    let mut signals = HashSet::new();
    let mut reads = Vec::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        let value = ir
            .values
            .get(id.0 as usize)
            .ok_or_else(|| format!("missing Process value {id:?}"))?;
        match &value.kind {
            ProcessValueKind::Number(_)
            | ProcessValueKind::Char(_)
            | ProcessValueKind::Unary { .. }
            | ProcessValueKind::RawResize { .. }
            | ProcessValueKind::BitSlice { .. }
            | ProcessValueKind::TableLookup { .. }
            | ProcessValueKind::CheckedIndex { .. }
            | ProcessValueKind::Select { .. }
            | ProcessValueKind::MetaCompare { .. }
            | ProcessValueKind::ForeignCall { .. } => {}
            ProcessValueKind::Signal { signals: ids, .. } if ids.len() == 1 => {
                if signals.insert(ids[0]) {
                    reads.push(ids[0]);
                }
            }
            ProcessValueKind::Binary { operation, .. } => {
                digital_binary(operation)?;
            }
            other => {
                return Err(format!(
                "Process value {id:?} cannot derive a digital scheduler expression from {other:?}"
            ))
            }
        }
        // Children go on the stack last-first, as a reversed list would.
        let first = pending.len();
        let mut non_dominating = None;
        super::process::for_each_process_value_dependency(&value.kind, |child| {
            if child.0 >= id.0 {
                non_dominating = Some(child);
            }
            pending.push(child);
        });
        if let Some(child) = non_dominating {
            return Err(format!(
                "Process value {id:?} has non-dominating dependency {child:?}"
            ));
        }
        pending[first..].reverse();
    }
    Ok(Expr::Canonical {
        value: id,
        reads: reads.into(),
    })
}

/// Explicit expansion for small legacy expression-shape unit fixtures only.
/// Production scheduler projection must retain IDs, never call this helper.
#[cfg(test)]
pub(crate) fn materialize_digital_expression(
    ir: &ProcessIr,
    id: ProcessValueId,
) -> Result<Expr, String> {
    digital_node(ir, id, |child| materialize_digital_expression(ir, child))
}

/// Inspect one arena node with caller-supplied child handles. Source
/// normalization uses compact references, never recursive tree expansion.
pub(super) fn digital_node(
    ir: &ProcessIr,
    id: ProcessValueId,
    child: impl Fn(ProcessValueId) -> Result<Expr, String>,
) -> Result<Expr, String> {
    let value = ir
        .values
        .get(id.0 as usize)
        .ok_or_else(|| format!("missing Process value {id:?}"))?;
    // Production values are appended after their operands. Enforce that
    // invariant here as well: projection runs before full Design validation,
    // and a malformed arena must return an error, not recurse forever.
    let child_expression = |dependency: ProcessValueId| {
        if dependency.0 >= id.0 {
            return Err(format!(
                "Process value {id:?} has non-dominating dependency {dependency:?}"
            ));
        }
        child(dependency)
    };
    let recurse = |child| child_expression(child).map(Box::new);
    let expression = match &value.kind {
        ProcessValueKind::Number(ProcessNumber::Integer(words)) => match words.as_slice() {
            [] => Expr::Const(0),
            [word] => Expr::Const(*word),
            _ => Expr::WideConst(words.clone()),
        },
        ProcessValueKind::Number(ProcessNumber::Real(bits)) => Expr::Real(f64::from_bits(*bits)),
        ProcessValueKind::Char(character) => Expr::Logic(*character),
        ProcessValueKind::Signal { signals, state } => {
            let [signal] = signals.as_slice() else {
                return Err(format!(
                    "Process value {id:?} combines {} signals in a scalar digital expression",
                    signals.len()
                ));
            };
            match state {
                ProcessSignalState::Current => Expr::Current(*signal),
                ProcessSignalState::Old => Expr::Old(*signal),
                ProcessSignalState::Event => Expr::Event(*signal),
            }
        }
        ProcessValueKind::Unary { operation, operand } => Expr::Unary {
            op: match operation {
                ProcessUnaryOp::Neg => UnOp::Neg,
                ProcessUnaryOp::Not => UnOp::Not,
                ProcessUnaryOp::RealToInteger => UnOp::RealToInt,
                ProcessUnaryOp::IntegerToReal => UnOp::IntToReal,
            },
            rhs: recurse(*operand)?,
        },
        ProcessValueKind::RawResize { operand } => child_expression(*operand)?,
        ProcessValueKind::Binary {
            operation,
            left,
            right,
        } => Expr::Binary {
            op: digital_binary(operation)?,
            lhs: recurse(*left)?,
            rhs: recurse(*right)?,
        },
        ProcessValueKind::BitSlice { base, high, low } => Expr::Slice {
            base: recurse(*base)?,
            hi: *high,
            lo: *low,
        },
        ProcessValueKind::TableLookup { table, index } => Expr::TableLookup {
            table: *table,
            index: recurse(*index)?,
        },
        ProcessValueKind::CheckedIndex {
            index,
            valid,
            left,
            right,
            span,
        } => Expr::CheckedIndex {
            index: recurse(*index)?,
            valid: recurse(*valid)?,
            left: *left,
            right: *right,
            span: *span,
        },
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        } => Expr::Select {
            cond: recurse(*condition)?,
            then: recurse(*then_value)?,
            els: recurse(*else_value)?,
        },
        ProcessValueKind::MetaCompare {
            not_equal,
            operands,
            inner,
        } => Expr::MetaCmp {
            ne: *not_equal,
            operands: operands
                .iter()
                .map(|operand| child_expression(*operand))
                .collect::<Result<Vec<_>, _>>()?,
            inner: recurse(*inner)?,
        },
        ProcessValueKind::ForeignCall {
            name,
            arguments,
            float_arguments,
            integer_arguments,
            float_result,
            integer_result,
        } => Expr::CCall {
            name: name.clone(),
            args: arguments
                .iter()
                .map(|argument| child_expression(*argument))
                .collect::<Result<Vec<_>, _>>()?,
            f64_args: float_arguments.clone(),
            integer_args: integer_arguments.clone(),
            f64_ret: *float_result,
            integer_ret: *integer_result,
        },
        ProcessValueKind::Invalid => Expr::Unknown,
        other => {
            return Err(format!(
                "Process value {id:?} cannot derive a digital scheduler expression from {other:?}"
            ));
        }
    };
    Ok(expression)
}

fn digital_binary(operation: &ProcessBinaryOp) -> Result<BinOp, String> {
    Ok(match operation {
        ProcessBinaryOp::Add => BinOp::Add,
        ProcessBinaryOp::Sub => BinOp::Sub,
        ProcessBinaryOp::Mul => BinOp::Mul,
        ProcessBinaryOp::Div => BinOp::Div,
        ProcessBinaryOp::Rem => BinOp::Rem,
        ProcessBinaryOp::SignedAdd => BinOp::SAdd,
        ProcessBinaryOp::SignedSub => BinOp::SSub,
        ProcessBinaryOp::SignedMul => BinOp::SMul,
        ProcessBinaryOp::SignedDiv => BinOp::SDiv,
        ProcessBinaryOp::SignedRem => BinOp::SRem,
        ProcessBinaryOp::And => BinOp::And,
        ProcessBinaryOp::Or => BinOp::Or,
        ProcessBinaryOp::Xor => BinOp::Xor,
        ProcessBinaryOp::Shl => BinOp::Shl,
        ProcessBinaryOp::Shr => BinOp::Shr,
        ProcessBinaryOp::ArithmeticShr => BinOp::AShr,
        ProcessBinaryOp::Eq => BinOp::Eq,
        ProcessBinaryOp::Ne => BinOp::Ne,
        ProcessBinaryOp::Lt => BinOp::Lt,
        ProcessBinaryOp::Le => BinOp::Le,
        ProcessBinaryOp::Gt => BinOp::Gt,
        ProcessBinaryOp::Ge => BinOp::Ge,
        ProcessBinaryOp::SignedLt => BinOp::SLt,
        ProcessBinaryOp::SignedLe => BinOp::SLe,
        ProcessBinaryOp::SignedGt => BinOp::SGt,
        ProcessBinaryOp::SignedGe => BinOp::SGe,
        ProcessBinaryOp::FloatAdd => BinOp::FAdd,
        ProcessBinaryOp::FloatSub => BinOp::FSub,
        ProcessBinaryOp::FloatMul => BinOp::FMul,
        ProcessBinaryOp::FloatDiv => BinOp::FDiv,
        ProcessBinaryOp::FloatRem => BinOp::FRem,
        ProcessBinaryOp::FloatEq => BinOp::FEq,
        ProcessBinaryOp::FloatNe => BinOp::FNe,
        ProcessBinaryOp::FloatLt => BinOp::FLt,
        ProcessBinaryOp::FloatLe => BinOp::FLe,
        ProcessBinaryOp::FloatGt => BinOp::FGt,
        ProcessBinaryOp::FloatGe => BinOp::FGe,
        ProcessBinaryOp::Custom(symbol) => {
            return Err(format!(
                "custom Process operator `{symbol}` has no normalized digital opcode"
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{FileId, Span};
    use crate::elab::InstanceId;

    fn event_design() -> Design {
        let span = Span::new(FileId(0), 0..1);
        let signal = |path: &str| Signal {
            path: path.into(),
            declaration_span: span,
            width: 1,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: vec![0],
            enum_type: None,
        };
        let value = |kind| ProcessValue {
            span,
            ty: None,
            bit_width: Some(1),
            kind,
        };
        let process_ir = ProcessIr {
            values: vec![
                value(ProcessValueKind::Signal {
                    signals: vec![SignalId(0)],
                    state: ProcessSignalState::Current,
                }),
                value(ProcessValueKind::Signal {
                    signals: vec![SignalId(1)],
                    state: ProcessSignalState::Current,
                }),
                value(ProcessValueKind::Signal {
                    signals: vec![SignalId(2)],
                    state: ProcessSignalState::Event,
                }),
            ],
            processes: vec![ProcessCfg {
                id: ProcessId(0),
                root: InstanceId(0),
                owner: InstanceId(0),
                label: Some("clocked".into()),
                span,
                activation: ProcessActivation::Reactive {
                    sensitivity: vec![ProcessSensitivity::Signal(SignalId(2))],
                },
                region: ProcessRegion::Event {
                    condition: ProcessValueId(2),
                    body: ProcessBlockId(1),
                    driver_context: 9,
                },
                entry: ProcessBlockId(0),
                locals: vec![],
                blocks: vec![
                    ProcessBlock {
                        id: ProcessBlockId(0),
                        instructions: vec![],
                        terminator: ProcessTerminator::Branch {
                            condition: ProcessValueId(2),
                            then_block: ProcessBlockId(1),
                            else_block: ProcessBlockId(2),
                        },
                    },
                    ProcessBlock {
                        id: ProcessBlockId(1),
                        instructions: vec![ProcessInstruction::Assign {
                            semantics: ProcessAssignment::StagedSignal,
                            driver_context: Some(9),
                            target: ProcessValueId(0),
                            value: ProcessValueId(1),
                            span,
                        }],
                        terminator: ProcessTerminator::Return {
                            value: None,
                            span: None,
                        },
                    },
                    ProcessBlock::empty(ProcessBlockId(2)),
                ],
            }],
            ..ProcessIr::default()
        };
        Design {
            signals: vec![signal("q"), signal("d"), signal("clk")],
            drivers: vec![Driver {
                target: SignalId(2),
                cond: None,
                expr: Expr::Const(1),
                meta: None,
                ctx: 77,
                span: Some(span),
            }],
            process_ir,
            ..Design::default()
        }
    }

    #[test]
    fn event_scheduler_view_is_rebuilt_from_the_declared_process_region() {
        let mut design = event_design();

        derive_scheduler_forms(&mut design).expect("event region is derivable");

        assert!(
            design.drivers.is_empty(),
            "the pre-Process graph is replaced"
        );
        let [event] = design.event_blocks.as_slice() else {
            panic!("expected exactly one derived event block");
        };
        assert!(matches!(
            event.condition,
            Expr::Canonical {
                value: ProcessValueId(2),
                ..
            }
        ));
        assert_eq!(event.ctx, 9);
        let [update] = event.updates.as_slice() else {
            panic!("expected exactly one derived update");
        };
        assert_eq!(update.target, SignalId(0));
        assert!(matches!(
            update.expr,
            Expr::Canonical {
                value: ProcessValueId(1),
                ..
            }
        ));
    }

    #[test]
    fn guarded_combinational_writes_keep_source_order_and_context() {
        let mut design = event_design();
        let process = &mut design.process_ir.processes[0];
        process.region = ProcessRegion::Combinational;
        process.entry = ProcessBlockId(1);
        let instructions = process.blocks[1].instructions.clone();
        process.blocks[1].terminator = ProcessTerminator::Branch {
            condition: ProcessValueId(2),
            then_block: ProcessBlockId(3),
            else_block: ProcessBlockId(2),
        };
        process.blocks.push(ProcessBlock {
            id: ProcessBlockId(3),
            instructions,
            terminator: ProcessTerminator::Goto(ProcessBlockId(2)),
        });

        derive_scheduler_forms(&mut design).expect("guarded region is derivable");

        let [first, guarded] = design.drivers.as_slice() else {
            panic!("expected unconditional then guarded driver");
        };
        assert!(first.cond.is_none());
        assert!(matches!(
            guarded.cond,
            Some(Expr::Canonical {
                value: ProcessValueId(2),
                ..
            })
        ));
        assert_eq!(first.target, guarded.target);
        assert_eq!(first.ctx, 9);
        assert_eq!(guarded.ctx, 9);
        assert!(design.event_blocks.is_empty());
    }

    #[test]
    fn malformed_regions_fail_without_replacing_the_existing_scheduler_view() {
        for case in 0..5 {
            let mut design = event_design();
            match case {
                0 => {
                    let instruction =
                        design.process_ir.processes[0].blocks[1].instructions[0].clone();
                    design.process_ir.processes[0].blocks[2]
                        .instructions
                        .push(instruction);
                }
                1 => {
                    design.process_ir.processes[0].blocks[0].terminator =
                        ProcessTerminator::Goto(ProcessBlockId(1));
                }
                2 => {
                    design.process_ir.values[1].kind = ProcessValueKind::Unary {
                        operation: ProcessUnaryOp::Not,
                        operand: ProcessValueId(1),
                    };
                }
                3 => {
                    design.process_ir.processes[0].blocks[1].terminator =
                        ProcessTerminator::Goto(ProcessBlockId(1));
                }
                4 => {
                    let ProcessInstruction::Assign { driver_context, .. } =
                        &mut design.process_ir.processes[0].blocks[1].instructions[0]
                    else {
                        unreachable!()
                    };
                    *driver_context = Some(11);
                }
                _ => unreachable!(),
            }

            assert!(derive_scheduler_forms(&mut design).is_err(), "case {case}");
            let [original] = design.drivers.as_slice() else {
                panic!("case {case} changed the old scheduler view");
            };
            assert_eq!(original.ctx, 77);
            assert!(design.event_blocks.is_empty());
        }
    }

    #[test]
    fn integer_to_real_conversion_survives_scheduler_projection() {
        let mut ir = ProcessIr::default();
        let expression = Expr::Unary {
            op: UnOp::IntToReal,
            rhs: Box::new(Expr::Const(42)),
        };
        let id = ir.import_test_fragment(&expression, Span::new(FileId(0), 0..1));
        let projected = digital_expr(&ir, id).expect("integer-to-real is a normalized opcode");
        assert!(matches!(projected, Expr::Canonical { value, .. } if value == id));
        assert!(matches!(
            ir.values[id.0 as usize].kind,
            ProcessValueKind::Unary {
                operation: ProcessUnaryOp::IntegerToReal,
                ..
            }
        ));
    }

    #[test]
    fn shared_value_dag_is_retained_not_expanded_by_scheduler_projection() {
        let mut design = event_design();
        let process = &mut design.process_ir.processes[0];
        process.region = ProcessRegion::Combinational;
        process.entry = ProcessBlockId(1);
        let mut previous = ProcessValueId(1);
        for _ in 0..50_000 {
            let id = ProcessValueId(design.process_ir.values.len() as u32);
            design.process_ir.values.push(ProcessValue {
                span: process.span,
                ty: None,
                bit_width: Some(1),
                kind: ProcessValueKind::Binary {
                    operation: ProcessBinaryOp::And,
                    left: previous,
                    right: previous,
                },
            });
            previous = id;
        }
        let ProcessInstruction::Assign { value, .. } = &mut process.blocks[1].instructions[0]
        else {
            unreachable!()
        };
        *value = previous;
        derive_scheduler_forms(&mut design).expect("shared graph is derivable");
        let [driver] = design.drivers.as_slice() else {
            panic!("one root reference")
        };
        assert!(matches!(&driver.expr, Expr::Canonical { value, reads }
            if *value == previous && reads.as_ref() == [SignalId(1)]));
        assert!(design.validate().is_empty(), "{:?}", design.validate());
        let Expr::Canonical { reads, .. } = &mut design.drivers[0].expr else {
            unreachable!()
        };
        *reads = std::sync::Arc::from([]);
        assert!(design
            .validate()
            .iter()
            .any(|issue| issue.contains("stale sensitivity")));
    }
}
