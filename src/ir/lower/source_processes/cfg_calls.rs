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
    let first_inline = process.inline_calls.len();
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
    process.inline_calls.truncate(first_inline);
    process.blocks[block.0 as usize] = original;
    truncate_process_values(context, first_value);
    (block, value)
}

fn has_call(value: ProcessValueId, context: &mut LoweringContext<'_>) -> bool {
    while context.cfg_call_cache.len() < context.process_ir.values.len() {
        let index = context.cfg_call_cache.len();
        let node = &context.process_ir.values[index];
        // A recursive call of a shared value function stays a call.
        let self_call = context
            .self_calls
            .borrow()
            .contains(&ProcessValueId(index as u32));
        let contains = !self_call
            && matches!(
                node.kind,
                ProcessValueKind::Call { .. }
                    | ProcessValueKind::HostCall { .. }
                    | ProcessValueKind::ForeignCall { .. }
                    | ProcessValueKind::Invalid
            )
            || crate::ir::process::any_process_value_dependency(&node.kind, |dependency| {
                context.cfg_call_cache[dependency.0 as usize]
            });
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
    let first_inline = process.inline_calls.len();
    let previous_scope = context.inline_scope;
    let scope = process.block_inline_call(block).or(previous_scope);
    context.inline_scope = scope;
    process.blocks[block.0 as usize] = ProcessBlock::empty(block);
    let result = normalize_contents(&original, process, context);
    context.inline_scope = previous_scope;
    if result.is_none() {
        process.blocks.truncate(first_block);
        process.locals.truncate(first_local);
        process.inline_calls.truncate(first_inline);
        process.blocks[block.0 as usize] = original;
        truncate_process_values(context, first_value);
        return false;
    }
    if let Some(scope) = scope {
        if first_block < process.blocks.len() {
            process.inline_calls[scope as usize]
                .blocks
                .push(first_block as u32..process.blocks.len() as u32);
        }
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
                arguments, format, ..
            } => {
                let originals = arguments.clone();
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
                // A part is an argument, matched in order so a value read
                // twice keeps each read's snapshot, or a projection of one (a
                // struct field, an array element) when a composite prints as
                // its parts; those normalize through the same memo, so a call
                // they read is the one already inlined.
                let mut cursor = 0;
                for part in format.iter_mut().flatten() {
                    if let ProcessFormatPart::Value { value, .. } = part {
                        if let Some(offset) = originals[cursor..]
                            .iter()
                            .position(|argument| argument == value)
                        {
                            *value = arguments[cursor + offset];
                            cursor += offset + 1;
                        } else {
                            *value = normalize_value(
                                *value, false, &mut block, process, context, &mut memo,
                            )?;
                        }
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
            if let Some(shared) = shared_leading_value(&[*then_value, *else_value], context) {
                normalize_value(shared, false, block, process, context, memo)?;
            }
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
            let values = arms.iter().map(|arm| arm.value).collect::<Vec<_>>();
            if let Some(shared) = shared_leading_value(&values, context) {
                normalize_value(shared, false, block, process, context, memo)?;
            }
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
            let call = match &node.kind {
                ProcessValueKind::InlineCall { source, .. } => Some((source.function, source.call)),
                _ => None,
            };
            let caller_block = *block;
            let first_instruction = process.blocks[block.0 as usize].instructions.len();
            let first_block = process.blocks.len();
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
            if let Some((function, call)) = call {
                // Extracted effects run in the caller CFG, not in the native
                // value frame. Preserve their evaluation order, but do not
                // label a caller instruction as the callee's return line.
                for (id, body) in process.blocks.iter_mut().enumerate() {
                    let start = if id == caller_block.0 as usize {
                        first_instruction
                    } else if id >= first_block {
                        0
                    } else {
                        continue;
                    };
                    for instruction in &mut body.instructions[start..] {
                        let span = match instruction {
                            ProcessInstruction::Declare { span, .. }
                            | ProcessInstruction::Assign { span, .. }
                            | ProcessInstruction::Schedule { span, .. }
                            | ProcessInstruction::Runtime { span, .. } => span,
                        };
                        if span.file == function.file
                            && span.start >= function.start
                            && span.end <= function.end
                        {
                            *span = call;
                        }
                    }
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
            crate::ir::process::remap_process_value_bindings(&mut kind, |child| {
                memo.get(&child)
                    .copied()
                    .or_else(|| (!has_call(child, context)).then_some(child))
            });
            // A deferred call can acquire its concrete width during CFG
            // expansion. Its enclosing value must not retain an unknown width.
            let width = node
                .bit_width
                .or_else(|| source_value_width(&kind, node.ty.as_ref(), process, context));
            let result = push_value(node.span, node.ty, width, kind, context);
            context.process_ir.value_layouts[result.0 as usize] =
                context.process_ir.value_layouts[value.0 as usize].clone();
            // Host/foreign effects belong at this source evaluation boundary,
            // including inside a selected conditional arm. Capture them once
            // in a CFG instruction instead of leaving an eager LLVM select.
            if matches!(
                context.process_ir.values[result.0 as usize].kind,
                ProcessValueKind::HostCall { .. } | ProcessValueKind::ForeignCall { .. }
            ) {
                capture_process_operand(result, context, process, *block)
            } else {
                result
            }
        }
    };
    memo.insert(value, result);
    Some(result)
}

/// A call-holding value that every arm evaluates before anything else of
/// its own, if they share one. Expanding it once ahead of the branch runs
/// nothing earlier than before, and the arms find it in the memo. Expanded
/// per arm instead, a chain of selections over the previous link (std's
/// `pow2`: `p1 = if c { p0 * 4.0 } else { p0 }`) doubled at every link.
fn shared_leading_value(
    arms: &[ProcessValueId],
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let (first, rest) = arms.split_first()?;
    let rest = rest
        .iter()
        .map(|arm| leading_values(*arm, context))
        .collect::<Vec<_>>();
    leading_values(*first, context)
        .into_iter()
        .find(|value| rest.iter().all(|chain| chain.contains(value)))
}

/// `value`, the call-holding operand it expands first, that operand's, and
/// so on: [`normalize_value`] expands operands in dependency order, so each
/// of these starts the expansion of the one before.
fn leading_values(
    mut value: ProcessValueId,
    context: &mut LoweringContext<'_>,
) -> Vec<ProcessValueId> {
    let mut chain = Vec::new();
    while has_call(value, context) {
        chain.push(value);
        let kind = &context.process_ir.values[value.0 as usize].kind;
        // These branch or call on their own; what they run first is theirs.
        if matches!(
            kind,
            ProcessValueKind::Call { .. }
                | ProcessValueKind::Select { .. }
                | ProcessValueKind::Match { .. }
                | ProcessValueKind::Invalid
        ) {
            break;
        }
        let dependencies = crate::ir::process_value_dependencies(kind);
        let Some(next) = dependencies
            .into_iter()
            .find(|dependency| has_call(*dependency, context))
        else {
            break;
        };
        value = next;
    }
    chain
}

fn result_local(
    node: &ProcessValue,
    source: ProcessValueId,
    process: &mut ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    let local = ProcessLocalId(process.locals.len() as u32);
    // A selection without a layout of its own has its arms' (an early
    // `return` inside a call that returns its receiver's format).
    let arms = match node.kind {
        ProcessValueKind::Select {
            then_value,
            else_value,
            ..
        } => vec![then_value, else_value],
        ProcessValueKind::Match { ref arms, .. } => arms.iter().map(|arm| arm.value).collect(),
        _ => Vec::new(),
    };
    let layout = process_value_source_layout(source, context.process_ir)
        .cloned()
        .or_else(|| {
            arms.iter().find_map(|arm| {
                process_value_source_layout(*arm, context.process_ir)
                    .filter(|layout| layout.packed_width().is_some())
                    .cloned()
            })
        })
        .or_else(|| {
            node.ty
                .as_ref()
                .and_then(|ty| process_layout_for_type(ty, node.span, context))
        })
        // An unsized family (`-> float` before its format is known) has no
        // width; the value's own word is what the local holds.
        .filter(|layout| layout.packed_width().is_some() || node.bit_width.is_none())
        .or_else(|| {
            node.bit_width.map(|width| SourceLayout {
                span: node.span,
                kind: LayoutKind::Scalar {
                    width,
                    domain: crate::ir::ScalarDomain::Bits,
                    nominal: None,
                    value_range: None,
                },
            })
        });
    let width = layout
        .as_ref()
        .and_then(SourceLayout::packed_width)
        .or(node.bit_width);
    process.locals.push(ProcessLocal {
        id: local,
        name: format!("<return:{}>", local.0),
        source: None,
        span: node.span,
        scope: None,
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

/// One recursive function expanded once for all its activations. Its
/// locals are the current activation's; a recursive call saves them on the
/// process's runtime frame stack with the number of its call site, binds the
/// arguments and jumps to `entry`. A `return` jumps to the dispatch, which
/// pops the site number and continues there, where the caller's locals are
/// restored.
pub(super) struct FrameState {
    pub(super) function: crate::diag::Span,
    pub(super) entry: ProcessBlockId,
    pub(super) parameters: Vec<ProcessValueId>,
    pub(super) result: ProcessValueId,
    pub(super) first_local: usize,
    pub(super) sites: Vec<ProcessBlockId>,
    /// Which parameters the body writes: each call copies them back to its
    /// argument when that is a local, as a plain call writes through.
    pub(super) written: Vec<bool>,
}

/// Expand a call; a function found recursive is expanded again with a frame
/// stack. A failed attempt leaves nothing behind.
fn inline_call(
    call: ProcessValueId,
    callee: ProcessValueId,
    arguments: &[ProcessValueId],
    block: &mut ProcessBlockId,
    process: &mut ProcessCfg,
    context: &mut LoweringContext<'_>,
    memo: &mut HashMap<ProcessValueId, ProcessValueId>,
) -> Option<ProcessValueId> {
    let start_block = *block;
    let original = process.blocks[block.0 as usize].clone();
    let (blocks, locals, inline_calls) = (
        process.blocks.len(),
        process.locals.len(),
        process.inline_calls.len(),
    );
    let values = context.process_ir.values.len();
    let recursions = context.recursive_calls.borrow().len();
    let saved_memo = memo.clone();
    if let Some(result) = inline_call_once(call, callee, arguments, block, process, context, memo) {
        return Some(result);
    }
    let recursive = context.recursive_calls.borrow()[recursions..]
        .iter()
        .map(|(_, _, function)| *function)
        .collect::<Vec<_>>();
    let &function = recursive.first()?;
    if context.frame_functions.contains(&function) {
        return None;
    }
    // Roll back and expand once more, now with a frame stack for it.
    let rollback = |process: &mut ProcessCfg, context: &mut LoweringContext<'_>| {
        process.blocks.truncate(blocks);
        process.locals.truncate(locals);
        process.inline_calls.truncate(inline_calls);
        process.blocks[start_block.0 as usize] = original.clone();
        truncate_process_values(context, values);
    };
    rollback(process, context);
    *block = start_block;
    *memo = saved_memo.clone();
    context.frame_functions.insert(function);
    let framed = inline_call_once(call, callee, arguments, block, process, context, memo);
    context.frame_functions.remove(&function);
    match framed {
        Some(result) => {
            // The plain attempt's recursion is solved; drop its reports.
            context.recursive_calls.borrow_mut().truncate(recursions);
            Some(result)
        }
        None => {
            rollback(process, context);
            *block = start_block;
            *memo = saved_memo;
            None
        }
    }
}

fn inline_call_once(
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
            match context.functions.get_definition(definition) {
                Some(function) => (function, None),
                // `Target(value)`: the `From` body converting the argument's type.
                None => {
                    let [argument] = arguments else { return None };
                    let target = context.process_ir.values[call.0 as usize]
                        .ty
                        .as_ref()
                        .and_then(|ty| process_type_key(ty, context))?;
                    let source = process_value_type(*argument, context)
                        .and_then(|ty| conversion_source_key(&ty, context))?;
                    (context.functions.get_conversion(&target, &source)?, None)
                }
            }
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
        if let Some(frame) = context
            .frames
            .iter()
            .rposition(|frame| frame.function == function.span)
        {
            if let Some(result) =
                recursive_frame_call(frame, call, arguments, block, process, context, memo)
            {
                return Some(result);
            }
        }
        context.recursive_calls.borrow_mut().push((
            function.name.text.clone(),
            context.process_ir.values[call.0 as usize].span,
            function.span,
        ));
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
    // With a frame stack each activation has its own parameters: copies,
    // not aliases of the caller's places.
    let framed = receiver.is_none() && context.frame_functions.contains(&function.span);
    let frame_first_local = process.locals.len();
    let mut frame_parameters = Vec::new();
    let mut frame_arguments = Vec::new();
    let mut bindings = HashMap::new();
    for (parameter, argument) in parameters.iter().zip(arguments) {
        let definition = context.resolved.declared(parameter.name.as_ref()?.span)?;
        let argument = normalize_value(*argument, true, block, process, context, memo)?;
        let argument = if framed {
            frame_arguments.push(argument);
            let copy = frame_copy(argument, process, context, *block);
            frame_parameters.push(copy);
            copy
        } else {
            capture_process_argument(argument, context, process, *block)
        };
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
        .or_else(|| same_type_argument(function, arguments, context))
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
    let dispatch = framed.then(|| process.push_block());
    let previous_scope = context.inline_scope;
    let scope = begin_inline_call(function, node.span, receiver, &bindings, process, context);
    process.inline_calls[scope as usize].result = Some(result);
    let first_local = process.locals.len();
    let entry = process.push_block();
    if framed {
        // The first activation's return continues here, at site 0.
        let site = frame_number(0, node.span, context);
        frame_push(site, process, context, *block);
    }
    process.blocks[block.0 as usize].terminator = ProcessTerminator::Goto(entry);
    context.inline_functions.insert(function.span);
    context.value_bindings.push(bindings);
    context.inline_self_values.push(receiver);
    context.inline_return_types.push(node.ty);
    context
        .inline_return_blocks
        .push((dispatch.unwrap_or(resume), Some(result)));
    if framed {
        context.frames.push(FrameState {
            function: function.span,
            entry,
            parameters: frame_parameters,
            result,
            first_local: frame_first_local,
            sites: vec![resume],
            written: Vec::new(),
        });
    }
    let first_body_block = entry.0 as usize;
    let tail = lower_statements(&body.stmts, context, process, entry);
    // Before normalization adds the recursive sites' own argument bindings.
    let mut writes_parameter = false;
    if framed {
        let parameters = context.frames.last()?.parameters.clone();
        let written = parameters
            .iter()
            .map(|parameter| writes_local(&[*parameter], first_body_block, process, context))
            .collect::<Vec<_>>();
        // The first call writes back too, which only a local can take here;
        // an expression argument has no place, as in a plain call.
        writes_parameter = written
            .iter()
            .zip(&frame_arguments)
            .any(|(written, argument)| {
                *written
                    && local_place(*argument, context).is_none()
                    && process_place_assignment(*argument, context.process_ir).is_some()
            });
        context.frames.last_mut()?.written = written;
    }
    finish_inline_call(scope, entry, first_local, process);
    let body_end = process.blocks.len();
    let mut supported = tail.is_none();
    for index in first_body_block..body_end {
        let ok = normalize_block(ProcessBlockId(index as u32), process, context);
        supported &= ok;
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
    if let Some(dispatch) = dispatch {
        let frame = context.frames.pop()?;
        // Writing a parameter would need it to reach the caller's place, which
        // a copy cannot: such a function keeps no frame.
        supported &= !writes_parameter;
        frame_dispatch(dispatch, &frame.sites, node.span, process, context);
        // The first call's written parameters reach its arguments on return.
        for ((parameter, argument), written) in frame
            .parameters
            .iter()
            .zip(&frame_arguments)
            .zip(&frame.written)
        {
            if *written {
                if let Some(target) = local_place(*argument, context) {
                    assign_result(target, *parameter, process, resume, context);
                }
            }
        }
    }
    context.inline_return_blocks.pop();
    context.inline_return_types.pop();
    context.inline_self_values.pop();
    context.value_bindings.pop();
    context.inline_functions.remove(&function.span);
    context.inline_scope = previous_scope;
    if !supported {
        return None;
    }
    *block = resume;
    Some(result)
}

/// A recursive call inside its own frame expansion: save the activation,
/// record where to continue, bind the arguments and start the body again.
fn recursive_frame_call(
    frame: usize,
    call: ProcessValueId,
    arguments: &[ProcessValueId],
    block: &mut ProcessBlockId,
    process: &mut ProcessCfg,
    context: &mut LoweringContext<'_>,
    memo: &mut HashMap<ProcessValueId, ProcessValueId>,
) -> Option<ProcessValueId> {
    let span = context.process_ir.values[call.0 as usize].span;
    let parameters = context.frames[frame].parameters.clone();
    if parameters.len() != arguments.len() {
        return None;
    }
    let result = context.frames[frame].result;
    let local_of = |value: ProcessValueId, context: &LoweringContext<'_>| match context
        .process_ir
        .values[value.0 as usize]
        .kind
    {
        ProcessValueKind::Local { local, .. } => Some(local),
        _ => None,
    };
    let result_local = local_of(result, context)?;
    // The activation: every local of the frame so far but its return slot.
    let saved = (context.frames[frame].first_local..process.locals.len())
        .map(|index| ProcessLocalId(index as u32))
        .filter(|local| *local != result_local)
        .collect::<Vec<_>>();
    if !saved
        .iter()
        .all(|local| frame_saveable(&process.locals[local.0 as usize]))
    {
        return None;
    }
    // Arguments read the caller's activation, so they are taken first.
    let mut values = Vec::with_capacity(arguments.len());
    let mut places = Vec::with_capacity(arguments.len());
    for argument in arguments {
        let value = normalize_value(*argument, true, block, process, context, memo)?;
        places.push(local_place(value, context));
        values.push(capture_process_operand(value, context, process, *block));
    }
    for local in &saved {
        let value = local_value(*local, process, context);
        frame_push(value, process, context, *block);
    }
    let site = context.frames[frame].sites.len() as u64;
    let site = frame_number(site, span, context);
    frame_push(site, process, context, *block);
    for (parameter, value) in parameters.iter().zip(values) {
        assign_result(*parameter, value, process, *block, context);
    }
    let entry = context.frames[frame].entry;
    process.blocks[block.0 as usize].terminator = ProcessTerminator::Goto(entry);
    let continuation = process.push_block();
    context.frames[frame].sites.push(continuation);
    // The callee's written parameters, taken before the caller's activation
    // comes back, then written to the caller's argument locals.
    let written = context.frames[frame].written.clone();
    let mut write_back = Vec::new();
    for ((parameter, place), written) in parameters.iter().zip(&places).zip(&written) {
        if let (true, Some(place)) = (*written, place) {
            let taken = frame_copy(*parameter, process, context, continuation);
            write_back.push((*place, taken));
        }
    }
    for local in saved.iter().rev() {
        let target = local_value(*local, process, context);
        let node = context.process_ir.values[target.0 as usize].clone();
        // Value and X/Z plane go straight back into the local's state.
        let restored = push_value(
            span,
            node.ty.clone(),
            node.bit_width,
            ProcessValueKind::HostCall {
                operation: ProcessHostValueOp::FrameRestore,
                arguments: vec![target],
            },
            context,
        );
        frame_copy(restored, process, context, continuation);
    }
    for (place, taken) in write_back {
        assign_result(place, taken, process, continuation, context);
    }
    *block = continuation;
    Some(result)
}

/// `value` when it is a plain local: a place a written parameter can be
/// copied back to.
fn local_place(value: ProcessValueId, context: &LoweringContext<'_>) -> Option<ProcessValueId> {
    matches!(
        context.process_ir.values[value.0 as usize].kind,
        ProcessValueKind::Local { .. }
    )
    .then_some(value)
}

/// Whether a local can be saved: it has a packed width (its X/Z plane, if
/// any, is saved alongside), or is a kernel local known by its type.
fn frame_saveable(local: &ProcessLocal) -> bool {
    match local.layout.as_ref() {
        Some(layout) => layout.packed_width().is_some_and(|width| width > 0),
        // A loop cursor or other kernel local declared by its type alone.
        None => frame_kernel_width(local).is_some(),
    }
}

/// The width of a local known only by its kernel type.
fn frame_kernel_width(local: &ProcessLocal) -> Option<u32> {
    match local.ty.as_ref()? {
        crate::types::Ty::Integer | crate::types::Ty::Real => Some(64),
        crate::types::Ty::Char => Some(32),
        _ => None,
    }
}

/// A read of `local` as a value.
fn local_value(
    local: ProcessLocalId,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    let declared = &process.locals[local.0 as usize];
    let layout = declared.layout.clone();
    let width = layout
        .as_ref()
        .and_then(SourceLayout::packed_width)
        .or_else(|| frame_kernel_width(declared));
    let value = push_value(
        declared.span,
        declared.ty.clone(),
        width,
        ProcessValueKind::Local {
            process: process.id,
            local,
        },
        context,
    );
    context.process_ir.value_layouts[value.0 as usize] = layout;
    value
}

/// A fresh local initialized with `value`: a parameter's own copy, or an
/// effect's place to be evaluated.
pub(super) fn frame_copy(
    value: ProcessValueId,
    process: &mut ProcessCfg,
    context: &mut LoweringContext<'_>,
    block: ProcessBlockId,
) -> ProcessValueId {
    let node = context.process_ir.values[value.0 as usize].clone();
    let copy = result_local(&node, value, process, context);
    let ProcessValueKind::Local { local, .. } = context.process_ir.values[copy.0 as usize].kind
    else {
        unreachable!()
    };
    process.blocks[block.0 as usize]
        .instructions
        .push(ProcessInstruction::Declare {
            local,
            initializer: Some(value),
            span: node.span,
        });
    copy
}

fn frame_number(
    number: u64,
    span: crate::diag::Span,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    push_value(
        span,
        Some(crate::types::Ty::Integer),
        Some(64),
        ProcessValueKind::Number(ProcessNumber::Integer(vec![number])),
        context,
    )
}

/// Save `value` on the frame stack, as an instruction in `block`.
fn frame_push(
    value: ProcessValueId,
    process: &mut ProcessCfg,
    context: &mut LoweringContext<'_>,
    block: ProcessBlockId,
) {
    let span = context.process_ir.values[value.0 as usize].span;
    let pushed = push_value(
        span,
        Some(crate::types::Ty::Integer),
        Some(64),
        ProcessValueKind::HostCall {
            operation: ProcessHostValueOp::FramePush,
            arguments: vec![value],
        },
        context,
    );
    frame_copy(pushed, process, context, block);
}

/// Fill `dispatch`: pop the site number and continue at that site.
fn frame_dispatch(
    dispatch: ProcessBlockId,
    sites: &[ProcessBlockId],
    span: crate::diag::Span,
    process: &mut ProcessCfg,
    context: &mut LoweringContext<'_>,
) {
    let popped = push_value(
        span,
        Some(crate::types::Ty::Integer),
        Some(64),
        ProcessValueKind::HostCall {
            operation: ProcessHostValueOp::FramePop,
            arguments: Vec::new(),
        },
        context,
    );
    let site = frame_copy(popped, process, context, dispatch);
    let mut current = dispatch;
    for (index, target) in sites.iter().enumerate() {
        if index + 1 == sites.len() {
            process.blocks[current.0 as usize].terminator = ProcessTerminator::Goto(*target);
            break;
        }
        let number = frame_number(index as u64, span, context);
        let condition = push_value(
            span,
            bool_type(context.resolved),
            Some(1),
            ProcessValueKind::Binary {
                operation: ProcessBinaryOp::Eq,
                left: site,
                right: number,
            },
            context,
        );
        let next = process.push_block();
        process.blocks[current.0 as usize].terminator = ProcessTerminator::Branch {
            condition,
            then_block: *target,
            else_block: next,
        };
        current = next;
    }
}

/// Whether a block from `first` on assigns to one of `locals` (through a
/// field, index or slice of it).
fn writes_local(
    locals: &[ProcessValueId],
    first: usize,
    process: &ProcessCfg,
    context: &LoweringContext<'_>,
) -> bool {
    let root = |mut target: ProcessValueId| loop {
        match &context.process_ir.values[target.0 as usize].kind {
            ProcessValueKind::Local { local, .. } => return Some(*local),
            ProcessValueKind::Field { base, .. }
            | ProcessValueKind::Index { base, .. }
            | ProcessValueKind::BitSlice { base, .. }
            | ProcessValueKind::PackedSlice { base, .. } => target = *base,
            _ => return None,
        }
    };
    let written = locals
        .iter()
        .filter_map(|value| root(*value))
        .collect::<Vec<_>>();
    process.blocks[first..].iter().any(|block| {
        block
            .instructions
            .iter()
            .any(|instruction| match instruction {
                ProcessInstruction::Assign { target, .. } => {
                    root(*target).is_some_and(|local| written.contains(&local))
                }
                _ => false,
            })
    })
}
