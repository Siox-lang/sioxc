//! Loops and `match` dispatch.

use super::*;

/// Emit one inclusive, directional range-loop header. Cursor and end state
/// live in the design object because the body may suspend and resume through a
/// later call to the process entry. The range operands are evaluated only on
/// first entry, matching source-level `for` semantics.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_range_loop<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: FunctionValue<'ctx>,
    design: &Design,
    process: ProcessId,
    block: siox::ir::ProcessBlockId,
    local: ProcessLocalId,
    iterable: ProcessValueId,
    body: BasicBlock<'ctx>,
    exit: BasicBlock<'ctx>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<()> {
    let (left, right) = range_loop_bounds(design, iterable)?;
    let active_name = loop_active_name(process, block);
    let cursor_name = loop_cursor_name(process, block);
    let end_name = loop_end_name(process, block);
    let active = state_value(context, module, builder, &active_name, 1)?;
    let initialize = context.append_basic_block(function, &format!("bb{}.for.initialize", block.0));
    let advance = context.append_basic_block(function, &format!("bb{}.for.advance", block.0));
    let complete = context.append_basic_block(function, &format!("bb{}.for.complete", block.0));
    let next = context.append_basic_block(function, &format!("bb{}.for.next", block.0));
    builder
        .build_conditional_branch(active, advance, initialize)
        .ok()?;

    builder.position_at_end(initialize);
    let left = process_value_at(
        context,
        module,
        builder,
        design,
        left,
        64,
        true,
        None,
        index_sites,
        cache,
    )?;
    let right = process_value_at(
        context,
        module,
        builder,
        design,
        right,
        64,
        true,
        None,
        index_sites,
        cache,
    )?;
    store_state(module, builder, &cursor_name, 64, left)?;
    store_state(module, builder, &end_name, 64, right)?;
    store_state(
        module,
        builder,
        &active_name,
        1,
        context.bool_type().const_int(1, false),
    )?;
    store_state(module, builder, &local_state_name(process, local), 64, left)?;
    builder.build_unconditional_branch(body).ok()?;

    builder.position_at_end(advance);
    let cursor = state_value(context, module, builder, &cursor_name, 64)?;
    let end = state_value(context, module, builder, &end_name, 64)?;
    let at_end = builder
        .build_int_compare(IntPredicate::EQ, cursor, end, "process.loop.at_end")
        .ok()?;
    builder
        .build_conditional_branch(at_end, complete, next)
        .ok()?;

    builder.position_at_end(complete);
    store_state(
        module,
        builder,
        &active_name,
        1,
        context.bool_type().const_zero(),
    )?;
    builder.build_unconditional_branch(exit).ok()?;

    builder.position_at_end(next);
    let ascending = builder
        .build_int_compare(IntPredicate::SLT, cursor, end, "process.loop.ascending")
        .ok()?;
    let one = context.i64_type().const_int(1, false);
    let incremented = builder
        .build_int_add(cursor, one, "process.loop.incremented")
        .ok()?;
    let decremented = builder
        .build_int_sub(cursor, one, "process.loop.decremented")
        .ok()?;
    let cursor = builder
        .build_select(ascending, incremented, decremented, "process.loop.next")
        .ok()?
        .into_int_value();
    store_state(module, builder, &cursor_name, 64, cursor)?;
    store_state(
        module,
        builder,
        &local_state_name(process, local),
        64,
        cursor,
    )?;
    builder.build_unconditional_branch(body).ok()?;
    Some(())
}

