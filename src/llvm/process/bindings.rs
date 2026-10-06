//! Storage bindings, operand layouts, and default values.

use super::*;

/// Follow a flattened storage-binding suffix such as `.payload[2].valid`.
pub(super) fn projection_slice<'a>(
    layout: &'a SourceLayout,
    projection: &str,
) -> Option<LayoutSlice<'a>> {
    let mut selected = LayoutSlice {
        layout,
        offset: 0,
        width: layout_width(layout)?,
    };
    let mut rest = projection;
    while !rest.is_empty() {
        let child = if let Some(field) = rest.strip_prefix('.') {
            let boundary = field.find(['.', '[']).unwrap_or(field.len());
            let (name, tail) = field.split_at(boundary);
            rest = tail;
            field_slice(selected.layout, name)?
        } else if let Some(index) = rest.strip_prefix('[') {
            let (index, tail) = index.split_once(']')?;
            rest = tail;
            array_slice(selected.layout, index.parse().ok()?)?
        } else {
            return None;
        };
        selected.offset = selected.offset.checked_add(child.offset)?;
        selected.layout = child.layout;
        selected.width = child.width;
    }
    Some(selected)
}

pub(super) fn storage_binding_slice(
    design: &Design,
    storage: ProcessStorageId,
    projection: &str,
) -> Option<(u32, u32)> {
    let metadata = design.process_ir.storages.get(storage.0 as usize)?;
    if metadata.layout.is_some() {
        let selected = storage_binding_layout_slice(design, storage, projection)?;
        return Some((selected.offset, selected.width));
    }
    let width = storage_width(design, storage)?;
    projection.is_empty().then_some((0, width))
}

pub(super) fn storage_binding_layout_slice<'a>(
    design: &'a Design,
    storage: ProcessStorageId,
    projection: &str,
) -> Option<LayoutSlice<'a>> {
    let layout = design
        .process_ir
        .storages
        .get(storage.0 as usize)?
        .layout
        .as_ref()?;
    projection_slice(layout, projection)
}

pub(super) fn scalar_logic_encoding<'a>(
    design: &'a Design,
    layout: &SourceLayout,
) -> Option<(u32, &'a siox::ir::LogicEncoding)> {
    let LayoutKind::Scalar {
        width,
        domain: siox::ir::ScalarDomain::Enum(name),
        ..
    } = &layout.kind
    else {
        return None;
    };
    Some((*width, design.logic_encodings.get(name)?))
}

/// Packed indexing reconstructs a scalar discriminant even when its result
/// has no separately registered scalar layout.
pub(super) fn process_scalar_logic_encoding(
    design: &Design,
    id: ProcessValueId,
) -> Option<&siox::ir::LogicEncoding> {
    if let Some((_, encoding)) =
        process_value_layout(design, id).and_then(|layout| scalar_logic_encoding(design, layout))
    {
        return Some(encoding);
    }
    let ProcessValueKind::Index { base, .. } = &design.process_ir.values.get(id.0 as usize)?.kind
    else {
        return None;
    };
    packed_logic_layout(design, process_value_layout(design, *base)?).map(|(_, encoding)| encoding)
}

pub(super) fn storage_binding_is_compatible(
    design: &Design,
    storage: ProcessStorageId,
    projection: &str,
    signal: SignalId,
) -> bool {
    let Some(selected) = storage_binding_layout_slice(design, storage, projection) else {
        return storage_binding_slice(design, storage, projection)
            .is_some_and(|(_, width)| design.signal_width(signal) == Some(width));
    };
    if design.signal_width(signal) == Some(selected.width) {
        return true;
    }
    let Some(signal_layout) = signal_layout(design, &[signal]) else {
        return false;
    };
    scalar_logic_encoding(design, selected.layout).is_some()
        && scalar_logic_encoding(design, signal_layout).is_some()
}

/// Every persistent value, including recursive structs/arrays, has one exact
/// packed LLVM frame. Bindings must identify a concrete region whose width
/// agrees with the flattened DUT signal.
pub(super) fn storage_state_width(design: &Design, storage: ProcessStorageId) -> Option<u32> {
    let metadata = design.process_ir.storages.get(storage.0 as usize)?;
    let width = storage_width(design, storage)?;
    metadata
        .bindings
        .iter()
        .all(|binding| {
            storage_binding_is_compatible(design, storage, &binding.projection, binding.signal)
        })
        .then_some(width)
}

pub(super) fn packed_logic_layout<'a>(
    design: &'a Design,
    layout: &'a SourceLayout,
) -> Option<(u32, &'a siox::ir::LogicEncoding)> {
    let LayoutKind::Packed {
        width,
        element_enum: Some(element),
        ..
    } = &layout.kind
    else {
        return None;
    };
    let encoding = design.logic_encodings.get(element)?;
    Some((*width, encoding))
}

/// Give a resize operand the packed element contract of its destination while
/// retaining the operand's own element count. Conversion syntax may wrap a
/// scalar element or an untyped concatenation, neither of which owns a packed
/// `SourceLayout`; their value graph still carries the exact width and the
/// destination supplies the element enum needed for metadata propagation.
pub(super) fn packed_resize_operand_layout(
    target: &SourceLayout,
    width: u32,
    span: siox::diag::Span,
) -> Option<SourceLayout> {
    let LayoutKind::Packed {
        family,
        element_enum: Some(element_enum),
        ..
    } = &target.kind
    else {
        return None;
    };
    (width != 0).then(|| SourceLayout {
        span,
        kind: LayoutKind::Packed {
            width,
            family: family.clone(),
            range: Some(LayoutRange {
                left: 0,
                right: i64::from(width) - 1,
            }),
            element_enum: Some(element_enum.clone()),
        },
    })
}

