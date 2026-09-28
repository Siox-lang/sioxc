//! State globals: declaration, region reads and writes, stores, and range
//! checks.

use super::*;

/// Declare exact-width process frames before the design ABI is emitted. The
/// design reset/commit functions call the two internal helpers whose bodies
/// are filled after all Process IR metadata is available.
pub(in crate::llvm) fn declare_state<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    design: &Design,
) {
    let add_state = |name: &str, width: u32| {
        let ty = context
            .custom_width_int_type(std::num::NonZeroU32::new(width).expect("nonzero state width"))
            .expect("validated process state width");
        let global = module.add_global(ty, None, name);
        global.set_initializer(&ty.const_zero());
        global.set_linkage(Linkage::Internal);
    };
    for storage in &design.process_ir.storages {
        if let Some(width) = storage_state_width(design, storage.id) {
            add_state(&storage_state_name(storage.id), width);
            add_state(&storage_old_name(storage.id), width);
        }
        if let Some(width) = storage_meta_width(design, storage.id) {
            add_state(&storage_meta_name(storage.id), width);
            add_state(&storage_meta_old_name(storage.id), width);
        }
    }
    for process in &design.process_ir.processes {
        for local in &process.locals {
            if let Some(width) = local_width(design, process.id, local.id) {
                add_state(&local_state_name(process.id, local.id), width);
            }
            if let Some(width) = local_meta_width(design, process.id, local.id) {
                add_state(&local_meta_name(process.id, local.id), width);
            }
        }
        for block in &process.blocks {
            let ProcessTerminator::For { iterable, .. } = &block.terminator else {
                continue;
            };
            let dynamic_string = dynamic_string_value(design, *iterable);
            if range_loop_bounds(design, *iterable).is_none()
                && array_loop_shape(design, *iterable).is_none()
                && !dynamic_string
            {
                continue;
            }
            add_state(&loop_active_name(process.id, block.id), 1);
            add_state(&loop_cursor_name(process.id, block.id), 64);
            add_state(&loop_end_name(process.id, block.id), 64);
            if let Some((layout, _, _)) = array_loop_shape(design, *iterable) {
                if let Some(width) = layout_width(layout) {
                    add_state(&loop_iterable_name(process.id, block.id), width);
                }
            } else if dynamic_string {
                add_state(&loop_iterable_name(process.id, block.id), 64);
            }
        }
    }
    let storage_count = u32::try_from(design.process_ir.storages.len())
        .expect("ProcessStorageId is a u32 ABI index");
    let changed = context.i8_type().array_type(storage_count.max(1));
    let global = module.add_global(changed, None, "sx.process.storage.changed");
    global.set_initializer(&changed.const_zero());
    global.set_linkage(Linkage::Internal);
    let dirty = context.i8_type().array_type(storage_count.max(1));
    let global = module.add_global(dirty, None, "sx.process.storage.dirty");
    global.set_initializer(&dirty.const_zero());
    global.set_linkage(Linkage::Internal);

    module.add_function(
        "sx.process.reset",
        context
            .void_type()
            .fn_type(&[context.i32_type().into()], false),
        Some(Linkage::Internal),
    );
    module.add_function(
        "sx.process.commit.storage",
        context.bool_type().fn_type(&[], false),
        Some(Linkage::Internal),
    );
}

/// Convert an integer to another exact LLVM width without changing its
/// unsigned bit pattern.
pub(super) fn fit<'ctx>(
    builder: &Builder<'ctx>,
    value: IntValue<'ctx>,
    width: u32,
) -> Option<IntValue<'ctx>> {
    let ty = value
        .get_type()
        .get_context()
        .custom_width_int_type(std::num::NonZeroU32::new(width)?)
        .ok()?;
    match value.get_type().get_bit_width().cmp(&width) {
        std::cmp::Ordering::Less => builder.build_int_z_extend(value, ty, "pv.zx").ok(),
        std::cmp::Ordering::Greater => builder.build_int_truncate(value, ty, "pv.tr").ok(),
        std::cmp::Ordering::Equal => Some(value),
    }
}

