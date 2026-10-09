//! Canonical CFG construction for normalized source hardware writes.
//!
//! This is part of source lowering, before any public scheduler decomposition.
//! The private normalization draft is consumed once; only canonical Process IR
//! and its derived backend views leave the lowering pass.

use super::source_values::SourceValues;
use super::*;

/// One elaborated instance together with the root and path that own its
/// flattened signals.
struct InstanceLocation {
    id: crate::elab::InstanceId,
    root: crate::elab::InstanceId,
    path: String,
}

/// Bind finalized write/guard roots before canonical representation passes.
/// CFG construction below accepts handles only, never imports source fragments.
pub(super) fn canonicalize_draft(
    hierarchy: &Hierarchy,
    design: &Design,
    draft: &mut HardwareDraft,
    values: &mut SourceValues,
) {
    fn bind(values: &mut SourceValues, expression: &mut Expr, span: crate::diag::Span) {
        let value = values.append(expression, span, None);
        *expression = values.reference(value);
    }
    let locations = hierarchy_locations(hierarchy);
    let spans: HashMap<_, _> = source_contexts(draft)
        .into_iter()
        .map(|writes| (writes[0].ctx, context_span(&writes, design, &locations)))
        .collect();
    for write in &mut draft.drivers {
        let span = write.span.unwrap_or(spans[&write.ctx]);
        if let Some(condition) = &mut write.cond {
            bind(values, condition, span);
        }
        bind(values, &mut write.expr, span);
    }
    for event in &mut draft.event_blocks {
        let mut reads = Vec::new();
        read_set(&event.condition, &mut reads);
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
        bind(values, &mut event.condition, span);
        for write in &mut event.updates {
            let span = write.span.unwrap_or(span);
            if let Some(condition) = &mut write.cond {
                bind(values, condition, span);
            }
            bind(values, &mut write.expr, span);
        }
    }
}

/// First-seen source contexts, retaining source-order writes inside each.
fn source_contexts(draft: &HardwareDraft) -> Vec<Vec<&Driver>> {
    let mut contexts = Vec::<Vec<&Driver>>::new();
    let mut by_context = HashMap::new();
    for driver in &draft.drivers {
        let index = *by_context.entry(driver.ctx).or_insert_with(|| {
            contexts.push(Vec::new());
            contexts.len() - 1
        });
        contexts[index].push(driver);
    }
    contexts
}

fn context_span(
    writes: &[&Driver],
    design: &Design,
    locations: &[InstanceLocation],
) -> crate::diag::Span {
    let primary = writes
        .iter()
        .map(|write| write.target)
        .find(|target| signal_location(*target, design, locations).is_some())
        .unwrap_or(writes[0].target);
    writes
        .iter()
        .find_map(|write| write.span)
        .unwrap_or(design.signals[primary.0 as usize].declaration_span)
}