/// Emit an array loop over a snapshot of the source-order packed value. The
/// snapshot is object state so a suspension in the body does not re-read a
/// subsequently changed iterable.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_array_loop<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: FunctionValue<'ctx>,
    design: &Design,
    process: ProcessId,
    block: siox::ir::ProcessBlockId,
    local: ProcessLocalId,
    iterable: ProcessValueId,
    body: BasicBlock<'ctx>,
    exit: BasicBlock<'ctx>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<()> {
    let (layout, element, length) = array_loop_shape(design, iterable)?;
    let width = layout_width(layout)?;
    let element_width = layout_width(element)?;
    let active_name = loop_active_name(process, block);
    let cursor_name = loop_cursor_name(process, block);
    let end_name = loop_end_name(process, block);
    let iterable_name = loop_iterable_name(process, block);
    let active = state_value(context, module, builder, &active_name, 1)?;
    let initialize =
        context.append_basic_block(function, &format!("bb{}.array.initialize", block.0));
    let advance = context.append_basic_block(function, &format!("bb{}.array.advance", block.0));
    let complete = context.append_basic_block(function, &format!("bb{}.array.complete", block.0));
    let next = context.append_basic_block(function, &format!("bb{}.array.next", block.0));
    builder
        .build_conditional_branch(active, advance, initialize)
        .ok()?;

    builder.position_at_end(initialize);
    let snapshot = process_value_in_layout(
        context,
        module,
        builder,
        design,
        iterable,
        layout,
        None,
        index_sites,
        cache,
    )?;
    store_state(module, builder, &iterable_name, width, snapshot)?;
    let meta_width = layout_meta_width(design, layout);
    let metadata = if let Some(meta_width) = meta_width {
        let metadata = process_value_meta_in_layout(
            context,
            module,
            builder,
            design,
            iterable,
            layout,
            None,
            index_sites,
            cache,
        )?;
        store_state(
            module,
            builder,
            &loop_iterable_meta_name(process, block),
            meta_width,
            metadata,
        )?;
        Some(metadata)
    } else {
        None
    };
    store_state(
        module,
        builder,
        &cursor_name,
        64,
        context.i64_type().const_zero(),
    )?;
    store_state(
        module,
        builder,
        &end_name,
        64,
        context
            .i64_type()
            .const_int(u64::from(length.saturating_sub(1)), false),
    )?;
    store_state(
        module,
        builder,
        &active_name,
        1,
        context.bool_type().const_int(1, false),
    )?;
    let first = extract_region(builder, snapshot, 0, element_width)?;
    store_state(
        module,
        builder,
        &local_state_name(process, local),
        element_width,
        first,
    )?;
    if let Some(metadata) = metadata {
        let width = local_meta_width(design, process, local)?;
        let first = extract_region(builder, metadata, 0, width)?;
        store_state(
            module,
            builder,
            &local_meta_name(process, local),
            width,
            first,
        )?;
    }
    builder.build_unconditional_branch(body).ok()?;

    builder.position_at_end(advance);
    let cursor = state_value(context, module, builder, &cursor_name, 64)?;
    let end = state_value(context, module, builder, &end_name, 64)?;
    let at_end = builder
        .build_int_compare(IntPredicate::EQ, cursor, end, "process.array.at_end")
        .ok()?;
    builder
        .build_conditional_branch(at_end, complete, next)
        .ok()?;

    builder.position_at_end(complete);
    store_state(
        module,
        builder,
        &active_name,
        1,
        context.bool_type().const_zero(),
    )?;
    builder.build_unconditional_branch(exit).ok()?;

    builder.position_at_end(next);
    let cursor = builder
        .build_int_add(
            cursor,
            context.i64_type().const_int(1, false),
            "process.array.next",
        )
        .ok()?;
    store_state(module, builder, &cursor_name, 64, cursor)?;
    let snapshot = state_value(context, module, builder, &iterable_name, width)?;
    let offset = builder
        .build_int_mul(
            cursor,
            context
                .i64_type()
                .const_int(u64::from(element_width), false),
            "process.array.offset",
        )
        .ok()?;
    let offset = fit(builder, offset, width)?;
    let shifted = builder
        .build_right_shift(snapshot, offset, false, "process.array.element")
        .ok()?;
    let element = fit(builder, shifted, element_width)?;
    store_state(
        module,
        builder,
        &local_state_name(process, local),
        element_width,
        element,
    )?;
    if let Some(meta_width) = meta_width {
        let snapshot = state_value(
            context,
            module,
            builder,
            &loop_iterable_meta_name(process, block),
            meta_width,
        )?;
        let metadata_offset = fit(builder, cursor, meta_width)?;
        let local_width = local_meta_width(design, process, local)?;
        let offset = builder
            .build_int_mul(
                metadata_offset,
                metadata_offset
                    .get_type()
                    .const_int(u64::from(local_width), false),
                "process.array.metadata.offset",
            )
            .ok()?;
        let selected = extract_dynamic_region(builder, snapshot, offset, local_width)?;
        store_state(
            module,
            builder,
            &local_meta_name(process, local),
            local_width,
            selected,
        )?;
    }
    builder.build_unconditional_branch(body).ok()?;
    Some(())
}