/// Signed counterpart of [`fit`].
pub(super) fn fit_signed<'ctx>(
    builder: &Builder<'ctx>,
    value: IntValue<'ctx>,
    width: u32,
) -> Option<IntValue<'ctx>> {
    let ty = value
        .get_type()
        .get_context()
        .custom_width_int_type(std::num::NonZeroU32::new(width)?)
        .ok()?;
    match value.get_type().get_bit_width().cmp(&width) {
        std::cmp::Ordering::Less => builder.build_int_s_extend(value, ty, "pv.sx").ok(),
        std::cmp::Ordering::Greater => builder.build_int_truncate(value, ty, "pv.tr").ok(),
        std::cmp::Ordering::Equal => Some(value),
    }
}

/// Extract one packed aggregate region. Offsets count from the least
/// significant bit of the object-owned process frame.
pub(super) fn extract_region<'ctx>(
    builder: &Builder<'ctx>,
    value: IntValue<'ctx>,
    offset: u32,
    width: u32,
) -> Option<IntValue<'ctx>> {
    let total = value.get_type().get_bit_width();
    if width == 0 || offset.checked_add(width)? > total {
        return None;
    }
    let shifted = if offset == 0 {
        value
    } else {
        builder
            .build_right_shift(
                value,
                value.get_type().const_int(u64::from(offset), false),
                false,
                "pv.aggregate.extract",
            )
            .ok()?
    };
    fit(builder, shifted, width)
}

/// Replace one packed aggregate region without disturbing neighbouring
/// fields/elements.
pub(super) fn insert_region<'ctx>(
    builder: &Builder<'ctx>,
    base: IntValue<'ctx>,
    part: IntValue<'ctx>,
    offset: u32,
    width: u32,
) -> Option<IntValue<'ctx>> {
    let total = base.get_type().get_bit_width();
    if width == 0 || offset.checked_add(width)? > total {
        return None;
    }
    if offset == 0 && width == total {
        return fit(builder, part, total);
    }
    let ty = base.get_type();
    let low_mask = builder
        .build_right_shift(
            ty.const_all_ones(),
            ty.const_int(u64::from(total - width), false),
            false,
            "pv.aggregate.low_mask",
        )
        .ok()?;
    let mask = if offset == 0 {
        low_mask
    } else {
        builder
            .build_left_shift(
                low_mask,
                ty.const_int(u64::from(offset), false),
                "pv.aggregate.mask",
            )
            .ok()?
    };
    let cleared = builder
        .build_and(
            base,
            builder.build_not(mask, "pv.aggregate.keep").ok()?,
            "pv.aggregate.clear",
        )
        .ok()?;
    let part = fit(builder, part, total)?;
    let part = if offset == 0 {
        part
    } else {
        builder
            .build_left_shift(
                part,
                ty.const_int(u64::from(offset), false),
                "pv.aggregate.place",
            )
            .ok()?
    };
    builder.build_or(cleared, part, "pv.aggregate.insert").ok()
}

/// Variable-offset counterpart of [`insert_region`]. `offset` is already
/// bounded by the place layout, so neither shift can reach the root width.
pub(super) fn insert_dynamic_region<'ctx>(
    builder: &Builder<'ctx>,
    base: IntValue<'ctx>,
    part: IntValue<'ctx>,
    offset: IntValue<'ctx>,
    width: u32,
) -> Option<IntValue<'ctx>> {
    let total = base.get_type().get_bit_width();
    if width == 0 || width > total || offset.get_type() != base.get_type() {
        return None;
    }
    let ty = base.get_type();
    let low_mask = builder
        .build_right_shift(
            ty.const_all_ones(),
            ty.const_int(u64::from(total - width), false),
            false,
            "pv.dynamic.low_mask",
        )
        .ok()?;
    let mask = builder
        .build_left_shift(low_mask, offset, "pv.dynamic.mask")
        .ok()?;
    let cleared = builder
        .build_and(
            base,
            builder.build_not(mask, "pv.dynamic.keep").ok()?,
            "pv.dynamic.clear",
        )
        .ok()?;
    let part = fit(builder, part, total)?;
    let part = builder
        .build_and(part, low_mask, "pv.dynamic.part.masked")
        .ok()?;
    let placed = builder
        .build_left_shift(part, offset, "pv.dynamic.place")
        .ok()?;
    builder.build_or(cleared, placed, "pv.dynamic.insert").ok()
}

