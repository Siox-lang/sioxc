//! Import the normalized digital scheduler decomposition into Process IR.
//!
//! This is the temporary hardware-side bridge during the Process-first
//! migration. It belongs to IR lowering: it consumes only elaborated hierarchy
//! data and canonical digital IR, never typed testbench syntax or a `TestPlan`.

use super::*;

/// One elaborated instance together with the root and path that own its
/// flattened signals.
struct InstanceLocation {
    id: crate::elab::InstanceId,
    root: crate::elab::InstanceId,
    path: String,
}

/// Convert the normalized hardware scheduler decomposition into ordinary
/// Process IR CFGs.
///
/// Keep invocation after test processes are lowered until Process IDs no
/// longer encode scheduler order. Moving ownership here must not reorder the
/// executable process table.
pub(crate) fn import_hardware_processes(
    hierarchy: &Hierarchy,
    design: &Design,
    process_ir: &mut ProcessIr,
) {
    let locations = hierarchy_locations(hierarchy);
    for scheduled in design.processes() {
        let primary = match &scheduled.kind {
            ProcessKind::Comb { target, .. } => Some(*target),
            ProcessKind::Event { block } => design
                .event_blocks
                .get(*block)
                .and_then(|event| event.updates.first())
                .map(|update| update.target)
                .or_else(|| scheduled.reads.first().copied()),
        };
        let Some(primary) = primary else {
            continue;
        };
        let Some(location) =
            hardware_process_location(primary, &scheduled.reads, design, &locations)
        else {
            continue;
        };
        let id = ProcessId(process_ir.processes.len() as u32);
        let span = hardware_process_span(&scheduled.kind, primary, design);
        let label = if scheduled.labels.is_empty() {
            Some(format!(
                "{}::<hardware:{}>",
                location.path, design.signals[primary.0 as usize].path
            ))
        } else {
            Some(scheduled.labels.join(" + "))
        };
        let activation = ProcessActivation::Reactive {
            sensitivity: scheduled
                .reads
                .iter()
                .copied()
                .map(ProcessSensitivity::Signal)
                .collect(),
        };
        let mut process = ProcessCfg {
            id,
            root: location.root,
            owner: location.id,
            label,
            span,
            activation,
            entry: ProcessBlockId(0),
            locals: Vec::new(),
            blocks: vec![ProcessBlock::empty(ProcessBlockId(0))],
        };
        match scheduled.kind {
            ProcessKind::Comb { drivers, .. } => {
                let mut tail = ProcessBlockId(0);
                for driver in drivers {
                    let Some(driver) = design.drivers.get(driver) else {
                        continue;
                    };
                    let assignment_span = driver.span.unwrap_or(span);
                    tail = append_digital_assignment(
                        process_ir,
                        &mut process,
                        tail,
                        design,
                        ImportedAssignment {
                            signal: driver.target,
                            expression: &driver.expr,
                            condition: driver.cond.as_ref(),
                            driver_context: driver.ctx,
                            span: assignment_span,
                        },
                    );
                }
            }
            ProcessKind::Event { block } => {
                let Some(event) = design.event_blocks.get(block) else {
                    continue;
                };
                let body = process.push_block();
                let exit = process.push_block();
                let condition = push_normalized_value(process_ir, &event.condition, span, design);
                process.blocks[0].terminator = ProcessTerminator::Branch {
                    condition,
                    then_block: body,
                    else_block: exit,
                };
                let mut tail = body;
                for update in &event.updates {
                    tail = append_digital_assignment(
                        process_ir,
                        &mut process,
                        tail,
                        design,
                        ImportedAssignment {
                            signal: update.target,
                            expression: &update.expr,
                            condition: update.cond.as_ref(),
                            driver_context: event.ctx,
                            span: update.span.unwrap_or(span),
                        },
                    );
                }
                process.blocks[tail.0 as usize].terminator = ProcessTerminator::Goto(exit);
            }
        }
        process_ir.processes.push(process);
        if let Some(test) = process_ir
            .tests
            .iter_mut()
            .find(|test| test.root == location.root)
        {
            test.processes.push(id);
        }
    }
}

