//! Logic discriminants and metavalue companion planes.

use super::*;

pub(super) fn logic_binary_discriminant<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    table: &HashMap<(u64, u64), u64>,
    left: IntValue<'ctx>,
    right: IntValue<'ctx>,
) -> Option<IntValue<'ctx>> {
    let ty = context
        .custom_width_int_type(std::num::NonZeroU32::new(4)?)
        .ok()?;
    let mut entries = table.iter().collect::<Vec<_>>();
    entries.sort_by_key(|((left, right), _)| (*left, *right));
    let mut result = ty.const_zero();
    for (&(left_value, right_value), &output) in entries.into_iter().rev() {
        let left_matches = builder
            .build_int_compare(
                IntPredicate::EQ,
                left,
                ty.const_int(left_value, false),
                "pv.logic.table.left",
            )
            .ok()?;
        let right_matches = builder
            .build_int_compare(
                IntPredicate::EQ,
                right,
                ty.const_int(right_value, false),
                "pv.logic.table.right",
            )
            .ok()?;
        let matches = builder
            .build_and(left_matches, right_matches, "pv.logic.table.match")
            .ok()?;
        result = builder
            .build_select(
                matches,
                ty.const_int(output, false),
                result,
                "pv.logic.table.result",
            )
            .ok()?
            .into_int_value();
    }
    Some(result)
}

pub(super) fn logic_unary_discriminant<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    table: &HashMap<u64, u64>,
    operand: IntValue<'ctx>,
) -> Option<IntValue<'ctx>> {
    let ty = context
        .custom_width_int_type(std::num::NonZeroU32::new(4)?)
        .ok()?;
    let mut entries = table.iter().collect::<Vec<_>>();
    entries.sort_by_key(|(operand, _)| **operand);
    let mut result = ty.const_zero();
    for (&input, &output) in entries.into_iter().rev() {
        let matches = builder
            .build_int_compare(
                IntPredicate::EQ,
                operand,
                ty.const_int(input, false),
                "pv.logic.table.operand",
            )
            .ok()?;
        result = builder
            .build_select(
                matches,
                ty.const_int(output, false),
                result,
                "pv.logic.table.result",
            )
            .ok()?
            .into_int_value();
    }
    Some(result)
}

/// Static unknown-plane facts in dependency order. Shared subgraphs are
/// classified once, not recursively revisited for each path or consumer.
pub(super) fn meta_free_process_values(design: &Design) -> Vec<bool> {
    let mut facts = Vec::with_capacity(design.process_ir.values.len());
    for value in &design.process_ir.values {
        let free = |id: ProcessValueId| facts.get(id.0 as usize).copied().unwrap_or(false);
        let meta_free = match &value.kind {
            ProcessValueKind::Number(_)
            | ProcessValueKind::Suffixed { .. }
            | ProcessValueKind::BitString { .. }
            | ProcessValueKind::Char(_)
            | ProcessValueKind::Default
            | ProcessValueKind::Attribute { .. }
            | ProcessValueKind::TableLookup { .. }
            | ProcessValueKind::ForeignCall { .. }
            | ProcessValueKind::HostCall { .. }
            // A shared function is only called with metavalue-free arguments.
            | ProcessValueKind::Parameter { .. }
            | ProcessValueKind::MetaCompare { .. } => true,
            ProcessValueKind::Signal { signals, state } => {
                !matches!(state, ProcessSignalState::Event)
                    && signals
                        .iter()
                        .all(|signal| !design.meta_of.contains_key(&signal.0))
            }
            ProcessValueKind::Local { process, local } => {
                local_meta_width(design, *process, *local).is_none()
            }
            ProcessValueKind::Storage(storage) | ProcessValueKind::StorageState { storage, .. } => {
                storage_meta_width(design, *storage).is_none()
            }
            // `integer(x)` leaves the logic domain: a kernel integer has no
            // metavalues, so arithmetic over one stores into a packed word with
            // every element known (VHDL's `to_integer` likewise yields a number).
            ProcessValueKind::RawResize { .. }
                if matches!(value.ty, Some(siox::types::Ty::Integer)) =>
            {
                true
            }
            ProcessValueKind::Unary { operand, .. }
            | ProcessValueKind::RawResize { operand }
            | ProcessValueKind::Field { base: operand, .. }
            | ProcessValueKind::BitSlice { base: operand, .. }
            | ProcessValueKind::PackedSlice { base: operand, .. } => free(*operand),
            ProcessValueKind::CheckedIndex { index, valid, .. } => free(*index) && free(*valid),
            ProcessValueKind::Binary { left, right, .. } => free(*left) && free(*right),
            ProcessValueKind::Select {
                condition,
                then_value,
                else_value,
            } => free(*condition) && free(*then_value) && free(*else_value),
            ProcessValueKind::Match { scrutinee, arms } => {
                free(*scrutinee) && arms.iter().all(|arm| free(arm.value))
            }
            ProcessValueKind::Construct { fields, spread, .. } => {
                spread.is_none_or(free) && fields.iter().all(|field| field.value.is_none_or(free))
            }
            ProcessValueKind::Array(values) | ProcessValueKind::Concat(values) => {
                values.iter().all(|value| free(*value))
            }
            ProcessValueKind::Index { base, index } => free(*base) && free(*index),
            ProcessValueKind::String(_)
            | ProcessValueKind::Definition(_)
            | ProcessValueKind::Intrinsic(_)
            | ProcessValueKind::Range { .. }
            | ProcessValueKind::Call { .. }
            | ProcessValueKind::Invalid => false,
        };
        facts.push(meta_free);
    }
    facts
}