/// Build the recursive language default into the same packed frame used for
/// aggregate locals/storage. Leaf defaults still come from std-owned metadata.
pub(super) fn layout_default_value<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    design: &Design,
    layout: &SourceLayout,
) -> Option<IntValue<'ctx>> {
    let width = layout_width(layout)?;
    let ty = context
        .custom_width_int_type(std::num::NonZeroU32::new(width)?)
        .ok()?;
    match &layout.kind {
        LayoutKind::Scalar { .. } | LayoutKind::Packed { .. } | LayoutKind::Opaque { .. } => {
            Some(ty.const_int(layout_default(design, Some(layout)), false))
        }
        LayoutKind::Array {
            range: Some(range),
            element,
        } => {
            let element_width = layout_width(element)?;
            let element = layout_default_value(context, builder, design, element)?;
            let mut value = ty.const_zero();
            for position in 0..u32::try_from(range.len()?).ok()? {
                value = insert_region(
                    builder,
                    value,
                    element,
                    position.checked_mul(element_width)?,
                    element_width,
                )?;
            }
            Some(value)
        }
        LayoutKind::Struct { fields, .. } => {
            let mut value = ty.const_zero();
            let mut offset = 0u32;
            for field in fields {
                let field_width = layout_width(&field.layout)?;
                value = insert_region(
                    builder,
                    value,
                    layout_default_value(context, builder, design, &field.layout)?,
                    offset,
                    field_width,
                )?;
                offset = offset.checked_add(field_width)?;
            }
            Some(value)
        }
        LayoutKind::Array { range: None, .. } => None,
    }
}

/// Read one scalar signal value through a word accessor and reconstruct its
/// exact LLVM integer. The current plane uses the stable design ABI; old/event
/// accessors remain internal to the emitted design object.
pub(super) fn signal_value<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    signals: &[siox::ir::SignalId],
    state: ProcessSignalState,
    width: u32,
) -> Option<IntValue<'ctx>> {
    let [signal] = signals else {
        return None;
    };
    let signal_width = match state {
        ProcessSignalState::Current | ProcessSignalState::Old => design.signal_width(*signal)?,
        ProcessSignalState::Event => 1,
    };
    let value_type = context
        .custom_width_int_type(std::num::NonZeroU32::new(width)?)
        .ok()?;
    let read = module.get_function(match state {
        ProcessSignalState::Current => "sx_read_word",
        ProcessSignalState::Old => "sx.process.read.old",
        ProcessSignalState::Event => "sx.process.read.event",
    })?;
    let mut value = value_type.const_zero();
    // Never request or shift a word whose first bit lies outside the result.
    // A malformed hand-built IR can disagree with the signal width; treating
    // the result width as the cap keeps that mismatch from producing LLVM
    // shift poison before validation grows a stronger shape check.
    for word in 0..super::super::words_for(signal_width.min(width)) {
        let call = builder
            .build_call(
                read,
                &[
                    context
                        .i32_type()
                        .const_int(u64::from(signal.0), false)
                        .into(),
                    context.i32_type().const_int(u64::from(word), false).into(),
                ],
                "pv.word",
            )
            .ok()?
            .try_as_basic_value();
        let raw = match call {
            inkwell::values::ValueKind::Basic(value) => value.into_int_value(),
            _ => return None,
        };
        let placed = fit(builder, raw, width)?;
        let offset = word.checked_mul(super::super::ABI_WORD_BITS)?;
        let placed = if offset == 0 {
            placed
        } else {
            builder
                .build_left_shift(
                    placed,
                    value_type.const_int(u64::from(offset), false),
                    "pv.place",
                )
                .ok()?
        };
        value = builder.build_or(value, placed, "pv.join").ok()?;
    }
    Some(value)
}

