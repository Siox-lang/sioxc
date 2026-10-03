//! Canonical CFG construction for normalized source hardware writes.
//!
//! This is part of source lowering, before any public scheduler decomposition.
//! The private normalization draft is consumed once; only canonical Process IR
//! and its derived backend views leave the lowering pass.

use super::*;

/// One elaborated instance together with the root and path that own its
/// flattened signals.
struct InstanceLocation {
    id: crate::elab::InstanceId,
    root: crate::elab::InstanceId,
    path: String,
}

/// Finish source hardware into canonical CFGs. A source context retains all
/// of its combinational writes, including multiple targets and companion
/// planes; it is not split by the compatibility scheduler's target grouping.
pub(super) fn lower(
    hierarchy: &Hierarchy,
    design: &Design,
    draft: &HardwareDraft,
    sink: &mut DiagnosticSink,
) -> ProcessIr {
    let locations = hierarchy_locations(hierarchy);
    let mut ir = ProcessIr::default();

    // First-seen source context order, with source-order writes inside it.
    let mut contexts = Vec::<Vec<&Driver>>::new();
    let mut by_context = HashMap::new();
    for driver in &draft.drivers {
        let index = *by_context.entry(driver.ctx).or_insert_with(|| {
            contexts.push(Vec::new());
            contexts.len() - 1
        });
        contexts[index].push(driver);
    }
    for writes in contexts {
        // Hoisted implementation signals can precede the source target. Use
        // any owned target in this context before falling back to read-owner
        // inference; otherwise a constant helper could hide the whole CFG.
        let primary = writes
            .iter()
            .map(|write| write.target)
            .find(|target| signal_location(*target, design, &locations).is_some())
            .unwrap_or(writes[0].target);
        let span = writes
            .iter()
            .find_map(|write| write.span)
            .unwrap_or(design.signals[primary.0 as usize].declaration_span);
        let mut reads = Vec::new();
        let mut labels = Vec::new();
        for write in &writes {
            if let Some(condition) = &write.cond {
                read_set(condition, &mut reads);
            }
            read_set(&write.expr, &mut reads);
            if let Some(label) = design.process_labels.get(&write.ctx) {
                labels.push(label.clone());
            }
            if let Some(resolved) = design.resolved_process_labels.get(&write.target.0) {
                labels.extend(resolved.iter().cloned());
            }
        }
        labels.sort();
        labels.dedup();
        let Some(mut process) = new_process(
            &ir,
            primary,
            reads,
            labels,
            span,
            design,
            &locations,
            draft.context_paths.get(&writes[0].ctx).map(String::as_str),
        ) else {
            report_missing_owner(span, sink);
            continue;
        };
        process.region = ProcessRegion::Combinational;
        let mut tail = process.entry;
        for write in writes {
            tail = append_digital_assignment(
                &mut ir,
                &mut process,
                tail,
                design,
                SourceAssignment {
                    signal: write.target,
                    expression: &write.expr,
                    condition: write.cond.as_ref(),
                    driver_context: write.ctx,
                    span: write.span.unwrap_or(span),
                },
            );
        }
        ir.processes.push(process);
    }

    for event in &draft.event_blocks {
        let mut reads = Vec::new();
        read_set(&event.condition, &mut reads);
        for write in &event.updates {
            if let Some(condition) = &write.cond {
                read_set(condition, &mut reads);
            }
            read_set(&write.expr, &mut reads);
        }
        let Some(primary) = event
            .updates
            .first()
            .map(|write| write.target)
            .or_else(|| reads.first().copied())
        else {
            continue;
        };
        let span = event
            .updates
            .iter()
            .find_map(|write| write.span)
            .unwrap_or(design.signals[primary.0 as usize].declaration_span);
        let labels = design
            .process_labels
            .get(&event.ctx)
            .cloned()
            .into_iter()
            .collect();
        let Some(mut process) = new_process(
            &ir,
            primary,
            reads,
            labels,
            span,
            design,
            &locations,
            draft.context_paths.get(&event.ctx).map(String::as_str),
        ) else {
            report_missing_owner(span, sink);
            continue;
        };
        let body = process.push_block();
        let exit = process.push_block();
        let condition = push_normalized_value(&mut ir, &event.condition, span, design);
        process.region = ProcessRegion::Event {
            condition,
            body,
            driver_context: event.ctx,
        };
        process.blocks[0].terminator = ProcessTerminator::Branch {
            condition,
            then_block: body,
            else_block: exit,
        };
        let mut tail = body;
        for write in &event.updates {
            tail = append_digital_assignment(
                &mut ir,
                &mut process,
                tail,
                design,
                SourceAssignment {
                    signal: write.target,
                    expression: &write.expr,
                    condition: write.cond.as_ref(),
                    driver_context: event.ctx,
                    span: write.span.unwrap_or(span),
                },
            );
        }
        process.blocks[tail.0 as usize].terminator = ProcessTerminator::Goto(exit);
        ir.processes.push(process);
    }
    ir
}