/// Emit the exact discriminant plane for one packed Process value. A zero
/// nibble is the compact representation of an ordinary binary element: reads
/// reconstruct its source-defined `0`/`1` discriminant from the value plane.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_packed_meta_in_layout<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    id: ProcessValueId,
    layout: &SourceLayout,
    active: Option<IntValue<'ctx>>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    let key = (id, cache.key(id, active, None).1, layout.clone());
    if let Some(value) = cache.metadata.get(&key).copied() {
        return Some(value);
    }
    let value = process_packed_meta_uncached(
        context,
        module,
        builder,
        design,
        id,
        layout,
        active,
        index_sites,
        cache,
    )?;
    cache.metadata.insert(key, value);
    Some(value)
}

#[allow(clippy::too_many_arguments)]
fn process_packed_meta_uncached<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    id: ProcessValueId,
    layout: &SourceLayout,
    active: Option<IntValue<'ctx>>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    let (width, encoding) = packed_logic_layout(design, layout)?;
    let meta_width = width.checked_mul(4)?;
    let ty = context
        .custom_width_int_type(std::num::NonZeroU32::new(meta_width)?)
        .ok()?;
    let value = design.process_ir.values.get(id.0 as usize)?;
    let scalar_logic = width == 1 && process_scalar_logic_encoding(design, id).is_some();
    if scalar_logic {
        let value = process_value_at(
            context,
            module,
            builder,
            design,
            id,
            value.bit_width?,
            false,
            active,
            index_sites,
            cache,
        )?;
        return compact_discriminant(context, builder, encoding, value);
    }
    if cache.meta_free.get(id.0 as usize).copied().unwrap_or(false) {
        return Some(ty.const_zero());
    }
    if aggregate_metadata_projection(design, id) {
        return process_aggregate_projection_meta(
            context,
            module,
            builder,
            design,
            id,
            layout,
            active,
            index_sites,
            cache,
        );
    }
    match &value.kind {
        ProcessValueKind::Number(_)
        | ProcessValueKind::BitString { .. }
        | ProcessValueKind::Default => Some(ty.const_zero()),
        ProcessValueKind::String(text) => {
            let LayoutKind::Packed {
                element_enum: Some(element),
                ..
            } = &layout.kind
            else {
                return None;
            };
            packed_string_meta(context, builder, design, text, width, element, encoding)
        }
        ProcessValueKind::Storage(storage) => {
            (storage_meta_width(design, *storage) == Some(meta_width)).then(|| {
                state_value(
                    context,
                    module,
                    builder,
                    &storage_meta_name(*storage),
                    meta_width,
                )
            })?
        }
        ProcessValueKind::StorageState {
            storage,
            state: ProcessSignalState::Old,
        } => (storage_meta_width(design, *storage) == Some(meta_width)).then(|| {
            state_value(
                context,
                module,
                builder,
                &storage_meta_old_name(*storage),
                meta_width,
            )
        })?,
        ProcessValueKind::Local { process, local } => {
            (local_meta_width(design, *process, *local) == Some(meta_width)).then(|| {
                state_value(
                    context,
                    module,
                    builder,
                    &local_meta_name(*process, *local),
                    meta_width,
                )
            })?
        }
        ProcessValueKind::Signal { signals, state }
            if !matches!(state, ProcessSignalState::Event) =>
        {
            let mut result = ty.const_zero();
            let mut offset = 0u32;
            for signal in signals {
                let signal_width = design.signal_width(*signal)?;
                if let Some(companion) = design.meta_of.get(&signal.0).copied() {
                    let companion_width = signal_width.checked_mul(4)?;
                    let companion = cached_signal_value(
                        context,
                        module,
                        builder,
                        design,
                        &[SignalId(companion)],
                        *state,
                        companion_width,
                        cache,
                    )?;
                    result = insert_region(
                        builder,
                        result,
                        companion,
                        offset.checked_mul(4)?,
                        companion_width,
                    )?;
                }
                offset = offset.checked_add(signal_width)?;
            }
            (offset == width).then_some(result)
        }
        ProcessValueKind::Unary { operation, operand } => match operation {
            ProcessUnaryOp::Not => {
                let table = encoding.unary_ops.get("not")?;
                let operand_meta = process_packed_meta_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    *operand,
                    layout,
                    active,
                    index_sites,
                    cache,
                )?;
                let operand_value = process_value_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    *operand,
                    layout,
                    active,
                    index_sites,
                    cache,
                )?;
                let mut result = ty.const_zero();
                for position in 0..width {
                    let bit = extract_region(builder, operand_value, position, 1)?;
                    let metadata =
                        extract_region(builder, operand_meta, position.checked_mul(4)?, 4)?;
                    let discriminant =
                        packed_discriminant(context, builder, encoding, bit, Some(metadata), 4)?;
                    let output = logic_unary_discriminant(context, builder, table, discriminant)?;
                    let compact = compact_discriminant(context, builder, encoding, output)?;
                    result = insert_region(builder, result, compact, position.checked_mul(4)?, 4)?;
                }
                Some(result)
            }
            ProcessUnaryOp::Neg => {
                let operand_layout = packed_arithmetic_operand_layout(design, layout, *operand)?;
                let (operand_width, operand_encoding) =
                    packed_logic_layout(design, &operand_layout)?;
                let operand = process_packed_meta_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    *operand,
                    &operand_layout,
                    active,
                    index_sites,
                    cache,
                )?;
                let unknown = packed_meta_unknown(
                    context,
                    builder,
                    operand,
                    operand_width,
                    operand_encoding,
                )?;
                let poisoned = packed_unknown_meta(context, builder, width, encoding)?;
                builder
                    .build_select(unknown, poisoned, ty.const_zero(), "pv.meta.arithmetic")
                    .ok()
                    .map(|value| value.into_int_value())
            }
            ProcessUnaryOp::RealToInteger | ProcessUnaryOp::IntegerToReal => None,
        },
        ProcessValueKind::Binary {
            operation,
            left,
            right,
        } => match operation {
            ProcessBinaryOp::And | ProcessBinaryOp::Or | ProcessBinaryOp::Xor => {
                let symbol = match operation {
                    ProcessBinaryOp::And => "and",
                    ProcessBinaryOp::Or => "or",
                    ProcessBinaryOp::Xor => "xor",
                    _ => unreachable!(),
                };
                let table = encoding.binary_ops.get(symbol)?;
                let left_meta = process_packed_meta_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    *left,
                    layout,
                    active,
                    index_sites,
                    cache,
                )?;
                let right_meta = process_packed_meta_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    *right,
                    layout,
                    active,
                    index_sites,
                    cache,
                )?;
                let left_value = process_value_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    *left,
                    layout,
                    active,
                    index_sites,
                    cache,
                )?;
                let right_value = process_value_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    *right,
                    layout,
                    active,
                    index_sites,
                    cache,
                )?;
                let mut result = ty.const_zero();
                for position in 0..width {
                    let left_disc = packed_discriminant(
                        context,
                        builder,
                        encoding,
                        extract_region(builder, left_value, position, 1)?,
                        Some(extract_region(
                            builder,
                            left_meta,
                            position.checked_mul(4)?,
                            4,
                        )?),
                        4,
                    )?;
                    let right_disc = packed_discriminant(
                        context,
                        builder,
                        encoding,
                        extract_region(builder, right_value, position, 1)?,
                        Some(extract_region(
                            builder,
                            right_meta,
                            position.checked_mul(4)?,
                            4,
                        )?),
                        4,
                    )?;
                    let output =
                        logic_binary_discriminant(context, builder, table, left_disc, right_disc)?;
                    let compact = compact_discriminant(context, builder, encoding, output)?;
                    result = insert_region(builder, result, compact, position.checked_mul(4)?, 4)?;
                }
                Some(result)
            }
            ProcessBinaryOp::Add
            | ProcessBinaryOp::Sub
            | ProcessBinaryOp::Mul
            | ProcessBinaryOp::Div
            | ProcessBinaryOp::SignedAdd
            | ProcessBinaryOp::SignedSub
            | ProcessBinaryOp::SignedMul
            | ProcessBinaryOp::SignedDiv => {
                let left_layout = packed_arithmetic_operand_layout(design, layout, *left)?;
                let right_layout = packed_arithmetic_operand_layout(design, layout, *right)?;
                let (left_width, left_encoding) = packed_logic_layout(design, &left_layout)?;
                let (right_width, right_encoding) = packed_logic_layout(design, &right_layout)?;
                let left = process_packed_meta_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    *left,
                    &left_layout,
                    active,
                    index_sites,
                    cache,
                )?;
                let right = process_packed_meta_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    *right,
                    &right_layout,
                    active,
                    index_sites,
                    cache,
                )?;
                let left_unknown =
                    packed_meta_unknown(context, builder, left, left_width, left_encoding)?;
                let right_unknown =
                    packed_meta_unknown(context, builder, right, right_width, right_encoding)?;
                let unknown = builder
                    .build_or(left_unknown, right_unknown, "pv.meta.arithmetic.unknown")
                    .ok()?;
                let poisoned = packed_unknown_meta(context, builder, width, encoding)?;
                builder
                    .build_select(unknown, poisoned, ty.const_zero(), "pv.meta.arithmetic")
                    .ok()
                    .map(|value| value.into_int_value())
            }
            ProcessBinaryOp::Shl | ProcessBinaryOp::Shr => {
                let left_layout = packed_arithmetic_operand_layout(design, layout, *left)?;
                let left = process_packed_meta_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    *left,
                    &left_layout,
                    active,
                    index_sites,
                    cache,
                )?;
                let left = fit(builder, left, meta_width)?;
                let shift_width = design.process_ir.values.get(right.0 as usize)?.bit_width?;
                let shift = process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    *right,
                    shift_width,
                    false,
                    active,
                    index_sites,
                    cache,
                )?;
                let shift = builder
                    .build_int_mul(
                        shift,
                        shift.get_type().const_int(4, false),
                        "pv.meta.shift.amount",
                    )
                    .ok()?;
                let shift = fit(builder, shift, meta_width)?;
                match operation {
                    ProcessBinaryOp::Shl => {
                        builder.build_left_shift(left, shift, "pv.meta.shl").ok()
                    }
                    ProcessBinaryOp::Shr => builder
                        .build_right_shift(left, shift, false, "pv.meta.shr")
                        .ok(),
                    _ => unreachable!(),
                }
            }
            ProcessBinaryOp::ArithmeticShr => {
                let left_layout = packed_arithmetic_operand_layout(design, layout, *left)?;
                let (left_width, _) = packed_logic_layout(design, &left_layout)?;
                let left = process_packed_meta_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    *left,
                    &left_layout,
                    active,
                    index_sites,
                    cache,
                )?;
                let source_sign =
                    extract_region(builder, left, left_width.checked_sub(1)?.checked_mul(4)?, 4)?;
                let mut left = fit(builder, left, meta_width)?;
                for position in left_width..width {
                    left = insert_region(builder, left, source_sign, position.checked_mul(4)?, 4)?;
                }
                let shift = u32::try_from(process_constant_i64(design, *right)?).ok()?;
                let selected = shift.min(width);
                let mut result = if selected == 0 {
                    left
                } else {
                    builder
                        .build_right_shift(
                            left,
                            left.get_type()
                                .const_int(u64::from(selected.checked_mul(4)?), false),
                            false,
                            "pv.meta.ashr",
                        )
                        .ok()?
                };
                let sign = extract_region(builder, left, width.checked_sub(1)?.checked_mul(4)?, 4)?;
                for position in width.checked_sub(selected)?..width {
                    result = insert_region(builder, result, sign, position.checked_mul(4)?, 4)?;
                }
                Some(result)
            }
            _ => None,
        },
        ProcessValueKind::PackedSlice { base, left, right } => {
            let base_layout = process_value_layout(design, *base)?;
            let (base_width, _) = packed_logic_layout(design, base_layout)?;
            let base_meta = process_packed_meta_in_layout(
                context,
                module,
                builder,
                design,
                *base,
                base_layout,
                active,
                index_sites,
                cache,
            )?;
            let LayoutKind::Packed {
                range: Some(range), ..
            } = base_layout.kind
            else {
                return None;
            };
            if base_meta.get_type().get_bit_width() != base_width.checked_mul(4)? {
                return None;
            }
            let nibble = context
                .custom_width_int_type(std::num::NonZeroU32::new(4)?)
                .ok()?;
            let mut result = ty.const_zero();
            let step = if left <= right { 1i64 } else { -1i64 };
            let mut label = *left;
            for position in 0..width {
                let source = packed_label_position(range, label)?.checked_mul(4)?;
                let destination = width
                    .checked_sub(position.checked_add(1)?)?
                    .checked_mul(4)?;
                let discriminant = extract_region(builder, base_meta, source, 4)?;
                result = insert_region(
                    builder,
                    result,
                    fit(builder, discriminant, 4).unwrap_or_else(|| nibble.const_zero()),
                    destination,
                    4,
                )?;
                if position + 1 != width {
                    label = label.checked_add(step)?;
                }
            }
            Some(result)
        }
        ProcessValueKind::Index { base, index } if width == 1 => {
            let base_layout = process_value_layout(design, *base)?;
            packed_logic_layout(design, base_layout)?;
            let discriminant = packed_index_discriminant(
                context,
                module,
                builder,
                design,
                *base,
                *index,
                base_layout,
                4,
                active,
                index_sites,
                cache,
            )?;
            compact_discriminant(context, builder, encoding, discriminant)
        }
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        } => {
            let condition = process_value_at(
                context,
                module,
                builder,
                design,
                *condition,
                1,
                false,
                active,
                index_sites,
                cache,
            )?;
            let then_value = process_packed_meta_in_layout(
                context,
                module,
                builder,
                design,
                *then_value,
                layout,
                active,
                index_sites,
                cache,
            )?;
            let else_value = process_packed_meta_in_layout(
                context,
                module,
                builder,
                design,
                *else_value,
                layout,
                active,
                index_sites,
                cache,
            )?;
            builder
                .build_select(condition, then_value, else_value, "pv.meta.select")
                .ok()
                .map(|value| value.into_int_value())
        }
        ProcessValueKind::Match { scrutinee, arms } => {
            let scrutinee_value = process_value(
                context,
                module,
                builder,
                design,
                *scrutinee,
                active,
                index_sites,
                cache,
            )?;
            let eligible = process_match_eligibility(
                context,
                builder,
                design,
                *scrutinee,
                scrutinee_value,
                arms,
            )?;
            let mut result = None;
            for (arm, eligible) in arms.iter().zip(eligible).rev() {
                let value = process_packed_meta_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    arm.value,
                    layout,
                    active,
                    index_sites,
                    cache,
                )?;
                result = Some(match result {
                    Some(other) => builder
                        .build_select(eligible, value, other, "pv.meta.match")
                        .ok()?
                        .into_int_value(),
                    None => value,
                });
            }
            result
        }
        ProcessValueKind::Concat(parts) => {
            let LayoutKind::Packed {
                family,
                element_enum,
                ..
            } = &layout.kind
            else {
                return None;
            };
            let mut result = ty.const_zero();
            let mut offset = meta_width;
            for part in parts {
                let part_width = design.process_ir.values.get(part.0 as usize)?.bit_width?;
                let part_meta_width = part_width.checked_mul(4)?;
                offset = offset.checked_sub(part_meta_width)?;
                let part_layout = SourceLayout {
                    span: design.process_ir.values.get(part.0 as usize)?.span,
                    kind: LayoutKind::Packed {
                        width: part_width,
                        family: family.clone(),
                        range: Some(LayoutRange {
                            left: 0,
                            right: i64::from(part_width).checked_sub(1)?,
                        }),
                        element_enum: element_enum.clone(),
                    },
                };
                let metadata = process_packed_meta_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    *part,
                    &part_layout,
                    active,
                    index_sites,
                    cache,
                )?;
                result = insert_region(builder, result, metadata, offset, part_meta_width)?;
            }
            (offset == 0).then_some(result)
        }
        ProcessValueKind::RawResize { operand } => {
            let operand_layout = packed_operand_layout(design, layout, *operand)?;
            let (operand_width, _) = packed_logic_layout(design, &operand_layout)?;
            let operand = process_packed_meta_in_layout(
                context,
                module,
                builder,
                design,
                *operand,
                &operand_layout,
                active,
                index_sites,
                cache,
            )?;
            let fitted = fit(builder, operand, meta_width)?;
            (operand_width != 0).then_some(fitted)
        }
        _ => None,
    }
}