/// One assignment imported from the normalized digital scheduler product.
struct ImportedAssignment<'a> {
    signal: SignalId,
    expression: &'a Expr,
    condition: Option<&'a Expr>,
    driver_context: u32,
    span: crate::diag::Span,
}

/// Append one normalized signal assignment, spelling a guard as an explicit
/// branch so the resulting CFG needs no special conditional-write operation.
fn append_digital_assignment(
    process_ir: &mut ProcessIr,
    process: &mut ProcessCfg,
    tail: ProcessBlockId,
    design: &Design,
    assignment: ImportedAssignment<'_>,
) -> ProcessBlockId {
    let ImportedAssignment {
        signal,
        expression,
        condition,
        driver_context,
        span,
    } = assignment;
    let (assignment, next) = if let Some(condition) = condition {
        let assignment = process.push_block();
        let next = process.push_block();
        let condition = push_normalized_value(process_ir, condition, span, design);
        process.blocks[tail.0 as usize].terminator = ProcessTerminator::Branch {
            condition,
            then_block: assignment,
            else_block: next,
        };
        (assignment, Some(next))
    } else {
        (tail, None)
    };
    let target = ProcessValueId(process_ir.values.len() as u32);
    process_ir.values.push(ProcessValue {
        span,
        ty: None,
        bit_width: design.signal_width(signal),
        kind: ProcessValueKind::Signal {
            signals: vec![signal],
            state: ProcessSignalState::Current,
        },
    });
    if !process_ir.value_layouts.is_empty() {
        process_ir.value_layouts.push(None);
    }
    let value = push_normalized_value(process_ir, expression, span, design);
    process.blocks[assignment.0 as usize]
        .instructions
        .push(ProcessInstruction::Assign {
            semantics: ProcessAssignment::StagedSignal,
            driver_context: Some(driver_context),
            target,
            value,
            span,
        });
    if let Some(next) = next {
        process.blocks[assignment.0 as usize].terminator = ProcessTerminator::Goto(next);
        next
    } else {
        assignment
    }
}

/// Append one already-normalized digital expression and annotate every new
/// arena node with its natural packed width.
fn push_normalized_value(
    process_ir: &mut ProcessIr,
    expression: &Expr,
    span: crate::diag::Span,
    design: &Design,
) -> ProcessValueId {
    let first = process_ir.values.len();
    let value = process_ir.push_digital_expr(expression, span);
    for index in first..process_ir.values.len() {
        let width = normalized_value_width(process_ir, ProcessValueId(index as u32), design);
        process_ir.values[index].bit_width = width;
    }
    value
}