fn report_missing_owner(span: crate::diag::Span, sink: &mut DiagnosticSink) {
    sink.emit(
        crate::diag::Diagnostic::error(
            "cannot determine the owning instance for a source hardware context",
        )
        .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
        .at(span),
    );
}

#[allow(clippy::too_many_arguments)]
fn new_process(
    ir: &ProcessIr,
    primary: SignalId,
    mut reads: Vec<SignalId>,
    labels: Vec<String>,
    span: crate::diag::Span,
    design: &Design,
    locations: &[InstanceLocation],
    context_path: Option<&str>,
) -> Option<ProcessCfg> {
    let mut seen = HashSet::new();
    reads.retain(|id| seen.insert(*id));
    // A parent process can write a child input and its own output. Its owner
    // is the source container, not whichever target happens to be first.
    let location = locations
        .iter()
        .find(|location| Some(location.path.as_str()) == context_path)
        .or_else(|| hardware_process_location(primary, &reads, design, locations))?;
    let label = if labels.is_empty() {
        format!(
            "{}::<hardware:{}>",
            location.path, design.signals[primary.0 as usize].path
        )
    } else {
        labels.join(" + ")
    };
    Some(ProcessCfg {
        id: ProcessId(ir.processes.len() as u32),
        root: location.root,
        owner: location.id,
        label: Some(label),
        span,
        activation: ProcessActivation::Reactive {
            sensitivity: reads.into_iter().map(ProcessSensitivity::Signal).collect(),
        },
        region: ProcessRegion::Procedural,
        entry: ProcessBlockId(0),
        locals: Vec::new(),
        blocks: vec![ProcessBlock::empty(ProcessBlockId(0))],
    })
}

/// One representation-normalized source write.
struct SourceAssignment<'a> {
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
    assignment: SourceAssignment<'_>,
) -> ProcessBlockId {
    let SourceAssignment {
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
            ProcessUnaryOp::RealToInteger | ProcessUnaryOp::IntegerToReal => Some(64),
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

    #[test]
    fn constant_helper_keeps_its_context_owner_without_signal_reads() {
        let span = crate::diag::Span::new(FileId(0), 0..1);
        let design = Design {
            signals: vec![Signal {
                path: "$metatmp0".into(),
                declaration_span: span,
                width: 8,
                real: false,
                integer: false,
                char: false,
                range: None,
                init: vec![0],
                enum_type: None,
            }],
            ..Design::default()
        };
        let locations = vec![InstanceLocation {
            id: crate::elab::InstanceId(3),
            root: crate::elab::InstanceId(1),
            path: "Bench.dut".into(),
        }];
        let process = new_process(
            &ProcessIr::default(),
            SignalId(0),
            vec![],
            vec![],
            span,
            &design,
            &locations,
            Some("Bench.dut"),
        )
        .expect("source context owns a constant implementation helper");
        assert_eq!(process.owner, crate::elab::InstanceId(3));
        assert_eq!(process.root, crate::elab::InstanceId(1));
        assert!(
            matches!(process.activation, ProcessActivation::Reactive { ref sensitivity } if sensitivity.is_empty())
        );
    }
}