pub(super) fn packed_discriminant<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    encoding: &siox::ir::LogicEncoding,
    value_bit: IntValue<'ctx>,
    metadata: Option<IntValue<'ctx>>,
    width: u32,
) -> Option<IntValue<'ctx>> {
    let ty = context
        .custom_width_int_type(std::num::NonZeroU32::new(width)?)
        .ok()?;
    let low = ty.const_int(encoding.binary_value(false)?, false);
    let high = ty.const_int(encoding.binary_value(true)?, false);
    let clean = builder
        .build_select(value_bit, high, low, "pv.logic.binary")
        .ok()?
        .into_int_value();
    let Some(metadata) = metadata else {
        return Some(clean);
    };
    let metadata = fit(builder, metadata, width)?;
    let mut binary = context.bool_type().const_zero();
    for discriminant in &encoding.binary {
        let member = builder
            .build_int_compare(
                IntPredicate::EQ,
                metadata,
                ty.const_int(*discriminant, false),
                "pv.logic.binary.member",
            )
            .ok()?;
        binary = builder
            .build_or(binary, member, "pv.logic.binary.any")
            .ok()?;
    }
    builder
        .build_select(binary, clean, metadata, "pv.logic.discriminant")
        .ok()
        .map(|value| value.into_int_value())
}

