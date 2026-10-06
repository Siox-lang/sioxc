//! Recursive companion planes for Process frames and aggregate projections.

use super::*;

struct MetadataEmitter<'a, 'ctx, 'checks> {
    context: &'ctx Context,
    module: &'a Module<'ctx>,
    builder: &'a Builder<'ctx>,
    design: &'a Design,
    index_sites: &'a HashMap<IndexSite, u32>,
    cache: &'a mut ProcessValueCache<'ctx, 'checks>,
}

impl<'ctx> MetadataEmitter<'_, 'ctx, '_> {
    fn emit(
        &mut self,
        id: ProcessValueId,
        layout: &SourceLayout,
        active: Option<IntValue<'ctx>>,
    ) -> Option<IntValue<'ctx>> {
        let key = (id, self.cache.key(id, active, None).1, layout.clone());
        if let Some(value) = self.cache.metadata.get(&key).copied() {
            return Some(value);
        }
        let value = self.emit_uncached(id, layout, active)?;
        self.cache.metadata.insert(key, value);
        Some(value)
    }

    fn emit_uncached(
        &mut self,
        id: ProcessValueId,
        layout: &SourceLayout,
        active: Option<IntValue<'ctx>>,
    ) -> Option<IntValue<'ctx>> {
        let width = layout_width(layout)?.checked_mul(4)?;
        let ty = self
            .context
            .custom_width_int_type(std::num::NonZeroU32::new(width)?)
            .ok()?;
        if !layout_has_packed_metadata(self.design, layout) {
            return Some(ty.const_zero());
        }
        if packed_logic_layout(self.design, layout).is_some() {
            return process_packed_meta_in_layout(
                self.context,
                self.module,
                self.builder,
                self.design,
                id,
                layout,
                active,
                self.index_sites,
                self.cache,
            );
        }
        if self
            .cache
            .meta_free
            .get(id.0 as usize)
            .copied()
            .unwrap_or(false)
        {
            return Some(ty.const_zero());
        }
        if aggregate_metadata_projection(self.design, id) {
            return self.projection(id, layout, active);
        }
        let value = self.design.process_ir.values.get(id.0 as usize)?;
        match &value.kind {
            ProcessValueKind::Default => Some(ty.const_zero()),
            ProcessValueKind::Storage(storage) => {
                (storage_meta_width(self.design, *storage) == Some(width)).then(|| {
                    state_value(
                        self.context,
                        self.module,
                        self.builder,
                        &storage_meta_name(*storage),
                        width,
                    )
                })?
            }
            ProcessValueKind::StorageState {
                storage,
                state: ProcessSignalState::Old,
            } => (storage_meta_width(self.design, *storage) == Some(width)).then(|| {
                state_value(
                    self.context,
                    self.module,
                    self.builder,
                    &storage_meta_old_name(*storage),
                    width,
                )
            })?,
            ProcessValueKind::Local { process, local } => {
                (local_meta_width(self.design, *process, *local) == Some(width)).then(|| {
                    state_value(
                        self.context,
                        self.module,
                        self.builder,
                        &local_meta_name(*process, *local),
                        width,
                    )
                })?
            }
            ProcessValueKind::Signal { signals, state }
                if !matches!(state, ProcessSignalState::Event) =>
            {
                let mut result = ty.const_zero();
                let mut offset = 0u32;
                for signal in signals {
                    let signal_width = self.design.signal_width(*signal)?.checked_mul(4)?;
                    if let Some(companion) = self.design.meta_of.get(&signal.0) {
                        let part = cached_signal_value(
                            self.context,
                            self.module,
                            self.builder,
                            self.design,
                            &[SignalId(*companion)],
                            *state,
                            signal_width,
                            self.cache,
                        )?;
                        result = insert_region(self.builder, result, part, offset, signal_width)?;
                    }
                    offset = offset.checked_add(signal_width)?;
                }
                (offset == width).then_some(result)
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
                let element_width = layout_width(element)?.checked_mul(4)?;
                let mut result = ty.const_zero();
                for (position, value) in elements.iter().enumerate() {
                    let part = self.emit(*value, element, active)?;
                    result = insert_region(
                        self.builder,
                        result,
                        part,
                        u32::try_from(position).ok()?.checked_mul(element_width)?,
                        element_width,
                    )?;
                }
                Some(result)
            }
            ProcessValueKind::Construct { fields, spread, .. } => {
                let LayoutKind::Struct {
                    fields: layout_fields,
                    ..
                } = &layout.kind
                else {
                    return None;
                };
                let mut result = match spread {
                    Some(spread) => self.emit(*spread, layout, active)?,
                    None => ty.const_zero(),
                };
                let mut positional = 0usize;
                for field in fields {
                    let index = match &field.name {
                        Some(name) => layout_fields
                            .iter()
                            .position(|candidate| candidate.name == *name)?,
                        None => {
                            let index = positional;
                            positional = positional.checked_add(1)?;
                            index
                        }
                    };
                    let field_layout = layout_fields.get(index)?;
                    let selected = field_slice(layout, &field_layout.name)?;
                    let part = self.emit(field.value?, &field_layout.layout, active)?;
                    result = insert_region(
                        self.builder,
                        result,
                        part,
                        selected.offset.checked_mul(4)?,
                        selected.width.checked_mul(4)?,
                    )?;
                }
                Some(result)
            }
            ProcessValueKind::Select {
                condition,
                then_value,
                else_value,
            } => {
                let condition = process_value(
                    self.context,
                    self.module,
                    self.builder,
                    self.design,
                    *condition,
                    active,
                    self.index_sites,
                    self.cache,
                )?;
                let condition = as_condition(self.builder, condition)?;
                let checked = self.cache.requires_activity(*then_value)
                    || self.cache.requires_activity(*else_value);
                let (then_active, else_active) = if checked {
                    let then_active = self.arm_active(condition, active)?;
                    let inverse = self
                        .builder
                        .build_not(condition, "pv.meta.else.condition")
                        .ok()?;
                    (Some(then_active), Some(self.arm_active(inverse, active)?))
                } else {
                    (None, None)
                };
                let then_value = self.emit(*then_value, layout, then_active)?;
                let else_value = self.emit(*else_value, layout, else_active)?;
                Some(
                    self.builder
                        .build_select(
                            condition,
                            then_value,
                            else_value,
                            "pv.meta.aggregate.select",
                        )
                        .ok()?
                        .into_int_value(),
                )
            }
            ProcessValueKind::Match { scrutinee, arms } => {
                let scrutinee_value = process_value(
                    self.context,
                    self.module,
                    self.builder,
                    self.design,
                    *scrutinee,
                    active,
                    self.index_sites,
                    self.cache,
                )?;
                let eligible = process_match_eligibility(
                    self.context,
                    self.builder,
                    self.design,
                    *scrutinee,
                    scrutinee_value,
                    arms,
                )?;
                let mut result = None;
                for (arm, eligible) in arms.iter().zip(eligible).rev() {
                    let arm_active = if self.cache.requires_activity(arm.value) {
                        Some(self.arm_active(eligible, active)?)
                    } else {
                        None
                    };
                    let value = self.emit(arm.value, layout, arm_active)?;
                    result = Some(match result {
                        Some(other) => self
                            .builder
                            .build_select(eligible, value, other, "pv.meta.aggregate.match")
                            .ok()?
                            .into_int_value(),
                        None => value,
                    });
                }
                result
            }
            ProcessValueKind::RawResize { operand } => {
                let source = process_value_layout(self.design, *operand)?;
                let part = self.emit(*operand, source, active)?;
                fit(self.builder, part, width)
            }
            _ => None,
        }
    }

    fn arm_active(
        &self,
        arm: IntValue<'ctx>,
        outer: Option<IntValue<'ctx>>,
    ) -> Option<IntValue<'ctx>> {
        match outer {
            Some(outer) => self
                .builder
                .build_and(outer, arm, "pv.meta.arm.active")
                .ok(),
            None => Some(arm),
        }
    }

    fn projection(
        &mut self,
        id: ProcessValueId,
        layout: &SourceLayout,
        active: Option<IntValue<'ctx>>,
    ) -> Option<IntValue<'ctx>> {
        let value = self.design.process_ir.values.get(id.0 as usize)?;
        let width = layout_width(layout)?.checked_mul(4)?;
        match &value.kind {
            ProcessValueKind::Field { base, field } => {
                let source = process_value_layout(self.design, *base)?;
                let selected = field_slice(source, field)?;
                if selected.width.checked_mul(4)? != width {
                    return None;
                }
                let part = self.emit(*base, source, active)?;
                extract_region(self.builder, part, selected.offset.checked_mul(4)?, width)
            }
            ProcessValueKind::Index { base, index } => {
                let source = process_value_layout(self.design, *base)?;
                let LayoutKind::Array {
                    range: Some(range),
                    element,
                } = &source.kind
                else {
                    return None;
                };
                if layout_width(element)?.checked_mul(4)? != width {
                    return None;
                }
                if let Some(index) = process_constant_i64(self.design, *index) {
                    let selected = array_slice(source, index)?;
                    let part = self.emit(*base, source, active)?;
                    extract_region(self.builder, part, selected.offset.checked_mul(4)?, width)
                } else {
                    let source_width = layout_width(source)?.checked_mul(4)?;
                    let position = dynamic_index_position(
                        self.context,
                        self.module,
                        self.builder,
                        self.design,
                        *index,
                        *range,
                        true,
                        source_width,
                        active,
                        self.index_sites,
                        self.cache,
                    )?;
                    let offset = self
                        .builder
                        .build_int_mul(
                            position,
                            position.get_type().const_int(u64::from(width), false),
                            "pv.meta.index.offset",
                        )
                        .ok()?;
                    let part = self.emit(*base, source, active)?;
                    extract_dynamic_region(self.builder, part, offset, width)
                }
            }
            _ => None,
        }
    }
}