/// Iterate a runtime-owned UTF-8 string handle by Unicode scalar value. The
/// handle is snapshotted just like a fixed array so a suspended loop keeps the
/// same iterable even if its source storage is reassigned.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_dynamic_string_loop<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: FunctionValue<'ctx>,
    design: &Design,
    process: ProcessId,
    block: siox::ir::ProcessBlockId,
    local: ProcessLocalId,
    iterable: ProcessValueId,
    body: BasicBlock<'ctx>,
    exit: BasicBlock<'ctx>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<()> {
    if !dynamic_string_value(design, iterable) || local_width(design, process, local) != Some(32) {
        return None;
    }
    let i64 = context.i64_type();
    let length_fn = module
        .get_function("sx_runtime_string_length")
        .unwrap_or_else(|| {
            module.add_function(
                "sx_runtime_string_length",
                i64.fn_type(&[i64.into()], false),
                Some(Linkage::External),
            )
        });
    let index_fn = module
        .get_function("sx_runtime_string_index")
        .unwrap_or_else(|| {
            module.add_function(
                "sx_runtime_string_index",
                i64.fn_type(&[i64.into(), i64.into()], false),
                Some(Linkage::External),
            )
        });
    let active_name = loop_active_name(process, block);
    let cursor_name = loop_cursor_name(process, block);
    let end_name = loop_end_name(process, block);
    let iterable_name = loop_iterable_name(process, block);
    let active = state_value(context, module, builder, &active_name, 1)?;
    let initialize =
        context.append_basic_block(function, &format!("bb{}.string.initialize", block.0));
    let first = context.append_basic_block(function, &format!("bb{}.string.first", block.0));
    let advance = context.append_basic_block(function, &format!("bb{}.string.advance", block.0));
    let complete = context.append_basic_block(function, &format!("bb{}.string.complete", block.0));
    let next = context.append_basic_block(function, &format!("bb{}.string.next", block.0));
    builder
        .build_conditional_branch(active, advance, initialize)
        .ok()?;

    builder.position_at_end(initialize);
    let handle = process_value_at(
        context,
        module,
        builder,
        design,
        iterable,
        64,
        false,
        None,
        index_sites,
        cache,
    )?;
    store_state(module, builder, &iterable_name, 64, handle)?;
    let length = match builder
        .build_call(length_fn, &[handle.into()], "process.string.length")
        .ok()?
        .try_as_basic_value()
    {
        inkwell::values::ValueKind::Basic(value) => value.into_int_value(),
        _ => return None,
    };
    let empty = builder
        .build_int_compare(
            IntPredicate::EQ,
            length,
            i64.const_zero(),
            "process.string.empty",
        )
        .ok()?;
    builder
        .build_conditional_branch(empty, complete, first)
        .ok()?;

    builder.position_at_end(first);
    let zero = i64.const_zero();
    store_state(module, builder, &cursor_name, 64, zero)?;
    let end = builder
        .build_int_sub(length, i64.const_int(1, false), "process.string.end")
        .ok()?;
    store_state(module, builder, &end_name, 64, end)?;
    store_state(
        module,
        builder,
        &active_name,
        1,
        context.bool_type().const_int(1, false),
    )?;
    let character = match builder
        .build_call(
            index_fn,
            &[handle.into(), zero.into()],
            "process.string.first.character",
        )
        .ok()?
        .try_as_basic_value()
    {
        inkwell::values::ValueKind::Basic(value) => value.into_int_value(),
        _ => return None,
    };
    store_state(
        module,
        builder,
        &local_state_name(process, local),
        32,
        fit(builder, character, 32)?,
    )?;
    builder.build_unconditional_branch(body).ok()?;

    builder.position_at_end(advance);
    let cursor = state_value(context, module, builder, &cursor_name, 64)?;
    let end = state_value(context, module, builder, &end_name, 64)?;
    let at_end = builder
        .build_int_compare(IntPredicate::EQ, cursor, end, "process.string.at_end")
        .ok()?;
    builder
        .build_conditional_branch(at_end, complete, next)
        .ok()?;

    builder.position_at_end(complete);
    store_state(
        module,
        builder,
        &active_name,
        1,
        context.bool_type().const_zero(),
    )?;
    builder.build_unconditional_branch(exit).ok()?;

    builder.position_at_end(next);
    let cursor = builder
        .build_int_add(cursor, i64.const_int(1, false), "process.string.next")
        .ok()?;
    store_state(module, builder, &cursor_name, 64, cursor)?;
    let handle = state_value(context, module, builder, &iterable_name, 64)?;
    let character = match builder
        .build_call(
            index_fn,
            &[handle.into(), cursor.into()],
            "process.string.character",
        )
        .ok()?
        .try_as_basic_value()
    {
        inkwell::values::ValueKind::Basic(value) => value.into_int_value(),
        _ => return None,
    };
    store_state(
        module,
        builder,
        &local_state_name(process, local),
        32,
        fit(builder, character, 32)?,
    )?;
    builder.build_unconditional_branch(body).ok()?;
    Some(())
}