/// Whether native Process lowering can recover the unknown-value plane used by a
/// marked packed-vector comparison. Finalized hardware operands are signal
/// reads: each value-plane signal either has a companion in `meta_of`, or is
/// known to be two-valued. Literal numeric operands are likewise two-valued.
/// Other Process values stay fail-closed until Process storage carries its own
/// metavalue plane; accepting them here would silently compare only value bits.
pub(super) fn process_meta_operand_supported(
    design: &Design,
    id: ProcessValueId,
    supported: &[bool],
) -> bool {
    let Some(value) = design.process_ir.values.get(id.0 as usize) else {
        return false;
    };
    match &value.kind {
        ProcessValueKind::Number(_)
        | ProcessValueKind::BitString { .. }
        | ProcessValueKind::Char(_) => true,
        ProcessValueKind::Signal { signals, state } => {
            !signals.is_empty()
                && !matches!(state, ProcessSignalState::Event)
                && signals.iter().all(|signal| {
                    design.signals.get(signal.0 as usize).is_some()
                        && design.meta_of.get(&signal.0).is_none_or(|companion| {
                            design.signals.get(*companion as usize).is_some()
                                && design
                                    .array_element_enums
                                    .get(&signal.0)
                                    .and_then(|element| design.logic_encodings.get(element))
                                    .is_some()
                        })
                })
        }
        _ => process_value_layout(design, id)
            .is_some_and(|layout| process_packed_meta_supported(design, id, layout, supported)),
    }
}

/// Emit `true` when a comparison operand contains one of the std-declared
/// unknown logic discriminants. Companion storage packs one discriminant nibble
/// per value-plane bit. Weak `L`/`H` values are deliberately excluded because
/// the `LogicEncoding` contract does not list them in `unknown`.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_meta_operand_unknown<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    id: ProcessValueId,
    active: Option<IntValue<'ctx>>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    let value = design.process_ir.values.get(id.0 as usize)?;
    if matches!(
        &value.kind,
        ProcessValueKind::Number(_)
            | ProcessValueKind::BitString { .. }
            | ProcessValueKind::Char(_)
    ) {
        return Some(context.bool_type().const_zero());
    }
    let layout = process_value_layout(design, id)?;
    let (elements, encoding) = packed_logic_layout(design, layout)?;
    let packed = process_packed_meta_in_layout(
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
    let mut any = context.bool_type().const_zero();
    for position in 0..elements {
        let nibble = extract_region(builder, packed, position.checked_mul(4)?, 4)?;
        let mut unknown = context.bool_type().const_zero();
        for discriminant in &encoding.unknown {
            let equal = builder
                .build_int_compare(
                    IntPredicate::EQ,
                    nibble,
                    nibble.get_type().const_int(*discriminant, false),
                    "pv.meta.discriminant",
                )
                .ok()?;
            unknown = builder.build_or(unknown, equal, "pv.meta.member").ok()?;
        }
        any = builder.build_or(any, unknown, "pv.meta.any").ok()?;
    }
    Some(any)
}

pub(super) fn state_value<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    name: &str,
    width: u32,
) -> Option<IntValue<'ctx>> {
    let ty = context
        .custom_width_int_type(std::num::NonZeroU32::new(width)?)
        .ok()?;
    Some(
        builder
            .build_load(ty, module.get_global(name)?.as_pointer_value(), "pv.state")
            .ok()?
            .into_int_value(),
    )
}

