//! Change and dirty flags, and the emitted state helper functions.

use super::*;

pub(super) fn storage_changed_ptr<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    count: u32,
    storage: u32,
) -> PointerValue<'ctx> {
    storage_flag_ptr_at(
        context,
        module,
        builder,
        count,
        context.i32_type().const_int(u64::from(storage), false),
        "sx.process.storage.changed",
    )
}

pub(super) fn storage_changed_ptr_at<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    count: u32,
    storage: IntValue<'ctx>,
) -> PointerValue<'ctx> {
    storage_flag_ptr_at(
        context,
        module,
        builder,
        count,
        storage,
        "sx.process.storage.changed",
    )
}

pub(super) fn storage_dirty_ptr<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    count: u32,
    storage: u32,
) -> PointerValue<'ctx> {
    storage_flag_ptr_at(
        context,
        module,
        builder,
        count,
        context.i32_type().const_int(u64::from(storage), false),
        "sx.process.storage.dirty",
    )
}

pub(super) fn storage_flag_ptr_at<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    count: u32,
    storage: IntValue<'ctx>,
    name: &str,
) -> PointerValue<'ctx> {
    let byte = context.i8_type();
    let table = byte.array_type(count.max(1));
    let global = module
        .get_global(name)
        .expect("process storage change table");
    unsafe {
        builder
            .build_in_bounds_gep(
                table,
                global.as_pointer_value(),
                &[context.i32_type().const_zero(), storage],
                "process.storage.changed.pointer",
            )
            .expect("valid storage change pointer")
    }
}