/// Natural width of one dependency-ordered normalized value.
fn normalized_value_width(
    process_ir: &ProcessIr,
    id: ProcessValueId,
    design: &Design,
) -> Option<u32> {
    let value = process_ir.values.get(id.0 as usize)?;
    let width = |id: &ProcessValueId| process_ir.values.get(id.0 as usize)?.bit_width;
    let signal_width = |signals: &[SignalId]| {
        signals.iter().try_fold(0u32, |total, signal| {
            total.checked_add(design.signal_width(*signal)?)
        })
    };
    let width = match &value.kind {
        ProcessValueKind::Number(ProcessNumber::Integer(words)) => integer_words_width(words),
        ProcessValueKind::Number(ProcessNumber::Real(_))
        | ProcessValueKind::ForeignCall { .. }
        | ProcessValueKind::HostCall { .. } => Some(64),
        ProcessValueKind::BitString { width, .. } => Some(*width),
        ProcessValueKind::Char(_) => Some(1),
        ProcessValueKind::Signal {
            state: ProcessSignalState::Event,
            ..
        }
        | ProcessValueKind::StorageState {
            state: ProcessSignalState::Event,
            ..
        } => Some(1),
        ProcessValueKind::Signal { signals, .. } => signal_width(signals),
        ProcessValueKind::BitSlice { high, low, .. } => high.checked_sub(*low)?.checked_add(1),
        ProcessValueKind::PackedSlice { left, right, .. } => left
            .abs_diff(*right)
            .checked_add(1)
            .and_then(|width| u32::try_from(width).ok()),
        ProcessValueKind::CheckedIndex { index, .. } => width(index),
        ProcessValueKind::TableLookup { table, .. } => design
            .lookup_tables
            .get(table.0)
            .map(|table| table.element_width),
        ProcessValueKind::Unary { operation, operand } => match operation {
            ProcessUnaryOp::RealToInteger => Some(64),
            ProcessUnaryOp::Neg | ProcessUnaryOp::Not => width(operand),
        },
        ProcessValueKind::RawResize { operand } => width(operand),
        ProcessValueKind::Binary {
            operation,
            left,
            right,
        } => match operation {
            ProcessBinaryOp::Eq
            | ProcessBinaryOp::Ne
            | ProcessBinaryOp::Lt
            | ProcessBinaryOp::Le
            | ProcessBinaryOp::Gt
            | ProcessBinaryOp::Ge
            | ProcessBinaryOp::SignedLt
            | ProcessBinaryOp::SignedLe
            | ProcessBinaryOp::SignedGt
            | ProcessBinaryOp::SignedGe
            | ProcessBinaryOp::FloatEq
            | ProcessBinaryOp::FloatNe
            | ProcessBinaryOp::FloatLt
            | ProcessBinaryOp::FloatLe
            | ProcessBinaryOp::FloatGt
            | ProcessBinaryOp::FloatGe => Some(1),
            ProcessBinaryOp::FloatAdd
            | ProcessBinaryOp::FloatSub
            | ProcessBinaryOp::FloatMul
            | ProcessBinaryOp::FloatDiv => Some(64),
            ProcessBinaryOp::Shl => shifted_arena_width(width(left)?, *right, &process_ir.values),
            _ => Some(width(left)?.max(width(right)?)),
        },
        ProcessValueKind::Select {
            then_value,
            else_value,
            ..
        } => Some(width(then_value)?.max(width(else_value)?)),
        ProcessValueKind::MetaCompare { .. } => Some(1),
        ProcessValueKind::Concat(values) => values
            .iter()
            .try_fold(0u32, |total, value| total.checked_add(width(value)?)),
        ProcessValueKind::Suffixed { .. }
        | ProcessValueKind::String(_)
        | ProcessValueKind::Local { .. }
        | ProcessValueKind::Storage(_)
        | ProcessValueKind::StorageState { .. }
        | ProcessValueKind::Definition(_)
        | ProcessValueKind::Intrinsic(_)
        | ProcessValueKind::Default
        | ProcessValueKind::Field { .. }
        | ProcessValueKind::Attribute { .. }
        | ProcessValueKind::Index { .. }
        | ProcessValueKind::Range { .. }
        | ProcessValueKind::Match { .. }
        | ProcessValueKind::Call { .. }
        | ProcessValueKind::Construct { .. }
        | ProcessValueKind::Array(_)
        | ProcessValueKind::Invalid => None,
    };
    width.filter(|width| *width != 0)
}

fn hierarchy_locations(hierarchy: &Hierarchy) -> Vec<InstanceLocation> {
    fn visit(
        hierarchy: &Hierarchy,
        id: crate::elab::InstanceId,
        root: crate::elab::InstanceId,
        path: String,
        output: &mut Vec<InstanceLocation>,
    ) {
        output.push(InstanceLocation {
            id,
            root,
            path: path.clone(),
        });
        for &child in &hierarchy.instance(id).children {
            visit(
                hierarchy,
                child,
                root,
                format!("{path}.{}", hierarchy.instance(child).name),
                output,
            );
        }
    }

    let mut output = Vec::new();
    for &root in &hierarchy.roots {
        visit(
            hierarchy,
            root,
            root,
            hierarchy.root_path(root),
            &mut output,
        );
    }
    output
}

