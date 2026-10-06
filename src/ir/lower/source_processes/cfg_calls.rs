//! Expand remaining value calls inside the canonical Process CFG. Expression
//! values stay arena-owned; no AST substitution or backend call interpreter.

use super::*;
use std::collections::HashMap;

pub(super) fn lower_value_calls(process: &mut ProcessCfg, context: &mut LoweringContext<'_>) {
    // Inlined bodies normalize their own new blocks while their recursion
    // guard is active; do not revisit them as separate call sites.
    let blocks = process.blocks.len();
    for block in 0..blocks {
        normalize_block(ProcessBlockId(block as u32), process, context);
    }
}

/// Loop iterable evaluation belongs in the preheader, not on its back-edge.
/// Expand calls before constructing the dedicated iteration header so the
/// runtime's once-per-entry iterable snapshot remains the only loop snapshot.
pub(super) fn lower_loop_iterable(
    value: ProcessValueId,
    block: ProcessBlockId,
    process: &mut ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> (ProcessBlockId, ProcessValueId) {
    if !has_call(value, context) {
        return (block, value);
    }
    let original = process.blocks[block.0 as usize].clone();
    let first_block = process.blocks.len();
    let first_local = process.locals.len();
    let first_value = context.process_ir.values.len();
    let mut tail = block;
    if let Some(value) = normalize_value(
        value,
        false,
        &mut tail,
        process,
        context,
        &mut HashMap::new(),
    ) {
        return (tail, value);
    }
    process.blocks.truncate(first_block);
    process.locals.truncate(first_local);
    process.blocks[block.0 as usize] = original;
    truncate_process_values(context, first_value);
    (block, value)
}

fn has_call(value: ProcessValueId, context: &mut LoweringContext<'_>) -> bool {
    while context.cfg_call_cache.len() < context.process_ir.values.len() {
        let node = &context.process_ir.values[context.cfg_call_cache.len()];
        let contains = matches!(
            node.kind,
            ProcessValueKind::Call { .. } | ProcessValueKind::Invalid
        ) || crate::ir::process_value_dependencies(&node.kind)
            .iter()
            .any(|dependency| context.cfg_call_cache[dependency.0 as usize]);
        context.cfg_call_cache.push(contains);
    }
    context.cfg_call_cache[value.0 as usize]
}

fn normalize_block(
    block: ProcessBlockId,
    process: &mut ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> bool {
    let needed = process.blocks[block.0 as usize]
        .instructions
        .iter()
        .any(|instruction| match instruction {
            ProcessInstruction::Declare { initializer, .. } => {
                initializer.is_some_and(|value| has_call(value, context))
            }
            ProcessInstruction::Assign { target, value, .. } => {
                has_call(*target, context) || has_call(*value, context)
            }
            ProcessInstruction::Schedule {
                target,
                value,
                delay,
                ..
            } => {
                has_call(*target, context) || has_call(*value, context) || has_call(*delay, context)
            }
            ProcessInstruction::Runtime { arguments, .. } => {
                arguments.iter().any(|value| has_call(*value, context))
            }
        })
        || match &process.blocks[block.0 as usize].terminator {
            ProcessTerminator::Return { value, .. } => {
                value.is_some_and(|value| has_call(value, context))
            }
            ProcessTerminator::Branch { condition, .. } => has_call(*condition, context),
            ProcessTerminator::Match { scrutinee, .. } => has_call(*scrutinee, context),
            ProcessTerminator::For { iterable, .. } => has_call(*iterable, context),
            ProcessTerminator::Suspend { arguments, .. } => {
                arguments.iter().any(|value| has_call(*value, context))
            }
            _ => false,
        };
    if !needed {
        return true;
    }
    let original = process.blocks[block.0 as usize].clone();
    let first_block = process.blocks.len();
    let first_local = process.locals.len();
    let first_value = context.process_ir.values.len();
    process.blocks[block.0 as usize] = ProcessBlock::empty(block);
    let result = normalize_contents(&original, process, context);
    if result.is_none() {
        process.blocks.truncate(first_block);
        process.locals.truncate(first_local);
        process.blocks[block.0 as usize] = original;
        truncate_process_values(context, first_value);
        return false;
    }
    true
}

fn normalize_contents(
    original: &ProcessBlock,
    process: &mut ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<()> {
    let mut block = original.id;
    for mut instruction in original.instructions.clone() {
        let mut memo = HashMap::new();
        let delayed_call = match &instruction {
            ProcessInstruction::Schedule { delay, .. } => has_call(*delay, context),
            _ => false,
        };
        match &mut instruction {
            ProcessInstruction::Declare { initializer, .. } => {
                if let Some(value) = initializer {
                    *value =
                        normalize_value(*value, false, &mut block, process, context, &mut memo)?;
                }
            }
            ProcessInstruction::Assign { target, value, .. }
            | ProcessInstruction::Schedule { target, value, .. } => {
                *target = normalize_value(*target, true, &mut block, process, context, &mut memo)?;
                if has_call(*value, context) || delayed_call {
                    *target = capture_process_argument(*target, context, process, block);
                }
                *value = normalize_value(*value, false, &mut block, process, context, &mut memo)?;
                if let ProcessInstruction::Schedule { delay, value, .. } = &mut instruction {
                    if delayed_call {
                        *value = capture_process_operand(*value, context, process, block);
                    }
                    *delay =
                        normalize_value(*delay, false, &mut block, process, context, &mut memo)?;
                }
            }
            ProcessInstruction::Runtime {
                operation,
                arguments,
                format,
                ..
            } => {
                for index in 0..arguments.len() {
                    let old = arguments[index];
                    let mut value =
                        normalize_value(old, false, &mut block, process, context, &mut memo)?;
                    if arguments[index + 1..]
                        .iter()
                        .any(|value| has_call(*value, context))
                    {
                        value = capture_process_operand(value, context, process, block);
                    }
                    arguments[index] = value;
                }
                let first_formatted = match operation {
                    ProcessRuntimeOp::Print => 1,
                    ProcessRuntimeOp::Assert | ProcessRuntimeOp::Warn => 2,
                    _ => arguments.len(),
                };
                let mut formatted = arguments.iter().skip(first_formatted);
                for part in format.iter_mut().flatten() {
                    if let ProcessFormatPart::Value { value, .. } = part {
                        *value = *formatted.next()?;
                    }
                }
            }
        }
        process.blocks[block.0 as usize]
            .instructions
            .push(instruction);
    }
    let mut terminator = original.terminator.clone();
    let mut memo = HashMap::new();
    match &mut terminator {
        ProcessTerminator::Return {
            value: Some(value), ..
        }
        | ProcessTerminator::Branch {
            condition: value, ..
        }
        | ProcessTerminator::Match {
            scrutinee: value, ..
        }
        | ProcessTerminator::For {
            iterable: value, ..
        } => {
            *value = normalize_value(*value, false, &mut block, process, context, &mut memo)?;
        }
        ProcessTerminator::Suspend { arguments, .. } => {
            for value in arguments {
                *value = normalize_value(*value, false, &mut block, process, context, &mut memo)?;
            }
        }
        _ => {}
    }
    process.blocks[block.0 as usize].terminator = terminator;
    Some(())
}

fn normalize_value(
    value: ProcessValueId,
    place: bool,
    block: &mut ProcessBlockId,
    process: &mut ProcessCfg,
    context: &mut LoweringContext<'_>,
    memo: &mut HashMap<ProcessValueId, ProcessValueId>,
) -> Option<ProcessValueId> {
    if let Some(value) = memo.get(&value) {
        return Some(*value);
    }
    if !has_call(value, context) {
        return Some(value);
    }
    let node = context.process_ir.values[value.0 as usize].clone();
    let result = match &node.kind {
        ProcessValueKind::Call {
            callee,
            arguments,
            bang: false,
            type_arguments,
        } if type_arguments.is_empty() => {
            inline_call(value, *callee, arguments, block, process, context, memo)?
        }
        ProcessValueKind::Call { .. } | ProcessValueKind::Invalid => return None,
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        } => {
            let condition = normalize_value(*condition, false, block, process, context, memo)?;
            let result = result_local(&node, value, process, context);
            declare_result(result, process, *block, context);
            let then_block = process.push_block();
            let else_block = process.push_block();
            let join = process.push_block();
            process.blocks[block.0 as usize].terminator = ProcessTerminator::Branch {
                condition,
                then_block,
                else_block,
            };
            for (mut tail, operand) in [(then_block, *then_value), (else_block, *else_value)] {
                let selected = normalize_value(
                    operand,
                    false,
                    &mut tail,
                    process,
                    context,
                    &mut memo.clone(),
                )?;
                assign_result(result, selected, process, tail, context);
                process.blocks[tail.0 as usize].terminator = ProcessTerminator::Goto(join);
            }
            *block = join;
            result
        }
        ProcessValueKind::Match { scrutinee, arms } => {
            let scrutinee = normalize_value(*scrutinee, false, block, process, context, memo)?;
            let result = result_local(&node, value, process, context);
            declare_result(result, process, *block, context);
            let join = process.push_block();
            let mut branches = Vec::new();
            for arm in arms {
                let entry = process.push_block();
                let mut tail = entry;
                let selected = normalize_value(
                    arm.value,
                    false,
                    &mut tail,
                    process,
                    context,
                    &mut memo.clone(),
                )?;
                assign_result(result, selected, process, tail, context);
                process.blocks[tail.0 as usize].terminator = ProcessTerminator::Goto(join);
                branches.push(ProcessMatchArm {
                    pattern: arm.pattern.clone(),
                    block: entry,
                    span: arm.span,
                });
            }
            process.blocks[block.0 as usize].terminator = ProcessTerminator::Match {
                scrutinee,
                arms: branches,
                fallback: None,
            };
            *block = join;
            result
        }
        _ => {
            let dependencies = crate::ir::process_value_dependencies(&node.kind);
            let mut operands = Vec::with_capacity(dependencies.len());
            for (index, operand) in dependencies.iter().copied().enumerate() {
                let preserve_place = place && index == 0;
                let mut lowered =
                    normalize_value(operand, preserve_place, block, process, context, memo)?;
                if !preserve_place
                    && dependencies[index + 1..]
                        .iter()
                        .any(|value| has_call(*value, context))
                {
                    lowered = capture_process_operand(lowered, context, process, *block);
                }
                operands.push(lowered);
                // Snapshots of mutable reads are evaluation boundaries, not
                // replacements for the underlying place. Shared parameter IDs
                // must remain aliases in a later call, and reads after that
                // call must observe its immediate writes.
                if has_call(operand, context) {
                    memo.insert(operand, lowered);
                }
            }
            let mut kind = node.kind;
            let mut operands = operands.into_iter();
            // The dependency visitor preserves written positions. Equal IDs
            // can read a mutable parameter both before and after a call; an
            // ID-keyed map would incorrectly collapse those distinct reads.
            crate::ir::remap_process_value_dependencies(&mut kind, |_| {
                operands.next().expect("matching canonical operand order")
            });
            let result = push_value(node.span, node.ty, node.bit_width, kind, context);
            context.process_ir.value_layouts[result.0 as usize] =
                context.process_ir.value_layouts[value.0 as usize].clone();
            result
        }
    };
    memo.insert(value, result);
    Some(result)
}

fn result_local(
    node: &ProcessValue,
    source: ProcessValueId,
    process: &mut ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    let local = ProcessLocalId(process.locals.len() as u32);
    let layout = process_value_source_layout(source, context.process_ir)
        .cloned()
        .or_else(|| {
            node.ty
                .as_ref()
                .and_then(|ty| process_layout_for_type(ty, node.span, context))
        });
    let width = node
        .bit_width
        .or_else(|| layout.as_ref().and_then(SourceLayout::packed_width));
    process.locals.push(ProcessLocal {
        id: local,
        name: format!("<return:{}>", local.0),
        source: None,
        span: node.span,
        ty: node.ty.clone(),
        layout: layout.clone(),
    });
    let result = push_value(
        node.span,
        node.ty.clone(),
        width,
        ProcessValueKind::Local {
            process: process.id,
            local,
        },
        context,
    );
    context.process_ir.value_layouts[result.0 as usize] = layout;
    result
}

fn declare_result(
    value: ProcessValueId,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
    context: &LoweringContext<'_>,
) {
    let ProcessValueKind::Local { local, .. } = context.process_ir.values[value.0 as usize].kind
    else {
        unreachable!()
    };
    process.blocks[block.0 as usize]
        .instructions
        .push(ProcessInstruction::Declare {
            local,
            initializer: None,
            span: context.process_ir.values[value.0 as usize].span,
        });
}

fn assign_result(
    target: ProcessValueId,
    value: ProcessValueId,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
    context: &LoweringContext<'_>,
) {
    process.blocks[block.0 as usize]
        .instructions
        .push(ProcessInstruction::Assign {
            semantics: ProcessAssignment::ImmediateLocal,
            driver_context: None,
            target,
            value,
            span: context.process_ir.values[target.0 as usize].span,
        });
}

fn inline_call(
    call: ProcessValueId,
    callee: ProcessValueId,
    arguments: &[ProcessValueId],
    block: &mut ProcessBlockId,
    process: &mut ProcessCfg,
    context: &mut LoweringContext<'_>,
    memo: &mut HashMap<ProcessValueId, ProcessValueId>,
) -> Option<ProcessValueId> {
    let (function, receiver) = match context.process_ir.values[callee.0 as usize].kind.clone() {
        ProcessValueKind::Definition(definition) => {
            (context.functions.get_definition(definition)?, None)
        }
        ProcessValueKind::Field { base, field } => {
            let owner =
                process_value_type(base, context).and_then(|ty| process_type_key(&ty, context))?;
            (
                context.functions.get_associated(&owner, &field)?,
                Some(base),
            )
        }
        ProcessValueKind::Intrinsic(name) => {
            let (owner, method) = name.rsplit_once("::")?;
            let owner = context.functions.canonical_type_key(owner);
            (context.functions.get_associated(&owner, method)?, None)
        }
        _ => return None,
    };
    let body = function.body.as_ref()?;
    function.ret.as_ref()?;
    if context.inline_functions.contains(&function.span) {
        return None;
    }
    let parameters = function
        .params
        .iter()
        .filter(|parameter| !parameter.is_self)
        .collect::<Vec<_>>();
    if parameters.len() != arguments.len()
        || function.params.iter().any(|parameter| parameter.is_self) != receiver.is_some()
    {
        return None;
    }
    let receiver = match receiver {
        Some(receiver) => {
            let receiver = normalize_value(receiver, true, block, process, context, memo)?;
            Some(capture_process_argument(receiver, context, process, *block))
        }
        None => None,
    };
    let mut bindings = HashMap::new();
    for (parameter, argument) in parameters.iter().zip(arguments) {
        let definition = context.resolved.declared(parameter.name.as_ref()?.span)?;
        let argument = normalize_value(*argument, true, block, process, context, memo)?;
        let argument = capture_process_argument(argument, context, process, *block);
        bindings.insert(definition, argument);
    }
    let mut node = context.process_ir.values[call.0 as usize].clone();
    let generic_argument = generic_return_argument(function, arguments);
    node.ty = node
        .ty
        .filter(|ty| is_concrete_type(ty, context))
        .or_else(|| generic_argument.and_then(|value| process_value_type(value, context)))
        .or_else(|| {
            function
                .ret
                .as_ref()
                .and_then(|ty| process_declared_type(ty, context))
        });
    let result = result_local(&node, call, process, context);
    // `Ty` retains element count, not the source labels/direction. A return
    // frame must keep the function's declared shape, including nested ranges.
    let declared_layout = node.ty.as_ref().and_then(|ty| {
        let mut layout = process_layout_for_type(ty, node.span, context)?;
        apply_process_declared_ranges(
            &mut layout,
            function.ret.as_ref()?,
            context,
            &mut Default::default(),
        )?;
        Some(layout)
    });
    let inherited_layout = receiver
        .filter(|receiver| returns_receiver_type(function, *receiver, context))
        .or(generic_argument)
        .and_then(|receiver| process_value_source_layout(receiver, context.process_ir).cloned());
    if let Some(layout) = inherited_layout.or(declared_layout) {
        let ProcessValueKind::Local { local, .. } =
            context.process_ir.values[result.0 as usize].kind
        else {
            unreachable!()
        };
        process.locals[local.0 as usize].layout = Some(layout.clone());
        context.process_ir.values[result.0 as usize].bit_width = layout.packed_width();
        context.process_ir.value_layouts[result.0 as usize] = Some(layout);
    }
    declare_result(result, process, *block, context);
    let resume = process.push_block();
    let entry = process.push_block();
    process.blocks[block.0 as usize].terminator = ProcessTerminator::Goto(entry);
    context.inline_functions.insert(function.span);
    context.value_bindings.push(bindings);
    context.inline_self_values.push(receiver);
    context.inline_return_types.push(node.ty);
    context.inline_return_blocks.push((resume, Some(result)));
    let first_body_block = entry.0 as usize;
    let tail = lower_statements(&body.stmts, context, process, entry);
    let body_end = process.blocks.len();
    let mut supported = tail.is_none();
    for index in first_body_block..body_end {
        supported &= normalize_block(ProcessBlockId(index as u32), process, context);
        supported &= !process.blocks[index]
            .instructions
            .iter()
            .any(|instruction| {
                matches!(
                    instruction,
                    ProcessInstruction::Runtime {
                        operation: ProcessRuntimeOp::Call(_),
                        ..
                    }
                )
            });
    }
    context.inline_return_blocks.pop();
    context.inline_return_types.pop();
    context.inline_self_values.pop();
    context.value_bindings.pop();
    context.inline_functions.remove(&function.span);
    if !supported {
        return None;
    }
    *block = resume;
    Some(result)
}