pub(super) fn process_storage_state_value<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    storage: ProcessStorageId,
    state: ProcessSignalState,
    width: u32,
) -> Option<IntValue<'ctx>> {
    match state {
        ProcessSignalState::Current => None,
        ProcessSignalState::Old => {
            let stored_width = storage_state_width(design, storage)?;
            let value = state_value(
                context,
                module,
                builder,
                &storage_old_name(storage),
                stored_width,
            )?;
            let signed = design
                .process_ir
                .storages
                .get(storage.0 as usize)
                .is_some_and(|storage| {
                    storage.layout.as_ref().map_or_else(
                        || storage.ty.as_ref().is_some_and(process_type_is_signed),
                        process_layout_is_signed,
                    )
                });
            if signed {
                fit_signed(builder, value, width)
            } else {
                fit(builder, value, width)
            }
        }
        ProcessSignalState::Event if width == 1 => {
            let changed = builder
                .build_load(
                    context.i8_type(),
                    storage_changed_ptr(
                        context,
                        module,
                        builder,
                        u32::try_from(design.process_ir.storages.len()).ok()?,
                        storage.0,
                    ),
                    "pv.storage.event",
                )
                .ok()?
                .into_int_value();
            builder
                .build_int_compare(
                    IntPredicate::NE,
                    changed,
                    context.i8_type().const_zero(),
                    "pv.storage.changed",
                )
                .ok()
        }
        ProcessSignalState::Event => None,
    }
}

pub(super) fn store_state<'ctx>(
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    name: &str,
    width: u32,
    value: IntValue<'ctx>,
) -> Option<()> {
    let value = fit(builder, value, width)?;
    builder
        .build_store(module.get_global(name)?.as_pointer_value(), value)
        .ok()?;
    Some(())
}

pub(super) fn stage_signal<'ctx>(
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    signal: SignalId,
    value: IntValue<'ctx>,
) -> Option<()> {
    let stage = module.get_function(&format!("sx.process.stage.{}", signal.0))?;
    builder.build_call(stage, &[value.into()], "").ok()?;
    Some(())
}

/// Record the first checked-index failure in the same globals used by the
/// established hardware emitter. `active` preserves source control flow when
/// LLVM eagerly computes both operands of a value-level `select`.
pub(super) fn latch_index_failure<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    valid: IntValue<'ctx>,
    offending: IntValue<'ctx>,
    active: Option<IntValue<'ctx>>,
    site: u32,
) -> Option<()> {
    let invalid = builder.build_not(valid, "pv.index.invalid").ok()?;
    let invalid = match active {
        Some(active) => builder.build_and(active, invalid, "pv.index.active").ok()?,
        None => invalid,
    };
    let i32 = context.i32_type();
    let error = module.get_global("index_error")?.as_pointer_value();
    let previous = builder
        .build_load(i32, error, "pv.index.previous")
        .ok()?
        .into_int_value();
    let empty = builder
        .build_int_compare(
            IntPredicate::EQ,
            previous,
            i32.const_zero(),
            "pv.index.empty",
        )
        .ok()?;
    let record = builder.build_and(empty, invalid, "pv.index.record").ok()?;
    let next = builder
        .build_select(
            record,
            i32.const_int(u64::from(site), false),
            previous,
            "pv.index.next",
        )
        .ok()?
        .into_int_value();
    builder.build_store(error, next).ok()?;

    let offending = fit_signed(builder, offending, 64)?;
    let value = module.get_global("index_value")?.as_pointer_value();
    let previous = builder
        .build_load(context.i64_type(), value, "pv.index.value.previous")
        .ok()?
        .into_int_value();
    let next = builder
        .build_select(record, offending, previous, "pv.index.value.next")
        .ok()?
        .into_int_value();
    builder.build_store(value, next).ok()?;
    Some(())
}