/// Finish source hardware into canonical CFGs. A source context retains all
/// of its combinational writes, including multiple targets and companion
/// planes; it is not split by the compatibility scheduler's target grouping.
pub(super) fn lower(
    hierarchy: &Hierarchy,
    design: &Design,
    draft: &HardwareDraft,
    mut ir: ProcessIr,
    sink: &mut DiagnosticSink,
) -> ProcessIr {
    let locations = hierarchy_locations(hierarchy);
    for index in 0..ir.values.len() {
        ir.values[index].bit_width =
            normalized_value_width(&ir, ProcessValueId(index as u32), design);
    }

    for writes in source_contexts(draft) {
        // Hoisted implementation signals can precede the source target. Use
        // any owned target in this context before falling back to read-owner
        // inference; otherwise a constant helper could hide the whole CFG.
        let primary = writes
            .iter()
            .map(|write| write.target)
            .find(|target| signal_location(*target, design, &locations).is_some())
            .unwrap_or(writes[0].target);
        let span = context_span(&writes, design, &locations);
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
        let condition = canonical_value(&event.condition);
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
        let condition = canonical_value(condition);
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
    let value = canonical_value(expression);
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

fn canonical_value(expression: &Expr) -> ProcessValueId {
    let Expr::Canonical { value, .. } = expression else {
        panic!("source hardware roots must be finalized before CFG construction");
    };
    *value
}

/// Natural width of one dependency-ordered normalized value.
fn normalized_value_width(
    process_ir: &ProcessIr,
    id: ProcessValueId,
    design: &Design,
) -> Option<u32> {
    let value = process_ir.values.get(id.0 as usize)?;
    if matches!(value.kind, ProcessValueKind::RawResize { .. }) {
        if let Some(width) = value.bit_width {
            return (width != 0).then_some(width);
        }
    }
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
        ProcessValueKind::Parameter { .. } => value.bit_width,
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
            | ProcessBinaryOp::FloatDiv
            | ProcessBinaryOp::FloatRem => Some(64),
            // Raw kernel multiplication is evaluated before a consuming
            // slice, shift, or assignment selects the result's bits. Keeping
            // only max(lhs, rhs) here would drop fixed-point fraction bits
            // before the source-defined operator shifts them into place.
            ProcessBinaryOp::Mul | ProcessBinaryOp::SignedMul => {
                width(left)?.checked_add(width(right)?)
            }
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
    let width = match (&value.ty, width) {
        (Some(crate::types::Ty::Integer | crate::types::Ty::Real), Some(width)) => {
            Some(width.max(64))
        }
        (_, width) => width,
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
    fn cfg_construction_reuses_finalized_roots_without_importing_values() {
        let declaration = crate::diag::Span::new(FileId(0), 0..4);
        let assignment_span = crate::diag::Span::new(FileId(0), 10..20);
        let signal = |name: &str| Signal {
            path: format!("Bench.{name}"),
            declaration_span: declaration,
            width: 8,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: vec![0],
            enum_type: None,
        };
        let design = Design {
            signals: vec![signal("input"), signal("left"), signal("right")],
            ..Design::default()
        };
        let mut hierarchy = Hierarchy::default();
        hierarchy.roots.push(crate::elab::InstanceId(0));
        hierarchy.instances.push(crate::elab::Instance {
            name: "Bench".into(),
            entity: "Bench".into(),
            entity_id: DefId(0),
            attrs: vec![],
            params: vec![],
            connections: vec![],
            instance_arrays: vec![],
            children: vec![],
            is_extern: false,
        });
        let mut values = SourceValues::default();
        let shared = values.import_test_fragment(
            &Expr::CCall {
                name: "labs".into(),
                args: vec![Expr::Current(SignalId(0))],
                f64_args: vec![false],
                integer_args: vec![true],
                f64_ret: false,
                integer_ret: true,
            },
            assignment_span,
            None,
        );
        let root = Expr::Binary {
            op: BinOp::Add,
            lhs: Box::new(values.reference(shared)),
            rhs: Box::new(Expr::Const(1)),
        };
        let root = values.import_test_fragment(&root, assignment_span, None);
        let root = values.reference(root);
        let mut draft = HardwareDraft::default();
        for target in [SignalId(1), SignalId(2)] {
            draft.drivers.push(Driver {
                target,
                cond: Some(Expr::Current(SignalId(0))),
                expr: root.clone(),
                meta: None,
                ctx: 7,
                span: Some(assignment_span),
            });
        }
        draft.event_blocks.push(EventBlock {
            condition: Expr::Event(SignalId(0)),
            ctx: 8,
            updates: vec![NextUpdate {
                target: SignalId(1),
                cond: Some(Expr::Const(1)),
                expr: Expr::Old(SignalId(0)),
                meta: None,
                span: None,
            }],
        });
        canonicalize_draft(&hierarchy, &design, &mut draft, &mut values);
        values.retain_reachable(&mut draft);
        let left = canonical_value(&draft.drivers[0].expr);
        let right = canonical_value(&draft.drivers[1].expr);
        for id in [left, right] {
            assert_eq!(values.ir.values[id.0 as usize].span, assignment_span);
            assert!(matches!(values.ir.values[id.0 as usize].kind,
                ProcessValueKind::Binary { left, .. } if left == shared));
        }
        let event = canonical_value(&draft.event_blocks[0].condition);
        assert_eq!(values.ir.values[event.0 as usize].span, declaration);
        let count = values.ir.values.len();
        let mut sink = DiagnosticSink::new();
        let ir = lower(&hierarchy, &design, &draft, values.ir, &mut sink);
        assert!(!sink.has_errors());
        assert_eq!(ir.processes.len(), 2);
        assert_eq!(
            ir.values.len(),
            count + 3,
            "only assignment targets are new"
        );
        assert!(ir.values[count..]
            .iter()
            .all(|value| matches!(value.kind, ProcessValueKind::Signal { .. })));
        assert_eq!(
            ir.values
                .iter()
                .filter(|value| matches!(value.kind, ProcessValueKind::ForeignCall { .. }))
                .count(),
            1
        );
        assert_eq!(ir.values[left.0 as usize].span, assignment_span);
        assert_eq!(ir.values[right.0 as usize].span, assignment_span);
        assert!(matches!(ir.processes[0].activation,
            ProcessActivation::Reactive { ref sensitivity }
                if sensitivity == &[ProcessSensitivity::Signal(SignalId(0))]));
        assert!(matches!(ir.processes[1].region,
            ProcessRegion::Event { condition, .. } if condition == event));
        assert!(ir.validate(3).is_empty(), "{:?}", ir.validate(3));
    }

    fn normalized_test_value(
        expression: &Expr,
        span: crate::diag::Span,
    ) -> (ProcessIr, ProcessValueId) {
        let mut values = SourceValues::default();
        let value = values.import_test_fragment(expression, span, None);
        let mut ir = values.ir;
        for index in 0..ir.values.len() {
            ir.values[index].bit_width =
                normalized_value_width(&ir, ProcessValueId(index as u32), &Design::default());
        }
        (ir, value)
    }

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
        let expression = Expr::Binary {
            op: BinOp::Shl,
            lhs: Box::new(Expr::Const(1)),
            rhs: Box::new(Expr::Binary {
                op: BinOp::Sub,
                lhs: Box::new(Expr::Const(8)),
                rhs: Box::new(Expr::Const(1)),
            }),
        };

        let (process_ir, shifted) = normalized_test_value(&expression, span);

        assert_eq!(process_ir.values[shifted.0 as usize].bit_width, Some(8));
    }

    #[test]
    fn a_fraction_shift_keeps_the_full_raw_product() {
        let span = crate::diag::Span::new(FileId(0), 0..1);
        let expression = Expr::Binary {
            op: BinOp::Shr,
            lhs: Box::new(Expr::Binary {
                op: BinOp::Mul,
                lhs: Box::new(Expr::Const(40)),
                rhs: Box::new(Expr::Const(24)),
            }),
            rhs: Box::new(Expr::Const(4)),
        };
        let (process_ir, shifted) = normalized_test_value(&expression, span);
        // 40 needs six bits, 24 five. Their full product must survive until
        // the consumer shifts it: (40 * 24) >> 4 == 60, not 12 or 4.
        assert_eq!(process_ir.values[shifted.0 as usize].bit_width, Some(11));
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
