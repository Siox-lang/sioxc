//! Lowering a Process value to LLVM.

use super::*;

/// Emit a value using an expected recursive layout. Aggregates have no LLVM
/// ABI of their own; they are packed only inside the design object so field
/// reads, copies, and bindings share one exact-width representation.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_value_in_layout<'ctx>(
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
    let width = layout_width(layout)?;
    let cache_key = cache.key(id, active, Some(width));
    if let Some(value) = cache.emitted.get(&cache_key).copied() {
        return (value.get_type().get_bit_width() == width).then_some(value);
    }
    let value = design.process_ir.values.get(id.0 as usize)?;
    let emitted = match &value.kind {
        ProcessValueKind::Storage(storage) => match storage_state_width(design, *storage) {
            Some(storage_width) if storage_width == width => state_value(
                context,
                module,
                builder,
                &storage_state_name(*storage),
                width,
            )?,
            Some(_)
                if matches!(
                    layout.kind,
                    LayoutKind::Scalar { .. } | LayoutKind::Packed { .. }
                ) =>
            {
                process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    id,
                    width,
                    false,
                    active,
                    index_sites,
                    cache,
                )?
            }
            _ => return None,
        },
        ProcessValueKind::StorageState {
            storage,
            state: ProcessSignalState::Old,
        } => (storage_state_width(design, *storage) == Some(width))
            .then(|| state_value(context, module, builder, &storage_old_name(*storage), width))??,
        ProcessValueKind::Local { process, local } => match local_width(design, *process, *local) {
            Some(local_width) if local_width == width => state_value(
                context,
                module,
                builder,
                &local_state_name(*process, *local),
                width,
            )?,
            Some(_)
                if matches!(
                    layout.kind,
                    LayoutKind::Scalar { .. } | LayoutKind::Packed { .. }
                ) =>
            {
                process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    id,
                    width,
                    false,
                    active,
                    index_sites,
                    cache,
                )?
            }
            _ => return None,
        },
        ProcessValueKind::Signal { signals, state } => {
            let stored_width = if matches!(state, ProcessSignalState::Event) {
                1
            } else {
                signals.iter().try_fold(0u32, |total, signal| {
                    total.checked_add(design.signal_width(*signal)?)
                })?
            };
            if stored_width == width {
                aggregate_signal_value(context, module, builder, design, signals, *state, width)?
            } else if signals.len() == 1
                && matches!(
                    layout.kind,
                    LayoutKind::Scalar {
                        domain: siox::ir::ScalarDomain::Integer,
                        ..
                    }
                )
            {
                // Constrained integers use the leaf signal's physical width,
                // while their containing scalar layout uses the consumer's
                // mathematical width. Re-emit the scalar at that width so a
                // negative range sign-extends and a non-negative range
                // zero-extends. Packed resize remains raw bit-pattern logic.
                process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    id,
                    width,
                    true,
                    active,
                    index_sites,
                    cache,
                )?
            } else {
                return None;
            }
        }
        ProcessValueKind::Default => layout_default_value(context, builder, design, layout)?,
        ProcessValueKind::Field { base, field } => {
            let base_layout = process_value_layout(design, *base)?;
            let selected = field_slice(base_layout, field)?;
            if selected.width != width {
                return None;
            }
            let base = process_value_in_layout(
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
            extract_region(builder, base, selected.offset, selected.width)?
        }
        ProcessValueKind::Index { base, index } => {
            let base_layout = process_value_layout(design, *base)?;
            if let Some(index) = process_constant_i64(design, *index) {
                let selected = array_slice(base_layout, index)?;
                if selected.width != width {
                    return None;
                }
                let base = process_value_in_layout(
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
                extract_region(builder, base, selected.offset, selected.width)?
            } else {
                dynamic_index_region(
                    context,
                    module,
                    builder,
                    design,
                    *base,
                    *index,
                    base_layout,
                    width,
                    active,
                    index_sites,
                    cache,
                )?
            }
        }
        ProcessValueKind::String(text) => {
            if let LayoutKind::Packed {
                width: packed_width,
                element_enum: Some(element),
                ..
            } = &layout.kind
            {
                let encoding = design.logic_encodings.get(element)?;
                let symbols = design.enum_syms.get(element)?;
                let characters = text.chars().collect::<Vec<_>>();
                if u32::try_from(characters.len()).ok()? != *packed_width {
                    return None;
                }
                let ty = context
                    .custom_width_int_type(std::num::NonZeroU32::new(width)?)
                    .ok()?;
                let mut packed = ty.const_zero();
                for (position, character) in characters.into_iter().enumerate() {
                    let quoted = format!("'{character}'");
                    let discriminant = symbols.iter().find_map(|(discriminant, symbol)| {
                        (symbol == &quoted || symbol == &character.to_string())
                            .then_some(*discriminant)
                    })?;
                    if encoding.value_bit(discriminant)? == 0 {
                        continue;
                    }
                    let position = u32::try_from(position).ok()?;
                    let destination = packed_width.checked_sub(position.checked_add(1)?)?;
                    packed = insert_region(
                        builder,
                        packed,
                        context.bool_type().const_int(1, false),
                        destination,
                        1,
                    )?;
                }
                packed
            } else {
                let LayoutKind::Array {
                    range: Some(range),
                    element,
                } = &layout.kind
                else {
                    return None;
                };
                let characters = text.chars().collect::<Vec<_>>();
                if usize::try_from(range.len()?).ok()? != characters.len()
                    || !matches!(
                        element.kind,
                        LayoutKind::Scalar {
                            domain: siox::ir::ScalarDomain::Character,
                            ..
                        }
                    )
                {
                    return None;
                }
                let element_width = layout_width(element)?;
                let ty = context
                    .custom_width_int_type(std::num::NonZeroU32::new(width)?)
                    .ok()?;
                let element_ty = context
                    .custom_width_int_type(std::num::NonZeroU32::new(element_width)?)
                    .ok()?;
                let mut aggregate = ty.const_zero();
                for (position, character) in characters.into_iter().enumerate() {
                    aggregate = insert_region(
                        builder,
                        aggregate,
                        element_ty.const_int(u64::from(u32::from(character)), false),
                        u32::try_from(position).ok()?.checked_mul(element_width)?,
                        element_width,
                    )?;
                }
                aggregate
            }
        }
        ProcessValueKind::Construct { fields, spread, .. } => {
            let LayoutKind::Struct {
                fields: layout_fields,
                ..
            } = &layout.kind
            else {
                return None;
            };
            let mut aggregate = match spread {
                Some(spread) => process_value_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    *spread,
                    layout,
                    active,
                    index_sites,
                    cache,
                )?,
                None => layout_default_value(context, builder, design, layout)?,
            };
            let mut positional = 0usize;
            for field in fields {
                let field_index = match &field.name {
                    Some(name) => layout_fields
                        .iter()
                        .position(|candidate| candidate.name == *name)?,
                    None => {
                        let index = positional;
                        positional = positional.checked_add(1)?;
                        index
                    }
                };
                let field_layout = layout_fields.get(field_index)?;
                let field_value = process_value_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    field.value?,
                    &field_layout.layout,
                    active,
                    index_sites,
                    cache,
                )?;
                let selected = field_slice(layout, &field_layout.name)?;
                aggregate = insert_region(
                    builder,
                    aggregate,
                    field_value,
                    selected.offset,
                    selected.width,
                )?;
            }
            aggregate
        }
        ProcessValueKind::Array(elements) => {
            let LayoutKind::Array {
                range: Some(range),
                element,
            } = &layout.kind
            else {
                return None;
            };
            if usize::try_from(range.len()?).ok()? != elements.len() {
                return None;
            }
            let ty = context
                .custom_width_int_type(std::num::NonZeroU32::new(width)?)
                .ok()?;
            let mut aggregate = ty.const_zero();
            let element_width = layout_width(element)?;
            for (position, element_value) in elements.iter().enumerate() {
                let value = process_value_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    *element_value,
                    element,
                    active,
                    index_sites,
                    cache,
                )?;
                aggregate = insert_region(
                    builder,
                    aggregate,
                    value,
                    u32::try_from(position).ok()?.checked_mul(element_width)?,
                    element_width,
                )?;
            }
            aggregate
        }
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        } => {
            let condition = process_value(
                context,
                module,
                builder,
                design,
                *condition,
                active,
                index_sites,
                cache,
            )?;
            let condition = as_condition(builder, condition)?;
            let checked_arms =
                cache.contains_check(*then_value) || cache.contains_check(*else_value);
            let (then_active, else_active) = if checked_arms {
                let then_active = match active {
                    Some(outer) => builder
                        .build_and(outer, condition, "pv.aggregate.then.active")
                        .ok()?,
                    None => condition,
                };
                let not_condition = builder
                    .build_not(condition, "pv.aggregate.else.condition")
                    .ok()?;
                let else_active = match active {
                    Some(outer) => builder
                        .build_and(outer, not_condition, "pv.aggregate.else.active")
                        .ok()?,
                    None => not_condition,
                };
                (Some(then_active), Some(else_active))
            } else {
                (None, None)
            };
            let then_value = process_value_in_layout(
                context,
                module,
                builder,
                design,
                *then_value,
                layout,
                then_active,
                index_sites,
                cache,
            )?;
            let else_value = process_value_in_layout(
                context,
                module,
                builder,
                design,
                *else_value,
                layout,
                else_active,
                index_sites,
                cache,
            )?;
            builder
                .build_select(condition, then_value, else_value, "pv.aggregate.select")
                .ok()?
                .into_int_value()
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
                let arm_active = if cache.contains_check(arm.value) {
                    Some(match active {
                        Some(outer) => builder
                            .build_and(outer, eligible, "pv.aggregate.match.arm.active")
                            .ok()?,
                        None => eligible,
                    })
                } else {
                    None
                };
                let value = process_value_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    arm.value,
                    layout,
                    arm_active,
                    index_sites,
                    cache,
                )?;
                result = Some(match result {
                    Some(other) => builder
                        .build_select(eligible, value, other, "pv.aggregate.match.select")
                        .ok()?
                        .into_int_value(),
                    None => value,
                });
            }
            result?
        }
        _ => process_value_at(
            context,
            module,
            builder,
            design,
            id,
            width,
            false,
            active,
            index_sites,
            cache,
        )?,
    };
    cache.emitted.insert(cache_key, emitted);
    Some(emitted)
}