/// The primary packed plane uses the source encoding's value bit, not the low
/// bit of the scalar enum discriminant. X/Z and weak states need not have the
/// same parity as their encoded value bit.
pub(super) fn logic_value_bit<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    encoding: &siox::ir::LogicEncoding,
    value: IntValue<'ctx>,
) -> Option<IntValue<'ctx>> {
    let mut high = encoding
        .value_bits
        .iter()
        .filter_map(|(&discriminant, &bit)| bit.then_some(discriminant))
        .collect::<Vec<_>>();
    high.sort_unstable();
    let mut result = context.bool_type().const_zero();
    for discriminant in high {
        let member = builder
            .build_int_compare(
                IntPredicate::EQ,
                value,
                value.get_type().const_int(discriminant, false),
                "pv.logic.value.member",
            )
            .ok()?;
        result = builder
            .build_or(result, member, "pv.logic.value.bit")
            .ok()?;
    }
    Some(result)
}

pub(super) fn compact_discriminant<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    encoding: &siox::ir::LogicEncoding,
    value: IntValue<'ctx>,
) -> Option<IntValue<'ctx>> {
    let nibble = context
        .custom_width_int_type(std::num::NonZeroU32::new(4)?)
        .ok()?;
    let value = fit(builder, value, 4)?;
    let mut binary = context.bool_type().const_zero();
    for discriminant in &encoding.binary {
        let member = builder
            .build_int_compare(
                IntPredicate::EQ,
                value,
                nibble.const_int(*discriminant, false),
                "pv.logic.compact.member",
            )
            .ok()?;
        binary = builder
            .build_or(binary, member, "pv.logic.compact.binary")
            .ok()?;
    }
    builder
        .build_select(binary, nibble.const_zero(), value, "pv.logic.compact")
        .ok()
        .map(|value| value.into_int_value())
}