pub(super) fn process_pattern_condition<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    design: &Design,
    scrutinee_id: ProcessValueId,
    scrutinee: IntValue<'ctx>,
    pattern: &ProcessPattern,
) -> Option<IntValue<'ctx>> {
    let ty = scrutinee.get_type();
    match pattern {
        ProcessPattern::Wildcard => Some(context.bool_type().const_int(1, false)),
        ProcessPattern::Number(number) => {
            let expected = match number {
                ProcessNumber::Integer(words) => ty.const_int_arbitrary_precision(words),
                ProcessNumber::Real(bits) => ty.const_int(*bits, false),
            };
            builder
                .build_int_compare(IntPredicate::EQ, scrutinee, expected, "process.match.exact")
                .ok()
        }
        ProcessPattern::BitMask { mask, value } => {
            let mask = ty.const_int_arbitrary_precision(mask);
            let value = ty.const_int_arbitrary_precision(value);
            let selected = builder
                .build_and(scrutinee, mask, "process.match.mask")
                .ok()?;
            builder
                .build_int_compare(IntPredicate::EQ, selected, value, "process.match.bits")
                .ok()
        }
        ProcessPattern::Or(alternatives) => {
            let mut any = context.bool_type().const_zero();
            for alternative in alternatives {
                let matches = process_pattern_condition(
                    context,
                    builder,
                    design,
                    scrutinee_id,
                    scrutinee,
                    alternative,
                )?;
                any = builder.build_or(any, matches, "process.match.or").ok()?;
            }
            Some(any)
        }
        ProcessPattern::Range { left, right } => {
            let low = (*left).min(*right);
            let high = (*left).max(*right);
            if process_value_is_real(design, scrutinee_id) {
                let real = builder
                    .build_bit_cast(scrutinee, context.f64_type(), "process.match.real")
                    .ok()?
                    .into_float_value();
                let above = builder
                    .build_float_compare(
                        FloatPredicate::OGE,
                        real,
                        context.f64_type().const_float(low as f64),
                        "process.match.real.low",
                    )
                    .ok()?;
                let below = builder
                    .build_float_compare(
                        FloatPredicate::OLE,
                        real,
                        context.f64_type().const_float(high as f64),
                        "process.match.real.high",
                    )
                    .ok()?;
                builder
                    .build_and(above, below, "process.match.real.range")
                    .ok()
            } else {
                let signed = process_value_is_signed(design, scrutinee_id);
                let low = ty.const_int(low as u64, signed);
                let high = ty.const_int(high as u64, signed);
                let above = builder
                    .build_int_compare(
                        if signed {
                            IntPredicate::SGE
                        } else {
                            IntPredicate::UGE
                        },
                        scrutinee,
                        low,
                        "process.match.low",
                    )
                    .ok()?;
                let below = builder
                    .build_int_compare(
                        if signed {
                            IntPredicate::SLE
                        } else {
                            IntPredicate::ULE
                        },
                        scrutinee,
                        high,
                        "process.match.high",
                    )
                    .ok()?;
                builder.build_and(above, below, "process.match.range").ok()
            }
        }
        ProcessPattern::Char(character) => builder
            .build_int_compare(
                IntPredicate::EQ,
                scrutinee,
                ty.const_int(u64::from(u32::from(*character)), false),
                "process.match.char",
            )
            .ok(),
        ProcessPattern::Path { .. } | ProcessPattern::BitPattern(_) => None,
    }
}

pub(super) fn process_match_eligibility<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    design: &Design,
    scrutinee_id: ProcessValueId,
    scrutinee: IntValue<'ctx>,
    arms: &[siox::ir::ProcessValueMatchArm],
) -> Option<Vec<IntValue<'ctx>>> {
    let mut matched = context.bool_type().const_zero();
    let mut eligible = Vec::with_capacity(arms.len());
    for arm in arms {
        let condition = process_pattern_condition(
            context,
            builder,
            design,
            scrutinee_id,
            scrutinee,
            &arm.pattern,
        )?;
        let not_matched = builder.build_not(matched, "pv.match.not_matched").ok()?;
        eligible.push(
            builder
                .build_and(not_matched, condition, "pv.match.eligible")
                .ok()?,
        );
        matched = builder
            .build_or(matched, condition, "pv.match.matched")
            .ok()?;
    }
    Some(eligible)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_process_match<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    function: FunctionValue<'ctx>,
    design: &Design,
    scrutinee_id: ProcessValueId,
    scrutinee: IntValue<'ctx>,
    arms: &[siox::ir::ProcessMatchArm],
    fallback: Option<siox::ir::ProcessBlockId>,
    blocks: &[BasicBlock<'ctx>],
    invalid: BasicBlock<'ctx>,
) -> Option<()> {
    for (index, arm) in arms.iter().enumerate() {
        let condition = process_pattern_condition(
            context,
            builder,
            design,
            scrutinee_id,
            scrutinee,
            &arm.pattern,
        )?;
        let miss = if index + 1 == arms.len() {
            fallback.map_or(invalid, |fallback| blocks[fallback.0 as usize])
        } else {
            context.append_basic_block(function, &format!("process.match.next.{index}"))
        };
        builder
            .build_conditional_branch(condition, blocks[arm.block.0 as usize], miss)
            .ok()?;
        if index + 1 != arms.len() {
            builder.position_at_end(miss);
        }
    }
    Some(())
}
