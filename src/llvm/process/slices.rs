//! Layout slices and packed-bit extraction.

use super::*;

/// One recursively selected region within a packed process-frame value.
/// Aggregate children are laid out in source order from least-significant to
/// most-significant bits; this convention is internal to the design object.
#[derive(Clone, Copy)]
pub(super) struct LayoutSlice<'a> {
    pub(super) layout: &'a SourceLayout,
    pub(super) offset: u32,
    pub(super) width: u32,
}

pub(super) fn field_slice<'a>(layout: &'a SourceLayout, field: &str) -> Option<LayoutSlice<'a>> {
    let LayoutKind::Struct { fields, .. } = &layout.kind else {
        return None;
    };
    let mut offset = 0u32;
    for candidate in fields {
        let width = layout_width(&candidate.layout)?;
        if candidate.name == field {
            return Some(LayoutSlice {
                layout: &candidate.layout,
                offset,
                width,
            });
        }
        offset = offset.checked_add(width)?;
    }
    None
}

pub(super) fn array_slice(layout: &SourceLayout, index: i64) -> Option<LayoutSlice<'_>> {
    let LayoutKind::Array {
        range: Some(range),
        element,
    } = &layout.kind
    else {
        return None;
    };
    let length = u32::try_from(range.len()?).ok()?;
    let position = if range.ascending() {
        index.checked_sub(range.left)?
    } else {
        range.left.checked_sub(index)?
    };
    let position = u32::try_from(position).ok()?;
    if position >= length {
        return None;
    }
    let width = layout_width(element)?;
    Some(LayoutSlice {
        layout: element,
        offset: position.checked_mul(width)?,
        width,
    })
}

pub(super) fn packed_label_position(range: LayoutRange, label: i64) -> Option<u32> {
    let low = range.left.min(range.right);
    let high = range.left.max(range.right);
    (low..=high)
        .contains(&label)
        .then(|| u32::try_from(label.checked_sub(low)?).ok())?
}

pub(super) fn reverse_bits<'ctx>(
    builder: &Builder<'ctx>,
    value: IntValue<'ctx>,
) -> Option<IntValue<'ctx>> {
    let width = value.get_type().get_bit_width();
    let mut reversed = value.get_type().const_zero();
    for source in 0..width {
        let bit = extract_region(builder, value, source, 1)?;
        reversed = insert_region(builder, reversed, bit, width.checked_sub(source + 1)?, 1)?;
    }
    Some(reversed)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn packed_slice_value<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    base: ProcessValueId,
    left: i64,
    right: i64,
    active: Option<IntValue<'ctx>>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    let layout = process_value_layout(design, base)?;
    let LayoutKind::Packed {
        width: base_width,
        range: Some(range),
        ..
    } = layout.kind
    else {
        return None;
    };
    let width = u32::try_from(left.abs_diff(right).checked_add(1)?).ok()?;
    let source = process_value_at(
        context,
        module,
        builder,
        design,
        base,
        base_width,
        false,
        active,
        index_sites,
        cache,
    )?;
    let ty = context
        .custom_width_int_type(std::num::NonZeroU32::new(width)?)
        .ok()?;
    let mut result = ty.const_zero();
    let step = if left <= right { 1i64 } else { -1i64 };
    let mut label = left;
    for position in 0..width {
        let source_position = packed_label_position(range, label)?;
        let destination = width.checked_sub(position + 1)?;
        let bit = extract_region(builder, source, source_position, 1)?;
        result = insert_region(builder, result, bit, destination, 1)?;
        if position + 1 != width {
            label = label.checked_add(step)?;
        }
    }
    Some(result)
}

pub(super) fn packed_string_meta<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    design: &Design,
    text: &str,
    width: u32,
    element: &str,
    encoding: &siox::ir::LogicEncoding,
) -> Option<IntValue<'ctx>> {
    let symbols = design.enum_syms.get(element)?;
    let characters = text.chars().collect::<Vec<_>>();
    if u32::try_from(characters.len()).ok()? != width {
        return None;
    }
    let meta_width = width.checked_mul(4)?;
    let ty = context
        .custom_width_int_type(std::num::NonZeroU32::new(meta_width)?)
        .ok()?;
    let nibble = context
        .custom_width_int_type(std::num::NonZeroU32::new(4)?)
        .ok()?;
    let mut result = ty.const_zero();
    for (position, character) in characters.into_iter().enumerate() {
        let quoted = format!("'{character}'");
        let discriminant = symbols.iter().find_map(|(discriminant, symbol)| {
            (symbol == &quoted || symbol == &character.to_string()).then_some(*discriminant)
        })?;
        if encoding.binary.contains(&discriminant) {
            continue;
        }
        let source_position = u32::try_from(position).ok()?;
        let element_position = width.checked_sub(source_position.checked_add(1)?)?;
        result = insert_region(
            builder,
            result,
            nibble.const_int(discriminant, false),
            element_position.checked_mul(4)?,
            4,
        )?;
    }
    Some(result)
}

pub(super) fn packed_meta_unknown<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    metadata: IntValue<'ctx>,
    elements: u32,
    encoding: &siox::ir::LogicEncoding,
) -> Option<IntValue<'ctx>> {
    let mut any = context.bool_type().const_zero();
    for position in 0..elements {
        let discriminant = extract_region(builder, metadata, position.checked_mul(4)?, 4)?;
        for unknown in &encoding.unknown {
            let equal = builder
                .build_int_compare(
                    IntPredicate::EQ,
                    discriminant,
                    discriminant.get_type().const_int(*unknown, false),
                    "pv.meta.unknown.member",
                )
                .ok()?;
            any = builder.build_or(any, equal, "pv.meta.unknown.any").ok()?;
        }
    }
    Some(any)
}

pub(super) fn packed_unknown_meta<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    elements: u32,
    encoding: &siox::ir::LogicEncoding,
) -> Option<IntValue<'ctx>> {
    let width = elements.checked_mul(4)?;
    let ty = context
        .custom_width_int_type(std::num::NonZeroU32::new(width)?)
        .ok()?;
    let nibble = context
        .custom_width_int_type(std::num::NonZeroU32::new(4)?)
        .ok()?;
    let unknown = encoding.canonical_unknown()?;
    let mut result = ty.const_zero();
    for position in 0..elements {
        result = insert_region(
            builder,
            result,
            nibble.const_int(unknown, false),
            position.checked_mul(4)?,
            4,
        )?;
    }
    Some(result)
}