/// Emit the scalar subset needed by direct CFG control. Values are dependency
/// ordered, but the per-block cache also prevents a shared arena node from
/// being emitted more than once in one LLVM block. Foreign calls invalidate
/// cached signal reads because they may mutate state through the public ABI.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_value<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    id: ProcessValueId,
    active: Option<IntValue<'ctx>>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    let cache_key = cache.key(id, active, None);
    if let Some(value) = cache.emitted.get(&cache_key).copied() {
        return Some(value);
    }
    let value = design.process_ir.values.get(id.0 as usize)?;
    let width = value.bit_width?;
    if width > super::super::emit::LLVM_MAX_INT_BITS {
        return None;
    }
    let ty = context
        .custom_width_int_type(std::num::NonZeroU32::new(width)?)
        .ok()?;
    let emitted = match &value.kind {
        ProcessValueKind::Number(ProcessNumber::Integer(words))
        | ProcessValueKind::BitString { words, .. } => ty.const_int_arbitrary_precision(words),
        ProcessValueKind::Number(ProcessNumber::Real(bits)) => ty.const_int(*bits, false),
        ProcessValueKind::Char(character) => ty.const_int(u64::from(u32::from(*character)), false),
        ProcessValueKind::String(text) => {
            let characters = text.chars().collect::<Vec<_>>();
            let expected = u32::try_from(characters.len()).ok()?.checked_mul(32)?;
            if expected != width || width == 0 {
                return None;
            }
            let character_ty = context.i32_type();
            let mut aggregate = ty.const_zero();
            for (position, character) in characters.into_iter().enumerate() {
                aggregate = insert_region(
                    builder,
                    aggregate,
                    character_ty.const_int(u64::from(u32::from(character)), false),
                    u32::try_from(position).ok()?.checked_mul(32)?,
                    32,
                )?;
            }
            aggregate
        }
        ProcessValueKind::Signal { signals, state } => {
            let stored_width = if matches!(state, ProcessSignalState::Event) {
                1
            } else {
                signals.iter().try_fold(0u32, |total, signal| {
                    total.checked_add(design.signal_width(*signal)?)
                })?
            };
            let stored = if signals.len() == 1 {
                signal_value(
                    context,
                    module,
                    builder,
                    design,
                    signals,
                    *state,
                    stored_width,
                )?
            } else {
                aggregate_signal_value(
                    context,
                    module,
                    builder,
                    design,
                    signals,
                    *state,
                    stored_width,
                )?
            };
            if process_value_is_signed(design, id) {
                fit_signed(builder, stored, width)?
            } else {
                fit(builder, stored, width)?
            }
        }
        ProcessValueKind::Local { process, local } => {
            let stored_width = local_width(design, *process, *local)?;
            let stored = state_value(
                context,
                module,
                builder,
                &local_state_name(*process, *local),
                stored_width,
            )?;
            if process_value_is_signed(design, id) {
                fit_signed(builder, stored, width)?
            } else {
                fit(builder, stored, width)?
            }
        }
        ProcessValueKind::Storage(storage) => {
            let stored_width = storage_state_width(design, *storage)?;
            let stored = state_value(
                context,
                module,
                builder,
                &storage_state_name(*storage),
                stored_width,
            )?;
            if process_value_is_signed(design, id) {
                fit_signed(builder, stored, width)?
            } else {
                fit(builder, stored, width)?
            }
        }
        ProcessValueKind::StorageState { storage, state } => {
            process_storage_state_value(context, module, builder, design, *storage, *state, width)?
        }
        ProcessValueKind::Default => match process_value_layout(design, id) {
            Some(layout) if layout_width(layout) == Some(width) => {
                layout_default_value(context, builder, design, layout)?
            }
            _ => ty.const_zero(),
        },
        ProcessValueKind::Attribute { base, attribute } => {
            ty.const_int(process_layout_attribute(design, *base, attribute)?, false)
        }
        ProcessValueKind::BitSlice { base, high, low } => {
            // A slice introduced for a packed conversion is a raw resize even
            // when its family is named `signed`; its high bit is data. A
            // kernel-integer expression, including the select synthesized by
            // `sext`, is mathematical and must instead keep its sign while it
            // is evaluated at the selected width.
            let signed = !process_value_layout(design, *base)
                .is_some_and(|layout| matches!(layout.kind, LayoutKind::Packed { .. }))
                && process_value_is_signed(design, *base);
            if high < low {
                return None;
            }
            // A normalized kernel conversion is represented as a slice of
            // its mathematical operand. Evaluate that operand in at least
            // the selected width before slicing: computing `-5` in its
            // positive literal's natural i3 first produces `3`, which no
            // later extension can recover. `process_value_at` preserves an
            // already-wide packed operand and only re-evaluates arithmetic
            // when the slice genuinely asks for more bits.
            let required = high.checked_add(1)?;
            let base = process_value_at(
                context,
                module,
                builder,
                design,
                *base,
                required,
                signed,
                active,
                index_sites,
                cache,
            )?;
            let base_width = base.get_type().get_bit_width();
            if *low >= base_width {
                if !signed {
                    return Some(ty.const_zero());
                }
                let sign = builder
                    .build_right_shift(
                        base,
                        base.get_type().const_int(u64::from(base_width - 1), false),
                        true,
                        "pv.slice.sign",
                    )
                    .ok()?;
                return fit_signed(builder, sign, width);
            }
            let shifted = if *low == 0 {
                base
            } else {
                builder
                    .build_right_shift(
                        base,
                        base.get_type().const_int(u64::from(*low), false),
                        signed,
                        "pv.slice",
                    )
                    .ok()?
            };
            if signed {
                fit_signed(builder, shifted, width)?
            } else {
                fit(builder, shifted, width)?
            }
        }
        ProcessValueKind::PackedSlice { base, left, right } => packed_slice_value(
            context,
            module,
            builder,
            design,
            *base,
            *left,
            *right,
            active,
            index_sites,
            cache,
        )?,
        ProcessValueKind::CheckedIndex {
            index,
            valid,
            left,
            right,
            span,
        } => {
            let index_value = process_value(
                context,
                module,
                builder,
                design,
                *index,
                active,
                index_sites,
                cache,
            )?;
            let valid = process_value(
                context,
                module,
                builder,
                design,
                *valid,
                active,
                index_sites,
                cache,
            )?;
            let valid = as_condition(builder, valid)?;
            let site = index_sites.get(&IndexSite {
                span: *span,
                left: *left,
                right: *right,
            })?;
            let offending = if process_value_is_signed(design, *index) {
                fit_signed(builder, index_value, 64)?
            } else {
                fit(builder, index_value, 64)?
            };
            latch_index_failure(context, module, builder, valid, offending, active, *site)?;
            fit(builder, index_value, width)?
        }
        ProcessValueKind::TableLookup { table, index } => {
            let metadata = design.lookup_tables.get(table.0)?;
            let count = u64::try_from(metadata.values.len()).ok()?;
            let count_width = (64 - count.leading_zeros()).max(1);
            let index_width = design
                .process_ir
                .values
                .get(index.0 as usize)?
                .bit_width?
                .max(count_width);
            let index = process_value_at(
                context,
                module,
                builder,
                design,
                *index,
                index_width,
                false,
                active,
                index_sites,
                cache,
            )?;
            let in_range = builder
                .build_int_compare(
                    IntPredicate::ULT,
                    index,
                    index.get_type().const_int(count, false),
                    "pv.table.in_range",
                )
                .ok()?;
            let safe_index = builder
                .build_select(
                    in_range,
                    index,
                    index.get_type().const_zero(),
                    "pv.table.safe_index",
                )
                .ok()?
                .into_int_value();
            let safe_index = fit(builder, safe_index, 64)?;
            let storage_width = metadata.element_width.next_power_of_two().max(8);
            let storage = context
                .custom_width_int_type(std::num::NonZeroU32::new(storage_width)?)
                .ok()?;
            let array = storage.array_type(u32::try_from(metadata.values.len()).ok()?);
            let global = module.get_global(&format!("sx.lookup.{}", table.0))?;
            let pointer = unsafe {
                builder
                    .build_in_bounds_gep(
                        array,
                        global.as_pointer_value(),
                        &[context.i64_type().const_zero(), safe_index],
                        "pv.table.pointer",
                    )
                    .ok()?
            };
            let loaded = builder
                .build_load(storage, pointer, "pv.table.value")
                .ok()?
                .into_int_value();
            let selected = builder
                .build_select(in_range, loaded, storage.const_zero(), "pv.table.result")
                .ok()?
                .into_int_value();
            fit(builder, selected, width)?
        }
        ProcessValueKind::ForeignCall {
            name,
            arguments,
            float_arguments,
            integer_arguments,
            float_result,
            integer_result,
        } => {
            use inkwell::types::BasicMetadataTypeEnum as MetadataType;
            use inkwell::values::BasicMetadataValueEnum as MetadataValue;

            let float = context.f64_type();
            let mut parameter_types = Vec::<MetadataType>::with_capacity(arguments.len());
            let mut argument_values = Vec::<MetadataValue>::with_capacity(arguments.len());
            for (index, argument) in arguments.iter().enumerate() {
                let argument = process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    *argument,
                    64,
                    integer_arguments.get(index).copied().unwrap_or(false),
                    active,
                    index_sites,
                    cache,
                )?;
                if float_arguments.get(index).copied().unwrap_or(false) {
                    parameter_types.push(float.into());
                    argument_values.push(
                        builder
                            .build_bit_cast(argument, float, "pv.foreign.float_argument")
                            .ok()?
                            .into_float_value()
                            .into(),
                    );
                } else {
                    parameter_types.push(context.i64_type().into());
                    argument_values.push(argument.into());
                }
            }
            let function = module.get_function(name).unwrap_or_else(|| {
                let signature = if *float_result {
                    float.fn_type(&parameter_types, false)
                } else {
                    context.i64_type().fn_type(&parameter_types, false)
                };
                module.add_function(name, signature, Some(Linkage::External))
            });
            let returned = match builder
                .build_call(function, &argument_values, "pv.foreign")
                .ok()?
                .try_as_basic_value()
            {
                inkwell::values::ValueKind::Basic(value) => value,
                _ => return None,
            };
            // Foreign code may call public signal accessors. Later arena reads
            // must observe that mutation rather than reuse an earlier load.
            cache.clear();
            let returned = if *float_result {
                builder
                    .build_bit_cast(
                        returned.into_float_value(),
                        context.i64_type(),
                        "pv.foreign.float_result",
                    )
                    .ok()?
                    .into_int_value()
            } else {
                returned.into_int_value()
            };
            if *integer_result {
                fit_signed(builder, returned, width)?
            } else {
                fit(builder, returned, width)?
            }
        }
        ProcessValueKind::HostCall {
            operation,
            arguments,
        } => {
            use inkwell::values::BasicMetadataValueEnum as MetadataValue;

            let i64 = context.i64_type();
            let pointer = context.ptr_type(AddressSpace::default());
            let scalar_argument = |argument: ProcessValueId,
                                   cache: &mut ProcessValueCache<'ctx, '_>|
             -> Option<MetadataValue<'ctx>> {
                process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    argument,
                    64,
                    false,
                    active,
                    index_sites,
                    cache,
                )
                .map(Into::into)
            };
            let string_argument = |argument: ProcessValueId| -> Option<MetadataValue<'ctx>> {
                let text = process_string(design, argument)?;
                Some(
                    private_string(
                        context,
                        module,
                        &format!("sx.host.string.{}", argument.0),
                        text,
                    )
                    .into(),
                )
            };
            let (name, signature, emitted) = match operation {
                ProcessHostValueOp::Random => {
                    ("sx_runtime_rand", i64.fn_type(&[], false), Vec::new())
                }
                ProcessHostValueOp::RandomRange => {
                    let [left, right] = arguments.as_slice() else {
                        return None;
                    };
                    (
                        "sx_runtime_randint",
                        i64.fn_type(&[i64.into(), i64.into()], false),
                        vec![
                            scalar_argument(*left, cache)?,
                            scalar_argument(*right, cache)?,
                        ],
                    )
                }
                ProcessHostValueOp::Uniform => {
                    ("sx_runtime_uniform", i64.fn_type(&[], false), Vec::new())
                }
                ProcessHostValueOp::ReadUtf8 => {
                    let [path] = arguments.as_slice() else {
                        return None;
                    };
                    (
                        "sx_runtime_read_utf8",
                        i64.fn_type(&[pointer.into()], false),
                        vec![string_argument(*path)?],
                    )
                }
                ProcessHostValueOp::ReadUtf8Fixed => {
                    let [path] = arguments.as_slice() else {
                        return None;
                    };
                    if !width.is_multiple_of(32) {
                        return None;
                    }
                    return emit_buffered_read_value(
                        context,
                        module,
                        builder,
                        design,
                        *path,
                        width,
                        "sx_runtime_read_utf8_fixed",
                        width / 32,
                    );
                }
                ProcessHostValueOp::ReadBinary => {
                    let [path] = arguments.as_slice() else {
                        return None;
                    };
                    return emit_buffered_read_value(
                        context,
                        module,
                        builder,
                        design,
                        *path,
                        width,
                        "sx_runtime_read_binary",
                        width.div_ceil(8),
                    );
                }
                ProcessHostValueOp::FileExists => {
                    let [path] = arguments.as_slice() else {
                        return None;
                    };
                    (
                        "sx_runtime_file_exists",
                        i64.fn_type(&[pointer.into()], false),
                        vec![string_argument(*path)?],
                    )
                }
                ProcessHostValueOp::StringLength => {
                    let [handle] = arguments.as_slice() else {
                        return None;
                    };
                    (
                        "sx_runtime_string_length",
                        i64.fn_type(&[i64.into()], false),
                        vec![scalar_argument(*handle, cache)?],
                    )
                }
                ProcessHostValueOp::StringIndex => {
                    let [handle, index] = arguments.as_slice() else {
                        return None;
                    };
                    (
                        "sx_runtime_string_index_at",
                        i64.fn_type(
                            &[
                                i64.into(),
                                i64.into(),
                                context.i32_type().into(),
                                context.i32_type().into(),
                            ],
                            false,
                        ),
                        vec![
                            scalar_argument(*handle, cache)?,
                            scalar_argument(*index, cache)?,
                            context
                                .i32_type()
                                .const_int(u64::from(value.span.file.0), false)
                                .into(),
                            context
                                .i32_type()
                                .const_int(u64::from(value.span.start), false)
                                .into(),
                        ],
                    )
                }
                ProcessHostValueOp::StringEqualsUtf8 => {
                    let [handle, literal] = arguments.as_slice() else {
                        return None;
                    };
                    (
                        "sx_runtime_string_equals_utf8",
                        i64.fn_type(&[i64.into(), pointer.into()], false),
                        vec![scalar_argument(*handle, cache)?, string_argument(*literal)?],
                    )
                }
            };
            let function = module
                .get_function(name)
                .unwrap_or_else(|| module.add_function(name, signature, Some(Linkage::External)));
            let returned = match builder
                .build_call(function, &emitted, "pv.host")
                .ok()?
                .try_as_basic_value()
            {
                inkwell::values::ValueKind::Basic(value) => value.into_int_value(),
                _ => return None,
            };
            fit(builder, returned, width)?
        }
        ProcessValueKind::Unary { operation, operand } => match operation {
            ProcessUnaryOp::Neg => {
                let operand_id = *operand;
                let operand = process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    *operand,
                    width,
                    false,
                    active,
                    index_sites,
                    cache,
                )?;
                if process_value_is_real(design, operand_id) {
                    let real = builder
                        .build_bit_cast(operand, context.f64_type(), "pv.neg.real.bits")
                        .ok()?
                        .into_float_value();
                    let negated = builder.build_float_neg(real, "pv.neg.real").ok()?;
                    let bits = builder
                        .build_bit_cast(negated, context.i64_type(), "pv.neg.real.result")
                        .ok()?
                        .into_int_value();
                    fit(builder, bits, width)?
                } else {
                    builder.build_int_neg(operand, "pv.neg").ok()?
                }
            }
            ProcessUnaryOp::Not => {
                let operand = process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    *operand,
                    width,
                    false,
                    active,
                    index_sites,
                    cache,
                )?;
                // `not` is Boolean per bit: on a scalar Bool/Bit this is the
                // familiar truth complement, while a packed vector inverts
                // every element. Width-one values make both descriptions the
                // same operation. Comparing the whole operand with zero was a
                // scalar-only shortcut that turned `not 0b11001000` into 0.
                builder.build_not(operand, "pv.not").ok()?
            }
            ProcessUnaryOp::RealToInteger => {
                let operand = process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    *operand,
                    64,
                    false,
                    active,
                    index_sites,
                    cache,
                )?;
                let real = builder
                    .build_bit_cast(operand, context.f64_type(), "pv.real.bits")
                    .ok()?
                    .into_float_value();
                let integer = builder
                    .build_float_to_signed_int(real, context.i64_type(), "pv.real.integer")
                    .ok()?;
                fit_signed(builder, integer, width)?
            }
            ProcessUnaryOp::IntegerToReal => {
                let operand = process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    *operand,
                    64,
                    true,
                    active,
                    index_sites,
                    cache,
                )?;
                let real = builder
                    .build_signed_int_to_float(operand, context.f64_type(), "pv.integer.real")
                    .ok()?;
                builder
                    .build_bit_cast(real, context.i64_type(), "pv.real.bits")
                    .ok()?
                    .into_int_value()
            }
        },
        ProcessValueKind::RawResize { operand } => {
            let operand = process_value(
                context,
                module,
                builder,
                design,
                *operand,
                active,
                index_sites,
                cache,
            )?;
            fit(builder, operand, width)?
        }
        ProcessValueKind::Binary {
            operation,
            left,
            right,
        } => match process_empty_string_comparison(design, operation, *left, *right) {
            Some(result) => ty.const_int(u64::from(result), false),
            None => process_binary(
                context,
                module,
                builder,
                design,
                operation,
                *left,
                *right,
                width,
                active,
                index_sites,
                cache,
            )?,
        },
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        } => process_select(
            context,
            module,
            builder,
            design,
            *condition,
            *then_value,
            *else_value,
            width,
            active,
            index_sites,
            cache,
        )?,
        ProcessValueKind::MetaCompare {
            not_equal,
            operands,
            inner,
        } => {
            let ordinary = process_value(
                context,
                module,
                builder,
                design,
                *inner,
                active,
                index_sites,
                cache,
            )?;
            let ordinary = as_condition(builder, ordinary)?;
            let mut unknown = context.bool_type().const_zero();
            for operand in operands {
                let operand_unknown = process_meta_operand_unknown(
                    context,
                    module,
                    builder,
                    design,
                    *operand,
                    active,
                    index_sites,
                    cache,
                )?;
                unknown = builder
                    .build_or(unknown, operand_unknown, "pv.meta.operand")
                    .ok()?;
            }
            let result = if *not_equal {
                builder.build_or(ordinary, unknown, "pv.meta.ne").ok()?
            } else {
                let known = builder.build_not(unknown, "pv.meta.known").ok()?;
                builder.build_and(ordinary, known, "pv.meta.compare").ok()?
            };
            fit(builder, result, width)?
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
                let arm_active = if cache.contains_check(arm.value) {
                    Some(match active {
                        Some(outer) => builder
                            .build_and(outer, eligible, "pv.match.arm.active")
                            .ok()?,
                        None => eligible,
                    })
                } else {
                    None
                };
                let value = process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    arm.value,
                    width,
                    true,
                    arm_active,
                    index_sites,
                    cache,
                )?;
                result = Some(match result {
                    Some(other) => builder
                        .build_select(eligible, value, other, "pv.match.select")
                        .ok()?
                        .into_int_value(),
                    None => value,
                });
            }
            result?
        }
        ProcessValueKind::Field { .. } => {
            let layout = process_value_layout(design, id)?;
            if layout_width(layout)? != width {
                return None;
            }
            process_value_in_layout(
                context,
                module,
                builder,
                design,
                id,
                layout,
                active,
                index_sites,
                cache,
            )?
        }
        ProcessValueKind::Index { base, index } => {
            let base_layout = process_value_layout(design, *base)?;
            match &base_layout.kind {
                LayoutKind::Array { .. } => {
                    let layout = process_value_layout(design, id)?;
                    if layout_width(layout)? != width {
                        return None;
                    }
                    process_value_in_layout(
                        context,
                        module,
                        builder,
                        design,
                        id,
                        layout,
                        active,
                        index_sites,
                        cache,
                    )?
                }
                LayoutKind::Packed {
                    element_enum: Some(_),
                    range: Some(_),
                    ..
                } => packed_index_discriminant(
                    context,
                    module,
                    builder,
                    design,
                    *base,
                    *index,
                    base_layout,
                    width,
                    active,
                    index_sites,
                    cache,
                )?,
                LayoutKind::Packed { range: Some(_), .. } if width == 1 => dynamic_index_region(
                    context,
                    module,
                    builder,
                    design,
                    *base,
                    *index,
                    base_layout,
                    width,
                    active,
                    index_sites,
                    cache,
                )?,
                _ => return None,
            }
        }
        ProcessValueKind::Concat(parts) => {
            let mut joined = ty.const_zero();
            let mut offset = width;
            for part in parts {
                let part_width = design.process_ir.values.get(part.0 as usize)?.bit_width?;
                offset = offset.checked_sub(part_width)?;
                let part = process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    *part,
                    width,
                    false,
                    active,
                    index_sites,
                    cache,
                )?;
                let part = if offset == 0 {
                    part
                } else {
                    builder
                        .build_left_shift(
                            part,
                            ty.const_int(u64::from(offset), false),
                            "pv.concat.place",
                        )
                        .ok()?
                };
                joined = builder.build_or(joined, part, "pv.concat").ok()?;
            }
            if offset != 0 {
                return None;
            }
            joined
        }
        _ => return None,
    };
    cache.emitted.insert(cache_key, emitted);
    Some(emitted)
}
