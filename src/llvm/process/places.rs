//! Assignment places and staged storage.

use super::*;

/// Resolve a whole current-signal place. Keeping this fast path preserves the
/// source expression's wider mathematical value for range checking before it
/// is narrowed to the signal's representation.
pub(super) fn staged_signal_target(design: &Design, target: ProcessValueId) -> Option<SignalId> {
    let signals = staged_signal_group(design, target)?;
    let [signal] = signals else {
        return None;
    };
    Some(*signal)
}

pub(super) fn staged_signal_group(design: &Design, target: ProcessValueId) -> Option<&[SignalId]> {
    let value = design.process_ir.values.get(target.0 as usize)?;
    let ProcessValueKind::Signal { signals, state } = &value.kind else {
        return None;
    };
    let width = signals.iter().try_fold(0u32, |width, signal| {
        width.checked_add(design.signal_width(*signal)?)
    })?;
    (matches!(state, ProcessSignalState::Current)
        && !signals.is_empty()
        && value.bit_width == Some(width))
    .then_some(signals)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum StaticPlaceRoot {
    Local(ProcessId, ProcessLocalId),
    Storage(ProcessStorageId),
    Signal(SignalId),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct StaticPlace {
    pub(super) root: StaticPlaceRoot,
    pub(super) root_width: u32,
    pub(super) offset: u32,
    pub(super) width: u32,
    pub(super) reverse: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DynamicPlaceIndex {
    pub(super) value: ProcessValueId,
    pub(super) range: LayoutRange,
    pub(super) stride: u32,
    pub(super) source_order: bool,
}

#[derive(Clone, Debug)]
pub(super) struct DynamicPlace {
    pub(super) root: StaticPlaceRoot,
    pub(super) root_width: u32,
    pub(super) constant_offset: u32,
    pub(super) width: u32,
    pub(super) indices: Vec<DynamicPlaceIndex>,
}

#[derive(Clone, Copy)]
pub(super) struct ScheduleSite {
    pub(super) id: u32,
    pub(super) process: ProcessId,
    pub(super) block: siox::ir::ProcessBlockId,
    pub(super) instruction: usize,
    pub(super) target: ProcessValueId,
    pub(super) width: u32,
    pub(super) span: siox::diag::Span,
}

/// A delayed write may target object-owned storage or a signal, but never a
/// lexical local: the local could have ended before the event expires. Ranged
/// endpoints stay fail-closed until the event ABI retains the wider
/// mathematical source value used by their diagnostic.
pub(super) fn delayed_place(design: &Design, target: ProcessValueId) -> Option<StaticPlace> {
    let place = static_place(design, target)?;
    match place.root {
        StaticPlaceRoot::Local(_, _) => None,
        StaticPlaceRoot::Signal(signal) => design
            .signals
            .get(signal.0 as usize)?
            .range
            .is_none()
            .then_some(place),
        StaticPlaceRoot::Storage(storage) => design
            .process_ir
            .storages
            .get(storage.0 as usize)?
            .bindings
            .iter()
            .all(|binding| {
                design
                    .signals
                    .get(binding.signal.0 as usize)
                    .is_some_and(|signal| signal.range.is_none())
            })
            .then_some(place),
    }
}

pub(super) fn schedule_sites(design: &Design) -> Vec<ScheduleSite> {
    let mut sites = Vec::new();
    for process in &design.process_ir.processes {
        for block in &process.blocks {
            for (instruction, node) in block.instructions.iter().enumerate() {
                let ProcessInstruction::Schedule { target, span, .. } = node else {
                    continue;
                };
                let Some(place) = delayed_place(design, *target) else {
                    continue;
                };
                sites.push(ScheduleSite {
                    id: sites.len() as u32,
                    process: process.id,
                    block: block.id,
                    instruction,
                    target: *target,
                    width: place.width,
                    span: *span,
                });
            }
        }
    }
    sites
}

pub(super) fn static_place(design: &Design, id: ProcessValueId) -> Option<StaticPlace> {
    let value = design.process_ir.values.get(id.0 as usize)?;
    match &value.kind {
        ProcessValueKind::Local { process, local } => {
            let width = local_width(design, *process, *local)?;
            (value.bit_width == Some(width)).then_some(StaticPlace {
                root: StaticPlaceRoot::Local(*process, *local),
                root_width: width,
                offset: 0,
                width,
                reverse: false,
            })
        }
        ProcessValueKind::Storage(storage) => {
            let width = storage_state_width(design, *storage)?;
            (value.bit_width == Some(width)).then_some(StaticPlace {
                root: StaticPlaceRoot::Storage(*storage),
                root_width: width,
                offset: 0,
                width,
                reverse: false,
            })
        }
        ProcessValueKind::Signal { signals, state } => {
            let [signal] = signals.as_slice() else {
                return None;
            };
            if !matches!(state, ProcessSignalState::Current) {
                return None;
            }
            let width = design.signal_width(*signal)?;
            (value.bit_width == Some(width)).then_some(StaticPlace {
                root: StaticPlaceRoot::Signal(*signal),
                root_width: width,
                offset: 0,
                width,
                reverse: false,
            })
        }
        ProcessValueKind::Field { base, field } => {
            let mut place = static_place(design, *base)?;
            let selected = field_slice(process_value_layout(design, *base)?, field)?;
            place.offset = place.offset.checked_add(selected.offset)?;
            place.width = selected.width;
            value
                .bit_width
                .is_none_or(|width| width == selected.width)
                .then_some(place)
        }
        ProcessValueKind::Index { base, index } => {
            let mut place = static_place(design, *base)?;
            let base_layout = process_value_layout(design, *base)?;
            let index = process_constant_i64(design, *index)?;
            let (offset, width) = match &base_layout.kind {
                LayoutKind::Array { .. } => {
                    let selected = array_slice(base_layout, index)?;
                    (selected.offset, selected.width)
                }
                LayoutKind::Packed {
                    range: Some(range), ..
                } => {
                    let low = range.left.min(range.right);
                    let offset = u32::try_from(index.checked_sub(low)?).ok()?;
                    (offset, 1)
                }
                _ => return None,
            };
            place.offset = place.offset.checked_add(offset)?;
            place.width = width;
            let value_width_matches = match &base_layout.kind {
                LayoutKind::Packed { .. } => value.bit_width.is_some(),
                _ => value.bit_width == Some(width),
            };
            (place.offset.checked_add(width)? <= place.root_width && value_width_matches)
                .then_some(place)
        }
        ProcessValueKind::PackedSlice { base, left, right } => {
            let mut place = static_place(design, *base)?;
            let layout = process_value_layout(design, *base)?;
            let LayoutKind::Packed {
                range: Some(range), ..
            } = layout.kind
            else {
                return None;
            };
            let left = packed_label_position(range, *left)?;
            let right = packed_label_position(range, *right)?;
            let low = left.min(right);
            let width = left.abs_diff(right).checked_add(1)?;
            place.offset = place.offset.checked_add(low)?;
            place.width = width;
            place.reverse = left < right;
            (place.offset.checked_add(width)? <= place.root_width && value.bit_width == Some(width))
                .then_some(place)
        }
        _ => None,
    }
}

/// Resolve an assignment place whose root is static but one or more aggregate
/// projections are selected at runtime. Constant and dynamic contributions
/// remain separate so codegen can evaluate every index before performing one
/// read/modify/write of the root storage object.
pub(super) fn dynamic_place(design: &Design, id: ProcessValueId) -> Option<DynamicPlace> {
    fn walk(design: &Design, id: ProcessValueId) -> Option<DynamicPlace> {
        let value = design.process_ir.values.get(id.0 as usize)?;
        match &value.kind {
            ProcessValueKind::Local { process, local } => {
                let width = local_width(design, *process, *local)?;
                (value.bit_width == Some(width)).then_some(DynamicPlace {
                    root: StaticPlaceRoot::Local(*process, *local),
                    root_width: width,
                    constant_offset: 0,
                    width,
                    indices: Vec::new(),
                })
            }
            ProcessValueKind::Storage(storage) => {
                let width = storage_state_width(design, *storage)?;
                (value.bit_width == Some(width)).then_some(DynamicPlace {
                    root: StaticPlaceRoot::Storage(*storage),
                    root_width: width,
                    constant_offset: 0,
                    width,
                    indices: Vec::new(),
                })
            }
            ProcessValueKind::Signal { signals, state } => {
                let [signal] = signals.as_slice() else {
                    return None;
                };
                if !matches!(state, ProcessSignalState::Current) {
                    return None;
                }
                let width = design.signal_width(*signal)?;
                (value.bit_width == Some(width)).then_some(DynamicPlace {
                    root: StaticPlaceRoot::Signal(*signal),
                    root_width: width,
                    constant_offset: 0,
                    width,
                    indices: Vec::new(),
                })
            }
            ProcessValueKind::Field { base, field } => {
                let mut place = walk(design, *base)?;
                let selected = field_slice(process_value_layout(design, *base)?, field)?;
                place.constant_offset = place.constant_offset.checked_add(selected.offset)?;
                place.width = selected.width;
                value
                    .bit_width
                    .is_none_or(|width| width == selected.width)
                    .then_some(place)
            }
            ProcessValueKind::Index { base, index } => {
                let mut place = walk(design, *base)?;
                let base_layout = process_value_layout(design, *base)?;
                if let Some(index) = process_constant_i64(design, *index) {
                    let (offset, width) = match &base_layout.kind {
                        LayoutKind::Array { .. } => {
                            let selected = array_slice(base_layout, index)?;
                            (selected.offset, selected.width)
                        }
                        LayoutKind::Packed {
                            range: Some(range), ..
                        } => {
                            let low = range.left.min(range.right);
                            (u32::try_from(index.checked_sub(low)?).ok()?, 1)
                        }
                        _ => return None,
                    };
                    place.constant_offset = place.constant_offset.checked_add(offset)?;
                    place.width = width;
                } else {
                    let (range, stride, source_order) = match &base_layout.kind {
                        LayoutKind::Array {
                            range: Some(range),
                            element,
                        } => (*range, layout_width(element)?, true),
                        LayoutKind::Packed {
                            range: Some(range), ..
                        } => (*range, 1, false),
                        _ => return None,
                    };
                    place.indices.push(DynamicPlaceIndex {
                        value: *index,
                        range,
                        stride,
                        source_order,
                    });
                    place.width = stride;
                }
                (value.bit_width == Some(place.width)).then_some(place)
            }
            _ => None,
        }
    }

    let place = walk(design, id)?;
    if place.indices.is_empty() {
        return None;
    }
    let maximum_offset =
        place
            .indices
            .iter()
            .try_fold(place.constant_offset, |offset, projection| {
                let count = u32::try_from(projection.range.len()?).ok()?;
                offset.checked_add(count.checked_sub(1)?.checked_mul(projection.stride)?)
            })?;
    (maximum_offset.checked_add(place.width)? <= place.root_width).then_some(place)
}

pub(super) fn place_has_semantics(
    place: StaticPlace,
    owner: ProcessId,
    semantics: ProcessAssignment,
) -> bool {
    root_has_semantics(place.root, owner, semantics)
}

pub(super) fn root_has_semantics(
    root: StaticPlaceRoot,
    owner: ProcessId,
    semantics: ProcessAssignment,
) -> bool {
    matches!(
        (root, semantics),
        (
            StaticPlaceRoot::Local(process, _),
            ProcessAssignment::ImmediateLocal
        ) if process == owner
    ) || matches!(
        (root, semantics),
        (
            StaticPlaceRoot::Storage(_),
            ProcessAssignment::ImmediateStorage
        ) | (StaticPlaceRoot::Signal(_), ProcessAssignment::StagedSignal)
    )
}

pub(super) fn assignment_place_supported(
    design: &Design,
    target: ProcessValueId,
    owner: ProcessId,
    semantics: ProcessAssignment,
    supported_values: &[bool],
) -> bool {
    static_place(design, target).is_some_and(|place| place_has_semantics(place, owner, semantics))
        || dynamic_place(design, target).is_some_and(|place| {
            root_has_semantics(place.root, owner, semantics)
                && place.indices.iter().all(|index| {
                    supported_values
                        .get(index.value.0 as usize)
                        .copied()
                        .unwrap_or(false)
                })
        })
}

pub(super) fn place_class(place: StaticPlace, owner: ProcessId) -> Option<u8> {
    match place.root {
        StaticPlaceRoot::Local(process, _) if process == owner => Some(0),
        StaticPlaceRoot::Storage(_) => Some(1),
        StaticPlaceRoot::Signal(_) => Some(2),
        StaticPlaceRoot::Local(_, _) => None,
    }
}

pub(super) fn per_place_targets(
    design: &Design,
    target: ProcessValueId,
) -> Option<Vec<StaticPlace>> {
    let ProcessValueKind::Concat(parts) = &design.process_ir.values.get(target.0 as usize)?.kind
    else {
        return None;
    };
    let places = parts
        .iter()
        .map(|part| static_place(design, *part))
        .collect::<Option<Vec<_>>>()?;
    let mut roots = std::collections::HashSet::new();
    (!places.is_empty() && places.iter().all(|place| roots.insert(place.root))).then_some(places)
}

pub(super) fn supported_per_place_assignment(
    design: &Design,
    owner: ProcessId,
    target: ProcessValueId,
    value: ProcessValueId,
) -> bool {
    let Some(places) = per_place_targets(design, target) else {
        return false;
    };
    let Some(width) = places.iter().try_fold(0u32, |width, place| {
        place_class(*place, owner)?;
        width.checked_add(place.width)
    }) else {
        return false;
    };
    design
        .process_ir
        .values
        .get(value.0 as usize)
        .and_then(|value| value.bit_width)
        == Some(width)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn assignment_value<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    target: ProcessValueId,
    value: ProcessValueId,
    width: u32,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    if let Some(layout) =
        process_value_layout(design, target).filter(|layout| layout_width(layout) == Some(width))
    {
        process_value_in_layout(
            context,
            module,
            builder,
            design,
            value,
            layout,
            None,
            index_sites,
            cache,
        )
    } else {
        process_value_at(
            context,
            module,
            builder,
            design,
            value,
            width,
            false,
            None,
            index_sites,
            cache,
        )
    }
}

pub(super) fn place_root_layout(design: &Design, root: StaticPlaceRoot) -> Option<&SourceLayout> {
    match root {
        StaticPlaceRoot::Local(process, local) => design
            .process_ir
            .processes
            .get(process.0 as usize)?
            .locals
            .get(local.0 as usize)
            .and_then(|local| {
                local
                    .layout
                    .as_ref()
                    .or_else(|| local.ty.as_ref().and_then(|ty| layout_for_type(design, ty)))
            }),
        StaticPlaceRoot::Storage(storage) => design
            .process_ir
            .storages
            .get(storage.0 as usize)
            .and_then(|storage| {
                storage.layout.as_ref().or_else(|| {
                    storage
                        .ty
                        .as_ref()
                        .and_then(|ty| layout_for_type(design, ty))
                })
            }),
        StaticPlaceRoot::Signal(signal) => signal_layout(design, &[signal]),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn assignment_metadata<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    root: StaticPlaceRoot,
    target: ProcessValueId,
    assigned: ProcessValueId,
    width: u32,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<Option<IntValue<'ctx>>> {
    if matches!(root, StaticPlaceRoot::Signal(_)) {
        return Some(None);
    }
    let Some(root_layout) = place_root_layout(design, root) else {
        return Some(None);
    };
    let Some((_, encoding)) = packed_logic_layout(design, root_layout) else {
        return Some(None);
    };
    let target_kind = &design.process_ir.values.get(target.0 as usize)?.kind;
    if matches!(target_kind, ProcessValueKind::Index { .. }) {
        if width != 1 {
            return None;
        }
        let assigned_width = design
            .process_ir
            .values
            .get(assigned.0 as usize)?
            .bit_width?;
        let assigned = process_value_at(
            context,
            module,
            builder,
            design,
            assigned,
            assigned_width,
            false,
            None,
            index_sites,
            cache,
        )?;
        return compact_discriminant(context, builder, encoding, assigned).map(Some);
    }
    let layout = process_value_layout(design, target)?;
    if packed_logic_layout(design, layout)?.0 != width {
        return None;
    }
    process_packed_meta_in_layout(
        context,
        module,
        builder,
        design,
        assigned,
        layout,
        None,
        index_sites,
        cache,
    )
    .map(Some)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn stage_storage_value<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    storage: ProcessStorageId,
    value: IntValue<'ctx>,
    span: siox::diag::Span,
    range_sites: &HashMap<siox::diag::Span, u32>,
) -> Option<()> {
    let metadata = design.process_ir.storages.get(storage.0 as usize)?;
    for binding in &metadata.bindings {
        if !matches!(
            binding.direction,
            LayoutDirection::In | LayoutDirection::InOut
        ) {
            continue;
        }
        let signal = binding.signal;
        let (offset, binding_width) = storage_binding_slice(design, storage, &binding.projection)?;
        let signal_width = design.signal_width(signal)?;
        let part = extract_region(builder, value, offset, binding_width)?;
        let part = if binding_width == signal_width {
            part
        } else {
            let source = storage_binding_layout_slice(design, storage, &binding.projection)?.layout;
            let target = signal_layout(design, &[signal])?;
            adapt_binding_value(context, builder, design, source, target, part)?
        };
        let ranged = design.signals.get(signal.0 as usize)?.range.is_some();
        let staged = if ranged {
            let checked_width = part.get_type().get_bit_width().max(signal_width).max(64);
            let checked = if design.signals.get(signal.0 as usize)?.integer {
                fit_signed(builder, part, checked_width)?
            } else {
                fit(builder, part, checked_width)?
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
            fit(builder, checked, signal_width)?
        } else {
            fit(builder, part, signal_width)?
        };
        stage_signal(module, builder, signal, staged)?;
    }
    Some(())
}

pub(super) fn stage_storage_metadata<'ctx>(
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    storage: ProcessStorageId,
    metadata: IntValue<'ctx>,
) -> Option<()> {
    let storage = design.process_ir.storages.get(storage.0 as usize)?;
    for binding in &storage.bindings {
        if !matches!(
            binding.direction,
            LayoutDirection::In | LayoutDirection::InOut
        ) {
            continue;
        }
        let Some(companion) = design.meta_of.get(&binding.signal.0).copied() else {
            continue;
        };
        let (offset, width) = storage_binding_slice(design, storage.id, &binding.projection)?;
        let width = width.checked_mul(4)?;
        let value = extract_region(builder, metadata, offset.checked_mul(4)?, width)?;
        stage_signal(module, builder, SignalId(companion), value)?;
    }
    Some(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn stage_signal_group<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    signals: &[SignalId],
    value: IntValue<'ctx>,
    span: siox::diag::Span,
    range_sites: &HashMap<siox::diag::Span, u32>,
) -> Option<()> {
    let mut offset = 0u32;
    for signal in signals {
        let width = design.signal_width(*signal)?;
        let part = extract_region(builder, value, offset, width)?;
        if design.signals.get(signal.0 as usize)?.range.is_some() {
            let checked_width = width.max(64);
            let checked = if design.signals.get(signal.0 as usize)?.integer {
                fit_signed(builder, part, checked_width)?
            } else {
                fit(builder, part, checked_width)?
            };
            latch_range_failure(
                context,
                module,
                builder,
                design,
                *signal,
                checked,
                span,
                range_sites,
            )?;
        }
        stage_signal(module, builder, *signal, part)?;
        offset = offset.checked_add(width)?;
    }
    (offset == value.get_type().get_bit_width()).then_some(())
}
