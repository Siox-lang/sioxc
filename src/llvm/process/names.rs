//! Names of the emitted state globals, loop shapes, and storage widths.

use super::*;

pub(super) fn local_state_name(process: siox::ir::ProcessId, local: ProcessLocalId) -> String {
    format!("sx.process.local.{}.{}", process.0, local.0)
}

pub(super) fn storage_state_name(storage: ProcessStorageId) -> String {
    format!("sx.process.storage.{}", storage.0)
}

pub(super) fn storage_old_name(storage: ProcessStorageId) -> String {
    format!("sx.process.storage.old.{}", storage.0)
}

pub(super) fn storage_meta_name(storage: ProcessStorageId) -> String {
    format!("sx.process.storage.meta.{}", storage.0)
}

pub(super) fn storage_meta_old_name(storage: ProcessStorageId) -> String {
    format!("sx.process.storage.meta.old.{}", storage.0)
}

pub(super) fn local_meta_name(process: ProcessId, local: ProcessLocalId) -> String {
    format!("sx.process.local.meta.{}.{}", process.0, local.0)
}

pub(super) fn loop_active_name(process: ProcessId, block: siox::ir::ProcessBlockId) -> String {
    format!("sx.process.loop.active.{}.{}", process.0, block.0)
}

pub(super) fn loop_cursor_name(process: ProcessId, block: siox::ir::ProcessBlockId) -> String {
    format!("sx.process.loop.cursor.{}.{}", process.0, block.0)
}

pub(super) fn loop_end_name(process: ProcessId, block: siox::ir::ProcessBlockId) -> String {
    format!("sx.process.loop.end.{}.{}", process.0, block.0)
}

pub(super) fn loop_iterable_name(process: ProcessId, block: siox::ir::ProcessBlockId) -> String {
    format!("sx.process.loop.iterable.{}.{}", process.0, block.0)
}

pub(super) fn range_loop_bounds(
    design: &Design,
    iterable: ProcessValueId,
) -> Option<(ProcessValueId, ProcessValueId)> {
    match &design.process_ir.values.get(iterable.0 as usize)?.kind {
        ProcessValueKind::Range {
            left: Some(left),
            right: Some(right),
        } => Some((*left, *right)),
        _ => None,
    }
}

pub(super) fn array_loop_shape(
    design: &Design,
    iterable: ProcessValueId,
) -> Option<(&SourceLayout, &SourceLayout, u32)> {
    let layout = process_value_layout(design, iterable)?;
    let LayoutKind::Array {
        range: Some(range),
        element,
    } = &layout.kind
    else {
        return None;
    };
    let length = u32::try_from(range.len()?).ok()?;
    (length != 0).then_some((layout, element, length))
}

pub(super) fn layout_width(layout: &siox::ir::SourceLayout) -> Option<u32> {
    layout.packed_width()
}

/// Dynamic UTF-8 strings cross the fixed runtime ABI as opaque handles. Their
/// source type remains the unconstrained `Char[]` array, so its semantic bit
/// width is deliberately zero; only Process-frame storage uses this ABI width.
pub(super) fn runtime_handle_width(ty: Option<&siox::types::Ty>) -> Option<u32> {
    matches!(
        ty,
        Some(siox::types::Ty::Array {
            elem,
            len: 0,
            family: None,
        }) if matches!(elem.as_ref(), siox::types::Ty::Char)
    )
    .then_some(64)
}

pub(super) fn runtime_handle_layout(layout: Option<&SourceLayout>) -> Option<u32> {
    matches!(
        layout.map(|layout| &layout.kind),
        Some(LayoutKind::Array {
            range: None,
            element,
        }) if matches!(
            element.kind,
            LayoutKind::Scalar {
                domain: siox::ir::ScalarDomain::Character,
                ..
            }
        )
    )
    .then_some(64)
}

pub(super) fn initializer_can_raise_host_error(
    design: &Design,
    initializer: ProcessValueId,
) -> bool {
    matches!(
        design
            .process_ir
            .values
            .get(initializer.0 as usize)
            .map(|value| &value.kind),
        Some(ProcessValueKind::HostCall {
            operation: ProcessHostValueOp::ReadUtf8
                | ProcessHostValueOp::ReadUtf8Fixed
                | ProcessHostValueOp::ReadBinary,
            ..
        })
    )
}

pub(super) fn dynamic_string_value(design: &Design, value: ProcessValueId) -> bool {
    design
        .process_ir
        .values
        .get(value.0 as usize)
        .is_some_and(|value| value.bit_width == Some(64))
        && runtime_handle_layout(process_value_layout(design, value)) == Some(64)
}

pub(super) fn local_width(
    design: &Design,
    process: siox::ir::ProcessId,
    local: ProcessLocalId,
) -> Option<u32> {
    let local = design
        .process_ir
        .processes
        .get(process.0 as usize)?
        .locals
        .get(local.0 as usize)?;
    local
        .layout
        .as_ref()
        .and_then(layout_width)
        .or_else(|| {
            local
                .ty
                .as_ref()
                .and_then(siox::types::Ty::bit_width)
                .filter(|width| *width != 0)
        })
        .or_else(|| runtime_handle_width(local.ty.as_ref()))
        .or_else(|| runtime_handle_layout(local.layout.as_ref()))
        .or_else(|| {
            design.process_ir.values.iter().find_map(|value| {
                matches!(
                    value.kind,
                    ProcessValueKind::Local {
                        process: owner,
                        local: id,
                    } if owner == process && id == local.id
                )
                .then_some(value.bit_width)
                .flatten()
            })
        })
        .filter(|width| *width != 0 && *width <= super::super::emit::LLVM_MAX_INT_BITS)
}

pub(super) fn storage_width(design: &Design, storage: ProcessStorageId) -> Option<u32> {
    let storage = design.process_ir.storages.get(storage.0 as usize)?;
    storage
        .layout
        .as_ref()
        .and_then(layout_width)
        .or_else(|| {
            storage
                .ty
                .as_ref()
                .and_then(siox::types::Ty::bit_width)
                .filter(|width| *width != 0)
        })
        .or_else(|| runtime_handle_width(storage.ty.as_ref()))
        .or_else(|| runtime_handle_layout(storage.layout.as_ref()))
        .or_else(|| {
            design.process_ir.values.iter().find_map(|value| {
                matches!(value.kind, ProcessValueKind::Storage(id) if id == storage.id)
                    .then_some(value.bit_width)
                    .flatten()
            })
        })
        .filter(|width| *width != 0 && *width <= super::super::emit::LLVM_MAX_INT_BITS)
}