fn signal_location<'a>(
    signal: SignalId,
    design: &Design,
    locations: &'a [InstanceLocation],
) -> Option<&'a InstanceLocation> {
    let path = &design.signals.get(signal.0 as usize)?.path;
    locations
        .iter()
        .filter(|location| {
            path == &location.path
                || path
                    .strip_prefix(&location.path)
                    .is_some_and(|rest| rest.starts_with('.'))
        })
        .max_by_key(|location| location.path.len())
}

fn hardware_process_location<'a>(
    primary: SignalId,
    reads: &[SignalId],
    design: &Design,
    locations: &'a [InstanceLocation],
) -> Option<&'a InstanceLocation> {
    signal_location(primary, design, locations).or_else(|| {
        reads
            .iter()
            .find_map(|signal| signal_location(*signal, design, locations))
    })
}

fn hardware_process_span(
    kind: &ProcessKind,
    primary: SignalId,
    design: &Design,
) -> crate::diag::Span {
    match kind {
        ProcessKind::Comb { drivers, .. } => drivers
            .iter()
            .filter_map(|index| design.drivers.get(*index)?.span)
            .next(),
        ProcessKind::Event { block } => design
            .event_blocks
            .get(*block)
            .and_then(|event| event.updates.iter().find_map(|update| update.span)),
    }
    .unwrap_or(design.signals[primary.0 as usize].declaration_span)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::FileId;

    #[test]
    fn event_process_values_are_one_bit() {
        let span = crate::diag::Span::new(FileId(0), 0..0);
        let process_ir = ProcessIr {
            values: vec![ProcessValue {
                span,
                ty: None,
                bit_width: None,
                kind: ProcessValueKind::Signal {
                    signals: vec![SignalId(0)],
                    state: ProcessSignalState::Event,
                },
            }],
            ..ProcessIr::default()
        };
        assert_eq!(
            normalized_value_width(&process_ir, ProcessValueId(0), &Design::default()),
            Some(1)
        );
    }

    #[test]
    fn normalized_shift_width_folds_integer_expression() {
        let span = crate::diag::Span::new(FileId(0), 0..0);
        let mut process_ir = ProcessIr::default();
        let expression = Expr::Binary {
            op: BinOp::Shl,
            lhs: Box::new(Expr::Const(1)),
            rhs: Box::new(Expr::Binary {
                op: BinOp::Sub,
                lhs: Box::new(Expr::Const(8)),
                rhs: Box::new(Expr::Const(1)),
            }),
        };

        let shifted = push_normalized_value(&mut process_ir, &expression, span, &Design::default());

        assert_eq!(process_ir.values[shifted.0 as usize].bit_width, Some(8));
    }

    #[test]
    fn internal_hardware_process_inherits_read_owner() {
        let span = crate::diag::Span::new(FileId(0), 0..1);
        let signal = |path: &str| Signal {
            path: path.to_string(),
            declaration_span: span,
            width: 1,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: vec![0],
            enum_type: None,
        };
        let design = Design {
            signals: vec![signal("$metatmp0"), signal("Bench.dut.input")],
            ..Design::default()
        };
        let locations = vec![
            InstanceLocation {
                id: crate::elab::InstanceId(0),
                root: crate::elab::InstanceId(0),
                path: "Bench".to_string(),
            },
            InstanceLocation {
                id: crate::elab::InstanceId(1),
                root: crate::elab::InstanceId(0),
                path: "Bench.dut".to_string(),
            },
        ];

        let location = hardware_process_location(SignalId(0), &[SignalId(1)], &design, &locations)
            .expect("internal helper should inherit an owner from its read set");
        assert_eq!(location.id, crate::elab::InstanceId(1));
        assert_eq!(location.root, crate::elab::InstanceId(0));
    }
}