/// Fill the process-frame reset/commit helpers declared before the design ABI
/// and expose storage-change queries beside signal-change queries.
pub(super) fn emit_state_helpers<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    design: &Design,
    supported_values: &ProcessValueSupport,
    checked_values: &[bool],
) {
    let builder = context.create_builder();
    let byte = context.i8_type();
    let storage_count = u32::try_from(design.process_ir.storages.len())
        .expect("ProcessStorageId is a u32 ABI index");
    let index_sites = design
        .index_sites()
        .into_iter()
        .enumerate()
        .map(|(index, site)| (site, index as u32 + 1))
        .collect::<HashMap<_, _>>();

    let reset = module
        .get_function("sx.process.reset")
        .expect("process reset declaration");
    builder.position_at_end(context.append_basic_block(reset, "entry"));
    let selected_root = reset
        .get_first_param()
        .expect("process reset has a selected root")
        .into_int_value();
    let all_roots = builder
        .build_int_compare(
            IntPredicate::EQ,
            selected_root,
            context.i32_type().const_all_ones(),
            "process.reset.all_roots",
        )
        .unwrap();
    for process in &design.process_ir.processes {
        for local in &process.locals {
            let Some(width) = local_width(design, process.id, local.id) else {
                continue;
            };
            let ty = context
                .custom_width_int_type(std::num::NonZeroU32::new(width).unwrap())
                .expect("validated local width");
            store_state(
                module,
                &builder,
                &local_state_name(process.id, local.id),
                width,
                ty.const_zero(),
            )
            .expect("declared process local state");
            if let Some(meta_width) = local_meta_width(design, process.id, local.id) {
                let meta_ty = context
                    .custom_width_int_type(std::num::NonZeroU32::new(meta_width).unwrap())
                    .expect("validated local metadata width");
                store_state(
                    module,
                    &builder,
                    &local_meta_name(process.id, local.id),
                    meta_width,
                    meta_ty.const_zero(),
                )
                .expect("declared process local metadata");
            }
        }
        for block in &process.blocks {
            let ProcessTerminator::For { iterable, .. } = &block.terminator else {
                continue;
            };
            let array = array_loop_shape(design, *iterable);
            let dynamic_string = dynamic_string_value(design, *iterable);
            if range_loop_bounds(design, *iterable).is_none() && array.is_none() && !dynamic_string
            {
                continue;
            }
            for (name, width) in [
                (loop_active_name(process.id, block.id), 1),
                (loop_cursor_name(process.id, block.id), 64),
                (loop_end_name(process.id, block.id), 64),
            ] {
                let ty = context
                    .custom_width_int_type(std::num::NonZeroU32::new(width).unwrap())
                    .expect("validated loop state width");
                store_state(module, &builder, &name, width, ty.const_zero())
                    .expect("declared process loop state");
            }
            if let Some((layout, _, _)) = array {
                let width = layout_width(layout).expect("declared array loop state width");
                let ty = context
                    .custom_width_int_type(std::num::NonZeroU32::new(width).unwrap())
                    .expect("validated array loop state width");
                store_state(
                    module,
                    &builder,
                    &loop_iterable_name(process.id, block.id),
                    width,
                    ty.const_zero(),
                )
                .expect("declared process array-loop state");
            } else if dynamic_string {
                store_state(
                    module,
                    &builder,
                    &loop_iterable_name(process.id, block.id),
                    64,
                    context.i64_type().const_zero(),
                )
                .expect("declared process dynamic-string loop state");
            }
        }
    }
    let mut cache = ProcessValueCache::new(checked_values, &supported_values.meta_free);
    for storage in &design.process_ir.storages {
        builder
            .build_store(
                storage_changed_ptr(context, module, &builder, storage_count, storage.id.0),
                byte.const_zero(),
            )
            .unwrap();
        builder
            .build_store(
                storage_dirty_ptr(context, module, &builder, storage_count, storage.id.0),
                byte.const_zero(),
            )
            .unwrap();
        let Some(width) = storage_state_width(design, storage.id) else {
            continue;
        };
        let ty = context
            .custom_width_int_type(std::num::NonZeroU32::new(width).unwrap())
            .expect("validated storage width");
        let initialized = storage
            .initializer
            .filter(|initializer| {
                storage.layout.as_ref().map_or_else(
                    || {
                        supported_values
                            .get(initializer.0 as usize)
                            .copied()
                            .unwrap_or(false)
                    },
                    |layout| {
                        if layout_width(layout).is_none() {
                            supported_values
                                .get(initializer.0 as usize)
                                .copied()
                                .unwrap_or(false)
                        } else {
                            process_value_supported_in_layout(
                                design,
                                *initializer,
                                layout,
                                supported_values,
                            )
                        }
                    },
                )
            })
            .and_then(|initializer| {
                if let Some(layout) = storage.layout.as_ref() {
                    if layout_width(layout).is_some() {
                        process_value_in_layout(
                            context,
                            module,
                            &builder,
                            design,
                            initializer,
                            layout,
                            None,
                            &index_sites,
                            &mut cache,
                        )
                    } else {
                        process_value_at(
                            context,
                            module,
                            &builder,
                            design,
                            initializer,
                            width,
                            false,
                            None,
                            &index_sites,
                            &mut cache,
                        )
                    }
                } else {
                    process_value_at(
                        context,
                        module,
                        &builder,
                        design,
                        initializer,
                        width,
                        false,
                        None,
                        &index_sites,
                        &mut cache,
                    )
                }
            });
        if initialized.is_some()
            && storage
                .initializer
                .is_some_and(|initializer| initializer_can_raise_host_error(design, initializer))
        {
            let i32 = context.i32_type();
            let note = module
                .get_function("sx_runtime_note_location")
                .unwrap_or_else(|| {
                    module.add_function(
                        "sx_runtime_note_location",
                        context
                            .void_type()
                            .fn_type(&[i32.into(), i32.into()], false),
                        Some(Linkage::External),
                    )
                });
            builder
                .build_call(
                    note,
                    &[
                        i32.const_int(u64::from(storage.span.file.0), false).into(),
                        i32.const_int(u64::from(storage.span.start), false).into(),
                    ],
                    "",
                )
                .expect("valid runtime initializer location call");
        }
        let mut value = if let Some(initialized) = initialized {
            initialized
        } else if let Some(layout) = storage.layout.as_ref() {
            layout_default_value(context, &builder, design, layout)
                .unwrap_or_else(|| ty.const_zero())
        } else {
            ty.const_int(storage_default(design, storage.id), false)
        };
        let meta_width = storage_meta_width(design, storage.id);
        let mut metadata = meta_width.map(|meta_width| {
            storage
                .initializer
                .zip(storage.layout.as_ref())
                .and_then(|(initializer, layout)| {
                    process_packed_meta_in_layout(
                        context,
                        module,
                        &builder,
                        design,
                        initializer,
                        layout,
                        None,
                        &index_sites,
                        &mut cache,
                    )
                })
                .unwrap_or_else(|| {
                    context
                        .custom_width_int_type(std::num::NonZeroU32::new(meta_width).unwrap())
                        .expect("validated storage metadata width")
                        .const_zero()
                })
        });
        // Pure output fields are observations, not reset-time drivers. Read
        // their already-reset DUT leaves into the packed storage object.
        for binding in &storage.bindings {
            if !matches!(binding.direction, LayoutDirection::Out) {
                continue;
            }
            let (offset, binding_width) =
                storage_binding_slice(design, storage.id, &binding.projection)
                    .expect("validated storage binding");
            let signal_width = design
                .signal_width(binding.signal)
                .expect("validated observed signal width");
            let mut observed = signal_value(
                context,
                module,
                &builder,
                design,
                &[binding.signal],
                ProcessSignalState::Current,
                signal_width,
            )
            .expect("validated observed storage binding");
            if signal_width != binding_width {
                let source = signal_layout(design, &[binding.signal])
                    .expect("validated observed signal layout");
                let target = storage_binding_layout_slice(design, storage.id, &binding.projection)
                    .expect("validated observed storage layout")
                    .layout;
                observed = adapt_binding_value(context, &builder, design, source, target, observed)
                    .expect("validated observed logic conversion");
            }
            value = insert_region(&builder, value, observed, offset, binding_width)
                .expect("validated observed storage region");
            if let Some(meta) = metadata.as_mut() {
                let binding_meta_width = binding_width
                    .checked_mul(4)
                    .expect("validated observed metadata width");
                let observed = design
                    .meta_of
                    .get(&binding.signal.0)
                    .copied()
                    .map(|companion| {
                        signal_value(
                            context,
                            module,
                            &builder,
                            design,
                            &[SignalId(companion)],
                            ProcessSignalState::Current,
                            binding_meta_width,
                        )
                        .expect("validated observed metadata binding")
                    })
                    .unwrap_or_else(|| {
                        context
                            .custom_width_int_type(
                                std::num::NonZeroU32::new(binding_meta_width).unwrap(),
                            )
                            .expect("validated observed metadata type")
                            .const_zero()
                    });
                *meta = insert_region(
                    &builder,
                    *meta,
                    observed,
                    offset.checked_mul(4).expect("validated metadata offset"),
                    binding_meta_width,
                )
                .expect("validated observed metadata region");
            }
        }
        store_state(
            module,
            &builder,
            &storage_state_name(storage.id),
            width,
            value,
        )
        .expect("declared process storage state");
        store_state(
            module,
            &builder,
            &storage_old_name(storage.id),
            width,
            value,
        )
        .expect("declared process storage snapshot");
        if let (Some(meta_width), Some(metadata)) = (meta_width, metadata) {
            store_state(
                module,
                &builder,
                &storage_meta_name(storage.id),
                meta_width,
                metadata,
            )
            .expect("declared process storage metadata");
            store_state(
                module,
                &builder,
                &storage_meta_old_name(storage.id),
                meta_width,
                metadata,
            )
            .expect("declared process storage metadata snapshot");
        }
        let drives = storage.bindings.iter().any(|binding| {
            matches!(
                binding.direction,
                LayoutDirection::In | LayoutDirection::InOut
            )
        });
        let next = drives.then(|| {
            let write =
                context.append_basic_block(reset, &format!("storage{}.bindings", storage.id.0));
            let next = context.append_basic_block(reset, &format!("storage{}.next", storage.id.0));
            let selected = builder
                .build_int_compare(
                    IntPredicate::EQ,
                    selected_root,
                    context
                        .i32_type()
                        .const_int(u64::from(storage.owner.0), false),
                    "process.reset.selected_root",
                )
                .unwrap();
            let enabled = builder
                .build_or(all_roots, selected, "process.reset.binding_enabled")
                .unwrap();
            builder
                .build_conditional_branch(enabled, write, next)
                .unwrap();
            builder.position_at_end(write);
            next
        });
        for binding in &storage.bindings {
            if !matches!(
                binding.direction,
                LayoutDirection::In | LayoutDirection::InOut
            ) {
                continue;
            }
            let (offset, binding_width) =
                storage_binding_slice(design, storage.id, &binding.projection)
                    .expect("validated storage binding");
            let mut staged = extract_region(&builder, value, offset, binding_width)
                .expect("validated writable storage region");
            let signal_width = design
                .signal_width(binding.signal)
                .expect("validated writable signal width");
            if signal_width != binding_width {
                let source = storage_binding_layout_slice(design, storage.id, &binding.projection)
                    .expect("validated writable storage layout")
                    .layout;
                let target = signal_layout(design, &[binding.signal])
                    .expect("validated writable signal layout");
                staged = adapt_binding_value(context, &builder, design, source, target, staged)
                    .expect("validated writable logic conversion");
            }
            stage_signal(module, &builder, binding.signal, staged)
                .expect("validated storage binding signal");
            if let (Some(metadata), Some(companion)) =
                (metadata, design.meta_of.get(&binding.signal.0).copied())
            {
                let binding_meta_width = binding_width
                    .checked_mul(4)
                    .expect("validated writable metadata width");
                let staged = extract_region(
                    &builder,
                    metadata,
                    offset.checked_mul(4).expect("validated metadata offset"),
                    binding_meta_width,
                )
                .expect("validated writable metadata region");
                stage_signal(module, &builder, SignalId(companion), staged)
                    .expect("validated storage metadata binding");
            }
        }
        if let Some(next) = next {
            builder.build_unconditional_branch(next).unwrap();
            builder.position_at_end(next);
        }
        cache.clear();
    }
    builder.build_return(None).unwrap();

    let commit = module
        .get_function("sx.process.commit.storage")
        .expect("process storage commit declaration");
    builder.position_at_end(context.append_basic_block(commit, "entry"));
    let mut any_changed = context.bool_type().const_zero();
    for storage in &design.process_ir.storages {
        let Some(width) = storage_state_width(design, storage.id) else {
            continue;
        };
        let previous = state_value(
            context,
            module,
            &builder,
            &storage_old_name(storage.id),
            width,
        )
        .expect("declared storage snapshot");
        let mut current = state_value(
            context,
            module,
            &builder,
            &storage_state_name(storage.id),
            width,
        )
        .expect("declared storage state");
        let before_observation = current;
        let meta_width = storage_meta_width(design, storage.id);
        let meta_previous = meta_width.map(|meta_width| {
            state_value(
                context,
                module,
                &builder,
                &storage_meta_old_name(storage.id),
                meta_width,
            )
            .expect("declared storage metadata snapshot")
        });
        let mut meta_current = meta_width.map(|meta_width| {
            state_value(
                context,
                module,
                &builder,
                &storage_meta_name(storage.id),
                meta_width,
            )
            .expect("declared storage metadata")
        });
        let meta_before_observation = meta_current;
        for binding in &storage.bindings {
            if !matches!(
                binding.direction,
                LayoutDirection::Out | LayoutDirection::InOut
            ) {
                continue;
            }
            let (offset, binding_width) =
                storage_binding_slice(design, storage.id, &binding.projection)
                    .expect("validated storage binding");
            let signal_width = design
                .signal_width(binding.signal)
                .expect("validated observed signal width");
            let mut observed = signal_value(
                context,
                module,
                &builder,
                design,
                &[binding.signal],
                ProcessSignalState::Current,
                signal_width,
            )
            .expect("validated observed storage binding");
            if signal_width != binding_width {
                let source = signal_layout(design, &[binding.signal])
                    .expect("validated observed signal layout");
                let target = storage_binding_layout_slice(design, storage.id, &binding.projection)
                    .expect("validated observed storage layout")
                    .layout;
                observed = adapt_binding_value(context, &builder, design, source, target, observed)
                    .expect("validated observed logic conversion");
            }
            current = insert_region(&builder, current, observed, offset, binding_width)
                .expect("validated observed storage region");
            if let Some(metadata) = meta_current.as_mut() {
                let binding_meta_width = binding_width
                    .checked_mul(4)
                    .expect("validated observed metadata width");
                let observed = design
                    .meta_of
                    .get(&binding.signal.0)
                    .copied()
                    .map(|companion| {
                        signal_value(
                            context,
                            module,
                            &builder,
                            design,
                            &[SignalId(companion)],
                            ProcessSignalState::Current,
                            binding_meta_width,
                        )
                        .expect("validated observed metadata binding")
                    })
                    .unwrap_or_else(|| {
                        context
                            .custom_width_int_type(
                                std::num::NonZeroU32::new(binding_meta_width).unwrap(),
                            )
                            .expect("validated observed metadata type")
                            .const_zero()
                    });
                *metadata = insert_region(
                    &builder,
                    *metadata,
                    observed,
                    offset.checked_mul(4).expect("validated metadata offset"),
                    binding_meta_width,
                )
                .expect("validated observed metadata region");
            }
        }
        store_state(
            module,
            &builder,
            &storage_state_name(storage.id),
            width,
            current,
        )
        .expect("declared storage state");
        if let (Some(meta_width), Some(metadata)) = (meta_width, meta_current) {
            store_state(
                module,
                &builder,
                &storage_meta_name(storage.id),
                meta_width,
                metadata,
            )
            .expect("declared storage metadata");
        }
        let dirty_pointer =
            storage_dirty_ptr(context, module, &builder, storage_count, storage.id.0);
        let dirty = builder
            .build_load(byte, dirty_pointer, "process.storage.commit.dirty")
            .unwrap()
            .into_int_value();
        let was_written = builder
            .build_int_compare(
                IntPredicate::NE,
                dirty,
                byte.const_zero(),
                "process.storage.was.written",
            )
            .unwrap();
        let previous = builder
            .build_select(
                was_written,
                previous,
                before_observation,
                "process.storage.previous",
            )
            .unwrap()
            .into_int_value();
        store_state(
            module,
            &builder,
            &storage_old_name(storage.id),
            width,
            previous,
        )
        .expect("declared storage snapshot");
        let meta_previous = match (meta_width, meta_previous, meta_before_observation) {
            (Some(meta_width), Some(previous), Some(before_observation)) => {
                let previous = builder
                    .build_select(
                        was_written,
                        previous,
                        before_observation,
                        "process.storage.meta.previous",
                    )
                    .unwrap()
                    .into_int_value();
                store_state(
                    module,
                    &builder,
                    &storage_meta_old_name(storage.id),
                    meta_width,
                    previous,
                )
                .expect("declared storage metadata snapshot");
                Some(previous)
            }
            _ => None,
        };
        builder
            .build_store(dirty_pointer, byte.const_zero())
            .unwrap();
        let value_changed = builder
            .build_int_compare(
                IntPredicate::NE,
                previous,
                current,
                "process.storage.changed",
            )
            .unwrap();
        let changed = match (meta_previous, meta_current) {
            (Some(previous), Some(current)) => {
                let meta_changed = builder
                    .build_int_compare(
                        IntPredicate::NE,
                        previous,
                        current,
                        "process.storage.meta.changed",
                    )
                    .unwrap();
                builder
                    .build_or(
                        value_changed,
                        meta_changed,
                        "process.storage.changed.any_plane",
                    )
                    .unwrap()
            }
            _ => value_changed,
        };
        builder
            .build_store(
                storage_changed_ptr(context, module, &builder, storage_count, storage.id.0),
                builder
                    .build_int_z_extend(changed, byte, "process.storage.changed.byte")
                    .unwrap(),
            )
            .unwrap();
        any_changed = builder
            .build_or(any_changed, changed, "process.storage.any_changed")
            .unwrap();
    }
    builder.build_return(Some(&any_changed)).unwrap();

    let query = module.add_function(
        "sx_process_storage_changed",
        byte.fn_type(&[context.i32_type().into()], false),
        None,
    );
    builder.position_at_end(context.append_basic_block(query, "entry"));
    let storage = query
        .get_first_param()
        .expect("storage change query has an id")
        .into_int_value();
    let in_range = builder
        .build_int_compare(
            IntPredicate::ULT,
            storage,
            context
                .i32_type()
                .const_int(u64::from(storage_count), false),
            "process.storage.changed.in_range",
        )
        .unwrap();
    let safe = builder
        .build_select(
            in_range,
            storage,
            context.i32_type().const_zero(),
            "process.storage.changed.safe_id",
        )
        .unwrap()
        .into_int_value();
    let loaded = builder
        .build_load(
            byte,
            storage_changed_ptr_at(context, module, &builder, storage_count, safe),
            "process.storage.changed.value",
        )
        .unwrap()
        .into_int_value();
    let result = builder
        .build_select(
            in_range,
            loaded,
            byte.const_zero(),
            "process.storage.changed.result",
        )
        .unwrap()
        .into_int_value();
    builder.build_return(Some(&result)).unwrap();
}