/// Packed fields/array elements are projections of a recursive plane, not
/// packed bit selections (which reconstruct a scalar source discriminant).
pub(super) fn aggregate_metadata_projection(design: &Design, id: ProcessValueId) -> bool {
    match design
        .process_ir
        .values
        .get(id.0 as usize)
        .map(|value| &value.kind)
    {
        Some(ProcessValueKind::Field { .. }) => true,
        Some(ProcessValueKind::Index { base, .. }) => process_value_layout(design, *base)
            .is_some_and(|layout| matches!(layout.kind, LayoutKind::Array { .. })),
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn process_aggregate_projection_meta<'ctx>(
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
    MetadataEmitter {
        context,
        module,
        builder,
        design,
        index_sites,
        cache,
    }
    .projection(id, layout, active)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn process_value_meta_in_layout<'ctx>(
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
    MetadataEmitter {
        context,
        module,
        builder,
        design,
        index_sites,
        cache,
    }
    .emit(id, layout, active)
}

pub(super) fn process_value_meta_supported(
    design: &Design,
    id: ProcessValueId,
    layout: &SourceLayout,
    supported: &ProcessValueSupport,
) -> bool {
    let Some(width) = layout_width(layout).and_then(|width| width.checked_mul(4)) else {
        return false;
    };
    if width > super::super::emit::LLVM_MAX_INT_BITS {
        return false;
    }
    if !layout_has_packed_metadata(design, layout) {
        return true;
    }
    if packed_logic_layout(design, layout).is_some() {
        return process_packed_meta_supported(design, id, layout, supported);
    }
    if supported
        .meta_free
        .get(id.0 as usize)
        .copied()
        .unwrap_or(false)
    {
        return true;
    }
    if aggregate_metadata_projection(design, id) {
        return process_aggregate_projection_meta_supported(design, id, layout, supported);
    }
    let Some(value) = design.process_ir.values.get(id.0 as usize) else {
        return false;
    };
    match &value.kind {
        ProcessValueKind::Default => true,
        ProcessValueKind::Storage(storage)
        | ProcessValueKind::StorageState {
            storage,
            state: ProcessSignalState::Old,
        } => storage_meta_width(design, *storage) == Some(width),
        ProcessValueKind::Local { process, local } => {
            local_meta_width(design, *process, *local) == Some(width)
        }
        ProcessValueKind::Signal { signals, state } => {
            !matches!(state, ProcessSignalState::Event)
                && signals.iter().try_fold(0u32, |total, signal| {
                    total.checked_add(design.signal_width(*signal)?)
                }) == Some(width / 4)
        }
        ProcessValueKind::Array(elements) => match &layout.kind {
            LayoutKind::Array {
                range: Some(range),
                element,
            } => {
                range.len().and_then(|len| usize::try_from(len).ok()) == Some(elements.len())
                    && elements
                        .iter()
                        .all(|id| process_value_meta_supported(design, *id, element, supported))
            }
            _ => false,
        },
        ProcessValueKind::Construct { fields, spread, .. } => {
            let LayoutKind::Struct {
                fields: layout_fields,
                ..
            } = &layout.kind
            else {
                return false;
            };
            let mut positional = 0usize;
            spread.is_none_or(|spread| {
                process_value_meta_supported(design, spread, layout, supported)
            }) && fields.iter().all(|field| {
                let field_layout = match &field.name {
                    Some(name) => layout_fields
                        .iter()
                        .find(|candidate| candidate.name == *name),
                    None => {
                        let layout = layout_fields.get(positional);
                        positional += 1;
                        layout
                    }
                };
                field_layout
                    .zip(field.value)
                    .is_some_and(|(field_layout, value)| {
                        process_value_meta_supported(design, value, &field_layout.layout, supported)
                    })
            })
        }
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        } => {
            supported
                .get(condition.0 as usize)
                .copied()
                .unwrap_or(false)
                && process_value_meta_supported(design, *then_value, layout, supported)
                && process_value_meta_supported(design, *else_value, layout, supported)
        }
        ProcessValueKind::Match { scrutinee, arms } => {
            supported
                .get(scrutinee.0 as usize)
                .copied()
                .unwrap_or(false)
                && !arms.is_empty()
                && arms.iter().all(|arm| {
                    process_pattern_supported(&arm.pattern)
                        && process_value_meta_supported(design, arm.value, layout, supported)
                })
        }
        ProcessValueKind::RawResize { operand } => process_value_layout(design, *operand)
            .is_some_and(|source| {
                process_value_meta_supported(design, *operand, source, supported)
            }),
        _ => false,
    }
}

pub(super) fn process_aggregate_projection_meta_supported(
    design: &Design,
    id: ProcessValueId,
    layout: &SourceLayout,
    supported: &ProcessValueSupport,
) -> bool {
    let Some(value) = design.process_ir.values.get(id.0 as usize) else {
        return false;
    };
    match &value.kind {
        ProcessValueKind::Field { base, field } => {
            process_value_layout(design, *base).is_some_and(|source| {
                field_slice(source, field)
                    .is_some_and(|selected| Some(selected.width) == layout_width(layout))
                    && process_value_meta_supported(design, *base, source, supported)
            })
        }
        ProcessValueKind::Index { base, index } => {
            process_value_layout(design, *base).is_some_and(|source| {
                let LayoutKind::Array {
                    range: Some(_),
                    element,
                } = &source.kind
                else {
                    return false;
                };
                layout_width(element) == layout_width(layout)
                    && supported.get(index.0 as usize).copied().unwrap_or(false)
                    && process_value_meta_supported(design, *base, source, supported)
            })
        }
        _ => false,
    }
}