/// Convert one scalar logic enum through the source-defined `LogicEncoding`
/// contracts. A binding may connect differently represented logic domains
/// (for example a four-bit `Logic` discriminant to a one-bit `Bit` port), so
/// raw truncation is not a representation-safe connection rule.
pub(super) fn convert_logic_scalar<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    source: &siox::ir::LogicEncoding,
    target: &siox::ir::LogicEncoding,
    value: IntValue<'ctx>,
    target_width: u32,
) -> Option<IntValue<'ctx>> {
    let source_width = value.get_type().get_bit_width();
    let source_ty = value.get_type();
    let target_ty = context
        .custom_width_int_type(std::num::NonZeroU32::new(target_width)?)
        .ok()?;
    let low = target_ty.const_int(target.binary_value(false)?, false);
    let mut converted = low;
    let mut entries = source.value_bits.iter().collect::<Vec<_>>();
    entries.sort_by_key(|(discriminant, _)| **discriminant);
    for (&discriminant, &bit) in entries.into_iter().rev() {
        let selected = target_ty.const_int(target.binary_value(bit)?, false);
        let member = builder
            .build_int_compare(
                IntPredicate::EQ,
                value,
                source_ty.const_int(discriminant, false),
                "pv.binding.logic.member",
            )
            .ok()?;
        converted = builder
            .build_select(member, selected, converted, "pv.binding.logic")
            .ok()?
            .into_int_value();
    }
    (source_width != 0).then_some(converted)
}

pub(super) fn adapt_binding_value<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    design: &Design,
    source_layout: &SourceLayout,
    target_layout: &SourceLayout,
    value: IntValue<'ctx>,
) -> Option<IntValue<'ctx>> {
    let (source_width, source) = scalar_logic_encoding(design, source_layout)?;
    let (target_width, target) = scalar_logic_encoding(design, target_layout)?;
    (value.get_type().get_bit_width() == source_width).then_some(())?;
    convert_logic_scalar(context, builder, source, target, value, target_width)
}

pub(super) fn reverse_meta_elements<'ctx>(
    builder: &Builder<'ctx>,
    value: IntValue<'ctx>,
) -> Option<IntValue<'ctx>> {
    let width = value.get_type().get_bit_width();
    if !width.is_multiple_of(4) {
        return None;
    }
    let mut reversed = value.get_type().const_zero();
    let elements = width / 4;
    for source in 0..elements {
        let discriminant = extract_region(builder, value, source.checked_mul(4)?, 4)?;
        let destination = elements
            .checked_sub(source.checked_add(1)?)?
            .checked_mul(4)?;
        reversed = insert_region(builder, reversed, discriminant, destination, 4)?;
    }
    Some(reversed)
}