/// Select the concrete packed contract an operand already owns, or derive one
/// from the result's element enum when the value is an untyped numeric
/// intermediate. Source-defined numeric operators intentionally mix a fixed
/// vector (`self`) with kernel-width arithmetic (`0 - self`); metadata only
/// needs each operand's own width to detect a non-binary element before the
/// result is poisoned at its destination width.
pub(super) fn packed_operand_layout(
    design: &Design,
    target: &SourceLayout,
    operand: ProcessValueId,
) -> Option<SourceLayout> {
    let value = design.process_ir.values.get(operand.0 as usize)?;
    if process_scalar_logic_encoding(design, operand).is_some() {
        return packed_resize_operand_layout(target, 1, value.span);
    }
    process_value_layout(design, operand)
        .filter(|layout| packed_logic_layout(design, layout).is_some_and(|(width, _)| width != 0))
        .cloned()
        .or_else(|| packed_resize_operand_layout(target, value.bit_width?, value.span))
}

/// Give an arithmetic dependency its declaration-owned packed layout when it
/// has one, otherwise evaluate the unsized intermediate in the result
/// context. A raw resize needs the operand's exact width; arithmetic instead
/// inherits context width while fixed locals, signals, and storage must still
/// read their own metadata planes.
pub(super) fn packed_arithmetic_operand_layout(
    design: &Design,
    target: &SourceLayout,
    operand: ProcessValueId,
) -> Option<SourceLayout> {
    packed_logic_layout(design, target)?;
    process_value_layout(design, operand)
        .filter(|layout| packed_logic_layout(design, layout).is_some_and(|(width, _)| width != 0))
        .cloned()
        .or_else(|| Some(target.clone()))
}

/// Metadata regions use the same offsets as value regions, scaled by four.
/// Non-packed fields are padding: scalar enums already store discriminants in
/// the value frame. Allocate a plane only when a recursive packed leaf needs it.
pub(super) fn layout_has_packed_metadata(design: &Design, layout: &SourceLayout) -> bool {
    match &layout.kind {
        LayoutKind::Packed { .. } => packed_logic_layout(design, layout).is_some(),
        LayoutKind::Array { element, .. } => layout_has_packed_metadata(design, element),
        LayoutKind::Struct { fields, .. } => fields
            .iter()
            .any(|field| layout_has_packed_metadata(design, &field.layout)),
        LayoutKind::Scalar { .. } | LayoutKind::Opaque { .. } => false,
    }
}

pub(super) fn layout_meta_width(design: &Design, layout: &SourceLayout) -> Option<u32> {
    layout_has_packed_metadata(design, layout).then(|| layout_width(layout)?.checked_mul(4))?
}

/// Initialization CFGs own all explicit initializer writes for their root.
/// The retained storage initializer is metadata, not a second execution path.
pub(super) fn storage_initialized_by_cfg(design: &Design, storage: ProcessStorageId) -> bool {
    let Some(storage) = design.process_ir.storages.get(storage.0 as usize) else {
        return false;
    };
    design.process_ir.processes.iter().any(|process| {
        process.owner == storage.owner
            && matches!(process.activation, ProcessActivation::Initialization)
    })
}

pub(super) fn storage_meta_width(design: &Design, storage: ProcessStorageId) -> Option<u32> {
    let storage = design.process_ir.storages.get(storage.0 as usize)?;
    let layout = storage.layout.as_ref().or_else(|| {
        storage
            .ty
            .as_ref()
            .and_then(|ty| layout_for_type(design, ty))
    })?;
    layout_meta_width(design, layout)
}

pub(super) fn local_meta_width(
    design: &Design,
    process: ProcessId,
    local: ProcessLocalId,
) -> Option<u32> {
    let local = design
        .process_ir
        .processes
        .get(process.0 as usize)?
        .locals
        .get(local.0 as usize)?;
    let layout = local
        .layout
        .as_ref()
        .or_else(|| local.ty.as_ref().and_then(|ty| layout_for_type(design, ty)))?;
    layout_meta_width(design, layout)
}

pub(super) fn layout_default(design: &Design, layout: Option<&siox::ir::SourceLayout>) -> u64 {
    let Some(layout) = layout else { return 0 };
    let key = match &layout.kind {
        LayoutKind::Scalar {
            nominal: Some(name),
            ..
        } => Some(name.as_str()),
        LayoutKind::Scalar {
            domain: siox::ir::ScalarDomain::Enum(name),
            ..
        }
        | LayoutKind::Packed { family: name, .. } => Some(name.as_str()),
        _ => None,
    };
    key.and_then(|key| design.new_defaults.get(key).copied())
        .unwrap_or(0)
}

pub(super) fn storage_default(design: &Design, storage: ProcessStorageId) -> u64 {
    layout_default(
        design,
        design
            .process_ir
            .storages
            .get(storage.0 as usize)
            .and_then(|storage| storage.layout.as_ref()),
    )
}

pub(super) fn local_default(
    design: &Design,
    process: siox::ir::ProcessId,
    local: ProcessLocalId,
) -> u64 {
    layout_default(
        design,
        design
            .process_ir
            .processes
            .get(process.0 as usize)
            .and_then(|process| process.locals.get(local.0 as usize))
            .and_then(|local| local.layout.as_ref()),
    )
}
