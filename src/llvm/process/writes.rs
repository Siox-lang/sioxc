//! Writing places and emitting schedule calls.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn write_static_place<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    place: StaticPlace,
    value: IntValue<'ctx>,
    metadata: Option<IntValue<'ctx>>,
    span: siox::diag::Span,
    range_sites: &HashMap<siox::diag::Span, u32>,
) -> Option<()> {
    let value = fit(builder, value, place.width)?;
    let value = if place.reverse {
        reverse_bits(builder, value)?
    } else {
        value
    };
    match place.root {
        StaticPlaceRoot::Local(process, local) => {
            let name = local_state_name(process, local);
            let value = if place.offset == 0 && place.width == place.root_width {
                value
            } else {
                insert_region(
                    builder,
                    state_value(context, module, builder, &name, place.root_width)?,
                    value,
                    place.offset,
                    place.width,
                )?
            };
            store_state(module, builder, &name, place.root_width, value)?;
            if let Some(meta_width) = local_meta_width(design, process, local) {
                let name = local_meta_name(process, local);
                let current = state_value(context, module, builder, &name, meta_width)?;
                let region_width = place.width.checked_mul(4)?;
                let metadata = metadata
                    .and_then(|metadata| fit(builder, metadata, region_width))
                    .unwrap_or_else(|| {
                        context
                            .custom_width_int_type(std::num::NonZeroU32::new(region_width).unwrap())
                            .expect("validated local metadata region")
                            .const_zero()
                    });
                let metadata = if place.reverse {
                    reverse_meta_elements(builder, metadata)?
                } else {
                    metadata
                };
                let updated = insert_region(
                    builder,
                    current,
                    metadata,
                    place.offset.checked_mul(4)?,
                    region_width,
                )?;
                store_state(module, builder, &name, meta_width, updated)?;
            }
            Some(())
        }
        StaticPlaceRoot::Storage(storage) => {
            let name = storage_state_name(storage);
            let current = state_value(context, module, builder, &name, place.root_width)?;
            let dirty_pointer = storage_dirty_ptr(
                context,
                module,
                builder,
                u32::try_from(design.process_ir.storages.len()).ok()?,
                storage.0,
            );
            let dirty = builder
                .build_load(context.i8_type(), dirty_pointer, "process.storage.dirty")
                .ok()?
                .into_int_value();
            let already_dirty = builder
                .build_int_compare(
                    IntPredicate::NE,
                    dirty,
                    context.i8_type().const_zero(),
                    "process.storage.already.dirty",
                )
                .ok()?;
            let old = state_value(
                context,
                module,
                builder,
                &storage_old_name(storage),
                place.root_width,
            )?;
            let snapshot = builder
                .build_select(already_dirty, old, current, "process.storage.snapshot")
                .ok()?
                .into_int_value();
            store_state(
                module,
                builder,
                &storage_old_name(storage),
                place.root_width,
                snapshot,
            )?;
            let metadata = storage_meta_width(design, storage).and_then(|meta_width| {
                let name = storage_meta_name(storage);
                let old_name = storage_meta_old_name(storage);
                let current = state_value(context, module, builder, &name, meta_width)?;
                let old = state_value(context, module, builder, &old_name, meta_width)?;
                let snapshot = builder
                    .build_select(already_dirty, old, current, "process.storage.meta.snapshot")
                    .ok()?
                    .into_int_value();
                store_state(module, builder, &old_name, meta_width, snapshot)?;
                let region_width = place.width.checked_mul(4)?;
                let region = metadata
                    .and_then(|metadata| fit(builder, metadata, region_width))
                    .unwrap_or_else(|| {
                        context
                            .custom_width_int_type(std::num::NonZeroU32::new(region_width).unwrap())
                            .expect("validated storage metadata region")
                            .const_zero()
                    });
                let region = if place.reverse {
                    reverse_meta_elements(builder, region)?
                } else {
                    region
                };
                let updated = insert_region(
                    builder,
                    current,
                    region,
                    place.offset.checked_mul(4)?,
                    region_width,
                )?;
                store_state(module, builder, &name, meta_width, updated)?;
                Some(updated)
            });
            builder
                .build_store(dirty_pointer, context.i8_type().const_int(1, false))
                .ok()?;
            let value = if place.offset == 0 && place.width == place.root_width {
                value
            } else {
                insert_region(builder, current, value, place.offset, place.width)?
            };
            store_state(module, builder, &name, place.root_width, value)?;
            stage_storage_value(
                context,
                module,
                builder,
                design,
                storage,
                value,
                span,
                range_sites,
            )?;
            if let Some(metadata) = metadata {
                stage_storage_metadata(module, builder, design, storage, metadata)?;
            }
            Some(())
        }
        StaticPlaceRoot::Signal(signal) => {
            let value = if place.offset == 0 && place.width == place.root_width {
                value
            } else {
                insert_region(
                    builder,
                    signal_value(
                        context,
                        module,
                        builder,
                        design,
                        &[signal],
                        ProcessSignalState::Current,
                        place.root_width,
                    )?,
                    value,
                    place.offset,
                    place.width,
                )?
            };
            if design.signals.get(signal.0 as usize)?.range.is_some() {
                let checked_width = place.root_width.max(64);
                let checked = if design.signals.get(signal.0 as usize)?.integer {
                    fit_signed(builder, value, checked_width)?
                } else {
                    fit(builder, value, checked_width)?
                };
                latch_range_failure(
                    context,
                    module,
                    builder,
                    design,
                    signal,
                    checked,
                    span,
                    range_sites,
                )?;
            }
            stage_signal(module, builder, signal, value)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn write_static_place_masked<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    place: StaticPlace,
    value: IntValue<'ctx>,
    mask: IntValue<'ctx>,
    span: siox::diag::Span,
    range_sites: &HashMap<siox::diag::Span, u32>,
) -> Option<()> {
    let mut value = fit(builder, value, place.width)?;
    let mut mask = fit(builder, mask, place.width)?;
    if place.reverse {
        value = reverse_bits(builder, value)?;
        mask = reverse_bits(builder, mask)?;
    }
    let root_type = context
        .custom_width_int_type(std::num::NonZeroU32::new(place.root_width)?)
        .ok()?;
    let root_value = insert_region(
        builder,
        root_type.const_zero(),
        value,
        place.offset,
        place.width,
    )?;
    let root_mask = insert_region(
        builder,
        root_type.const_zero(),
        mask,
        place.offset,
        place.width,
    )?;

    match place.root {
        StaticPlaceRoot::Signal(signal) => {
            let merged = stage_signal_masked(module, builder, signal, root_value, root_mask)?;
            if design.signals.get(signal.0 as usize)?.range.is_some() {
                let checked_width = place.root_width.max(64);
                let checked = if design.signals.get(signal.0 as usize)?.integer {
                    fit_signed(builder, merged, checked_width)?
                } else {
                    fit(builder, merged, checked_width)?
                };
                latch_range_failure(
                    context,
                    module,
                    builder,
                    design,
                    signal,
                    checked,
                    span,
                    range_sites,
                )?;
            }
            Some(())
        }
        StaticPlaceRoot::Storage(storage) => {
            let current = state_value(
                context,
                module,
                builder,
                &storage_state_name(storage),
                place.root_width,
            )?;
            let kept = builder
                .build_and(
                    current,
                    builder.build_not(root_mask, "process.schedule.keep").ok()?,
                    "process.schedule.kept",
                )
                .ok()?;
            let replacement = builder
                .build_and(root_value, root_mask, "process.schedule.replacement")
                .ok()?;
            let merged = builder
                .build_or(kept, replacement, "process.schedule.merged")
                .ok()?;
            write_static_place(
                context,
                module,
                builder,
                design,
                StaticPlace {
                    root: place.root,
                    root_width: place.root_width,
                    offset: 0,
                    width: place.root_width,
                    reverse: false,
                },
                merged,
                None,
                span,
                range_sites,
            )
        }
        StaticPlaceRoot::Local(_, _) => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn dynamic_place_offset<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    place: &DynamicPlace,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    let ty = context
        .custom_width_int_type(std::num::NonZeroU32::new(place.root_width)?)
        .ok()?;
    let mut offset = ty.const_int(u64::from(place.constant_offset), false);
    for projection in &place.indices {
        let position = dynamic_index_position(
            context,
            module,
            builder,
            design,
            projection.value,
            projection.range,
            projection.source_order,
            place.root_width,
            None,
            index_sites,
            cache,
        )?;
        let contribution = if projection.stride == 1 {
            position
        } else {
            builder
                .build_int_mul(
                    position,
                    ty.const_int(u64::from(projection.stride), false),
                    "process.place.stride",
                )
                .ok()?
        };
        offset = builder
            .build_int_add(offset, contribution, "process.place.offset")
            .ok()?;
    }
    Some(offset)
}

/// Apply one captured runtime-selected write as a single root
/// read/modify/write. The caller supplies the already-evaluated value,
/// metadata, and offset, so this mutation boundary cannot interleave target or
/// right-hand evaluation with publication. Overlapping aggregate copies
/// therefore observe the pre-write snapshot.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_dynamic_place<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    place: &DynamicPlace,
    value: IntValue<'ctx>,
    metadata: Option<IntValue<'ctx>>,
    offset: IntValue<'ctx>,
    span: siox::diag::Span,
    range_sites: &HashMap<siox::diag::Span, u32>,
) -> Option<()> {
    let value = fit(builder, value, place.width)?;
    match place.root {
        StaticPlaceRoot::Local(process, local) => {
            let name = local_state_name(process, local);
            let current = state_value(context, module, builder, &name, place.root_width)?;
            let value = insert_dynamic_region(builder, current, value, offset, place.width)?;
            store_state(module, builder, &name, place.root_width, value)?;
            if let Some(meta_width) = local_meta_width(design, process, local) {
                let name = local_meta_name(process, local);
                let current = state_value(context, module, builder, &name, meta_width)?;
                let region_width = place.width.checked_mul(4)?;
                let region = metadata
                    .and_then(|metadata| fit(builder, metadata, region_width))
                    .unwrap_or_else(|| {
                        context
                            .custom_width_int_type(std::num::NonZeroU32::new(region_width).unwrap())
                            .expect("validated local metadata region")
                            .const_zero()
                    });
                let meta_offset = builder
                    .build_int_mul(
                        offset,
                        offset.get_type().const_int(4, false),
                        "process.place.meta.offset",
                    )
                    .ok()?;
                let meta_offset = fit(builder, meta_offset, meta_width)?;
                let updated =
                    insert_dynamic_region(builder, current, region, meta_offset, region_width)?;
                store_state(module, builder, &name, meta_width, updated)?;
            }
            Some(())
        }
        StaticPlaceRoot::Storage(storage) => {
            let name = storage_state_name(storage);
            let current = state_value(context, module, builder, &name, place.root_width)?;
            let dirty_pointer = storage_dirty_ptr(
                context,
                module,
                builder,
                u32::try_from(design.process_ir.storages.len()).ok()?,
                storage.0,
            );
            let dirty = builder
                .build_load(context.i8_type(), dirty_pointer, "process.storage.dirty")
                .ok()?
                .into_int_value();
            let already_dirty = builder
                .build_int_compare(
                    IntPredicate::NE,
                    dirty,
                    context.i8_type().const_zero(),
                    "process.storage.already.dirty",
                )
                .ok()?;
            let old = state_value(
                context,
                module,
                builder,
                &storage_old_name(storage),
                place.root_width,
            )?;
            let snapshot = builder
                .build_select(already_dirty, old, current, "process.storage.snapshot")
                .ok()?
                .into_int_value();
            store_state(
                module,
                builder,
                &storage_old_name(storage),
                place.root_width,
                snapshot,
            )?;
            let metadata = storage_meta_width(design, storage).and_then(|meta_width| {
                let name = storage_meta_name(storage);
                let old_name = storage_meta_old_name(storage);
                let current = state_value(context, module, builder, &name, meta_width)?;
                let old = state_value(context, module, builder, &old_name, meta_width)?;
                let snapshot = builder
                    .build_select(already_dirty, old, current, "process.storage.meta.snapshot")
                    .ok()?
                    .into_int_value();
                store_state(module, builder, &old_name, meta_width, snapshot)?;
                let region_width = place.width.checked_mul(4)?;
                let region = metadata
                    .and_then(|metadata| fit(builder, metadata, region_width))
                    .unwrap_or_else(|| {
                        context
                            .custom_width_int_type(std::num::NonZeroU32::new(region_width).unwrap())
                            .expect("validated storage metadata region")
                            .const_zero()
                    });
                let meta_offset = builder
                    .build_int_mul(
                        offset,
                        offset.get_type().const_int(4, false),
                        "process.place.meta.offset",
                    )
                    .ok()?;
                let meta_offset = fit(builder, meta_offset, meta_width)?;
                let updated =
                    insert_dynamic_region(builder, current, region, meta_offset, region_width)?;
                store_state(module, builder, &name, meta_width, updated)?;
                Some(updated)
            });
            builder
                .build_store(dirty_pointer, context.i8_type().const_int(1, false))
                .ok()?;
            let value = insert_dynamic_region(builder, current, value, offset, place.width)?;
            store_state(module, builder, &name, place.root_width, value)?;
            stage_storage_value(
                context,
                module,
                builder,
                design,
                storage,
                value,
                span,
                range_sites,
            )?;
            if let Some(metadata) = metadata {
                stage_storage_metadata(module, builder, design, storage, metadata)?;
            }
            Some(())
        }
        StaticPlaceRoot::Signal(signal) => {
            let current = signal_value(
                context,
                module,
                builder,
                design,
                &[signal],
                ProcessSignalState::Current,
                place.root_width,
            )?;
            let value = insert_dynamic_region(builder, current, value, offset, place.width)?;
            if design.signals.get(signal.0 as usize)?.range.is_some() {
                let checked_width = place.root_width.max(64);
                let checked = if design.signals.get(signal.0 as usize)?.integer {
                    fit_signed(builder, value, checked_width)?
                } else {
                    fit(builder, value, checked_width)?
                };
                latch_range_failure(
                    context,
                    module,
                    builder,
                    design,
                    signal,
                    checked,
                    span,
                    range_sites,
                )?;
            }
            stage_signal(module, builder, signal, value)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_place_assignment<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    owner: ProcessId,
    semantics: ProcessAssignment,
    target: ProcessValueId,
    assigned: ProcessValueId,
    span: siox::diag::Span,
    index_sites: &HashMap<IndexSite, u32>,
    range_sites: &HashMap<siox::diag::Span, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<()> {
    if let Some(place) =
        static_place(design, target).filter(|place| place_has_semantics(*place, owner, semantics))
    {
        let mut storage_range_checks = Vec::new();
        if let StaticPlaceRoot::Storage(storage) = place.root {
            for binding in &design.process_ir.storages.get(storage.0 as usize)?.bindings {
                if !matches!(
                    binding.direction,
                    LayoutDirection::In | LayoutDirection::InOut
                ) || design
                    .signals
                    .get(binding.signal.0 as usize)?
                    .range
                    .is_none()
                {
                    continue;
                }
                let (offset, width) = storage_binding_slice(design, storage, &binding.projection)?;
                if offset != place.offset || width != place.width {
                    continue;
                }
                let assigned_width = design
                    .process_ir
                    .values
                    .get(assigned.0 as usize)?
                    .bit_width?;
                let checked_width = assigned_width
                    .max(design.signal_width(binding.signal)?)
                    .max(64);
                let checked = process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    assigned,
                    checked_width,
                    true,
                    None,
                    index_sites,
                    cache,
                )?;
                storage_range_checks.push((binding.signal, checked));
            }
        }
        let value = assignment_value(
            context,
            module,
            builder,
            design,
            target,
            assigned,
            place.width,
            index_sites,
            cache,
        )?;
        let metadata = assignment_metadata(
            context,
            module,
            builder,
            design,
            place.root,
            target,
            assigned,
            place.width,
            index_sites,
            cache,
        )?;
        write_static_place(
            context,
            module,
            builder,
            design,
            place,
            value,
            metadata,
            span,
            range_sites,
        )?;
        // `write_static_place` stages the narrowed storage image. Replace its
        // fallback check with the original mathematical RHS when this write
        // exactly targeted a ranged binding, so 8 assigned to a 4-bit
        // integer<0..10> remains 8 rather than sign-extending as -8.
        for (signal, checked) in storage_range_checks {
            latch_range_failure(
                context,
                module,
                builder,
                design,
                signal,
                checked,
                span,
                range_sites,
            )?;
        }
        return Some(());
    }
    let place = dynamic_place(design, target)
        .filter(|place| root_has_semantics(place.root, owner, semantics))?;
    let value = assignment_value(
        context,
        module,
        builder,
        design,
        target,
        assigned,
        place.width,
        index_sites,
        cache,
    )?;
    let metadata = assignment_metadata(
        context,
        module,
        builder,
        design,
        place.root,
        target,
        assigned,
        place.width,
        index_sites,
        cache,
    )?;
    let offset =
        dynamic_place_offset(context, module, builder, design, &place, index_sites, cache)?;
    write_dynamic_place(
        context,
        module,
        builder,
        design,
        &place,
        value,
        metadata,
        offset,
        span,
        range_sites,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_schedule_call<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    site: &ScheduleSite,
    target: ProcessValueId,
    value: ProcessValueId,
    delay: ProcessValueId,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<()> {
    let place = delayed_place(design, target)?;
    if place != site.place || place.width != site.width || site.lanes.is_empty() {
        return None;
    }
    let captured = assignment_value(
        context,
        module,
        builder,
        design,
        target,
        value,
        place.width,
        index_sites,
        cache,
    )?;
    let delay = process_value_at(
        context,
        module,
        builder,
        design,
        delay,
        64,
        false,
        None,
        index_sites,
        cache,
    )?;

    let i32 = context.i32_type();
    let i64 = context.i64_type();
    let pointer = context.ptr_type(AddressSpace::default());
    let word_count = super::super::words_for(place.width);
    let array = i64.array_type(word_count);
    let words = builder.build_alloca(array, "process.schedule.words").ok()?;
    for word in 0..word_count {
        let offset = word.checked_mul(super::super::ABI_WORD_BITS)?;
        let part = if offset == 0 {
            captured
        } else {
            builder
                .build_right_shift(
                    captured,
                    captured.get_type().const_int(u64::from(offset), false),
                    false,
                    "process.schedule.word.shift",
                )
                .ok()?
        };
        let part = fit(builder, part, 64)?;
        let destination = unsafe {
            builder
                .build_in_bounds_gep(
                    array,
                    words,
                    &[i32.const_zero(), i32.const_int(u64::from(word), false)],
                    "process.schedule.word.pointer",
                )
                .ok()?
        };
        builder.build_store(destination, part).ok()?;
    }

    let descriptor = |suffix: &str, values: Vec<u32>| {
        let ty = i32.array_type(u32::try_from(values.len()).ok()?);
        let global = module.add_global(ty, None, &format!("sx.schedule.{suffix}.{}", site.id));
        global.set_linkage(Linkage::Private);
        let values = values
            .into_iter()
            .map(|value| i32.const_int(u64::from(value), false))
            .collect::<Vec<_>>();
        global.set_initializer(&i32.const_array(&values));
        Some(global.as_pointer_value())
    };
    let waveforms = descriptor(
        "waveforms",
        site.lanes.iter().map(|lane| lane.waveform).collect(),
    )?;
    let offsets = descriptor(
        "offsets",
        site.lanes.iter().map(|lane| lane.offset).collect(),
    )?;
    let widths = descriptor("widths", site.lanes.iter().map(|lane| lane.width).collect())?;
    let lane_count = u32::try_from(site.lanes.len()).ok()?;

    let function = module
        .get_function("sx_runtime_schedule")
        .unwrap_or_else(|| {
            module.add_function(
                "sx_runtime_schedule",
                context.void_type().fn_type(
                    &[
                        i32.into(),
                        i64.into(),
                        pointer.into(),
                        i32.into(),
                        pointer.into(),
                        pointer.into(),
                        pointer.into(),
                        i32.into(),
                    ],
                    false,
                ),
                Some(Linkage::External),
            )
        });
    builder
        .build_call(
            function,
            &[
                i32.const_int(u64::from(site.id), false).into(),
                delay.into(),
                words.into(),
                i32.const_int(u64::from(word_count), false).into(),
                waveforms.into(),
                offsets.into(),
                widths.into(),
                i32.const_int(u64::from(lane_count), false).into(),
            ],
            "",
        )
        .ok()?;
    Some(())
}