/// Stage a ranged signal's pre-narrowed value and source site. The commit
/// helper performs the actual check after all writes in the delta, so a later
/// assignment that overwrites this one also overwrites its candidate failure.
#[allow(clippy::too_many_arguments)]
pub(super) fn latch_range_failure<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    signal: SignalId,
    value: IntValue<'ctx>,
    span: siox::diag::Span,
    range_sites: &HashMap<siox::diag::Span, u32>,
) -> Option<()> {
    design.signals.get(signal.0 as usize)?.range?;
    let value = fit_signed(builder, value, 64)?;
    let site_id = range_sites.get(&span).copied().unwrap_or(0);
    let stage = module.get_function(&format!("sx.process.stage.range.{}", signal.0))?;
    builder
        .build_call(
            stage,
            &[
                value.into(),
                context
                    .i32_type()
                    .const_int(u64::from(site_id), false)
                    .into(),
            ],
            "",
        )
        .ok()?;
    Some(())
}

/// Compute which dependency-ordered arena nodes contain a checked access.
/// One shared table serves every process/block in the module.
pub(super) fn checked_process_values(design: &Design) -> Vec<bool> {
    let mut checked = Vec::with_capacity(design.process_ir.values.len());
    let has =
        |checked: &[bool], id: ProcessValueId| checked.get(id.0 as usize).copied().unwrap_or(false);
    for value in &design.process_ir.values {
        let contains = match &value.kind {
            ProcessValueKind::CheckedIndex { .. } => true,
            ProcessValueKind::Field { base, .. }
            | ProcessValueKind::BitSlice { base, .. }
            | ProcessValueKind::PackedSlice { base, .. }
            | ProcessValueKind::TableLookup { index: base, .. }
            | ProcessValueKind::Unary { operand: base, .. }
            | ProcessValueKind::RawResize { operand: base } => has(&checked, *base),
            ProcessValueKind::Index { base, index }
            | ProcessValueKind::Binary {
                left: base,
                right: index,
                ..
            } => has(&checked, *base) || has(&checked, *index),
            ProcessValueKind::Range { left, right } => left
                .iter()
                .chain(right)
                .copied()
                .any(|value| has(&checked, value)),
            ProcessValueKind::Select {
                condition,
                then_value,
                else_value,
            } => [*condition, *then_value, *else_value]
                .into_iter()
                .any(|value| has(&checked, value)),
            ProcessValueKind::MetaCompare {
                operands, inner, ..
            } => operands
                .iter()
                .copied()
                .chain(std::iter::once(*inner))
                .any(|value| has(&checked, value)),
            ProcessValueKind::Match { scrutinee, arms } => {
                has(&checked, *scrutinee) || arms.iter().any(|arm| has(&checked, arm.value))
            }
            ProcessValueKind::Call {
                callee, arguments, ..
            } => std::iter::once(*callee)
                .chain(arguments.iter().copied())
                .any(|value| has(&checked, value)),
            ProcessValueKind::ForeignCall { arguments, .. }
            | ProcessValueKind::HostCall { arguments, .. } => {
                arguments.iter().any(|value| has(&checked, *value))
            }
            ProcessValueKind::Construct { fields, spread, .. } => fields
                .iter()
                .filter_map(|field| field.value)
                .chain(spread.iter().copied())
                .any(|value| has(&checked, value)),
            ProcessValueKind::Concat(values) | ProcessValueKind::Array(values) => {
                values.iter().any(|value| has(&checked, *value))
            }
            ProcessValueKind::Number(_)
            | ProcessValueKind::Suffixed { .. }
            | ProcessValueKind::BitString { .. }
            | ProcessValueKind::Char(_)
            | ProcessValueKind::String(_)
            | ProcessValueKind::Local { .. }
            | ProcessValueKind::Storage(_)
            | ProcessValueKind::StorageState { .. }
            | ProcessValueKind::Signal { .. }
            | ProcessValueKind::Definition(_)
            | ProcessValueKind::Intrinsic(_)
            | ProcessValueKind::Default
            | ProcessValueKind::Attribute { .. }
            | ProcessValueKind::Invalid => false,
        };
        checked.push(contains);
    }
    checked
}
