//! The C side of an `extern "C"` call: `std::ffi` scalars narrower than the
//! 64-bit ABI word or `double`, shared by both call emitters.

use crate::ir::CScalar;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue};

/// Bits of a C `long` on the build host: 32 on Windows (LLP64), 64 elsewhere.
fn long_bits() -> u32 {
    let triple = inkwell::targets::TargetMachine::get_default_triple();
    if triple.as_str().to_string_lossy().contains("windows") {
        32
    } else {
        64
    }
}

/// Bits `scalar` occupies at the C boundary.
fn bits(scalar: CScalar) -> u32 {
    match scalar {
        CScalar::I8 | CScalar::U8 => 8,
        CScalar::I16 | CScalar::U16 => 16,
        CScalar::I32 | CScalar::U32 | CScalar::Float => 32,
        CScalar::Long | CScalar::ULong => long_bits(),
    }
}

/// The integer type of `scalar`'s C width.
fn int_type(context: &Context, scalar: CScalar) -> inkwell::types::IntType<'_> {
    match bits(scalar) {
        8 => context.i8_type(),
        16 => context.i16_type(),
        32 => context.i32_type(),
        _ => context.i64_type(),
    }
}

/// The LLVM type `scalar` crosses the C boundary as.
pub(crate) fn c_type(context: &Context, scalar: CScalar) -> BasicTypeEnum<'_> {
    match scalar {
        CScalar::Float => context.f32_type().into(),
        _ => int_type(context, scalar).into(),
    }
}

/// Narrow an argument from the ABI word or `double` to `scalar`.
pub(crate) fn c_argument<'c>(
    context: &'c Context,
    builder: &Builder<'c>,
    value: BasicMetadataValueEnum<'c>,
    scalar: CScalar,
) -> Option<BasicMetadataValueEnum<'c>> {
    Some(match value {
        BasicMetadataValueEnum::FloatValue(value) => builder
            .build_float_trunc(value, context.f32_type(), "c.float")
            .ok()?
            .into(),
        BasicMetadataValueEnum::IntValue(value) if bits(scalar) < 64 => builder
            .build_int_truncate(value, int_type(context, scalar), "c.arg")
            .ok()?
            .into(),
        other => other,
    })
}

/// Widen a C result from `scalar` back to the ABI word or `double`.
pub(crate) fn c_result<'c>(
    context: &'c Context,
    builder: &Builder<'c>,
    value: BasicValueEnum<'c>,
    scalar: CScalar,
) -> Option<BasicValueEnum<'c>> {
    Some(match value {
        BasicValueEnum::FloatValue(value) => builder
            .build_float_ext(value, context.f64_type(), "c.double")
            .ok()?
            .into(),
        BasicValueEnum::IntValue(value) if value.get_type().get_bit_width() < 64 => {
            let word = context.i64_type();
            if scalar.signed() {
                builder.build_int_s_extend(value, word, "c.word")
            } else {
                builder.build_int_z_extend(value, word, "c.word")
            }
            .ok()?
            .into()
        }
        other => other,
    })
}

/// Mark integers narrower than `int` `signext`/`zeroext`, as a C compiler
/// does: some ABIs leave extending them to the caller.
pub(crate) fn mark_extensions(
    context: &Context,
    function: FunctionValue<'_>,
    arguments: &[Option<CScalar>],
    result: Option<CScalar>,
) {
    let mark = |location: AttributeLoc, scalar: CScalar| {
        if scalar != CScalar::Float && bits(scalar) < 32 {
            let name = if scalar.signed() {
                "signext"
            } else {
                "zeroext"
            };
            let kind = Attribute::get_named_enum_kind_id(name);
            function.add_attribute(location, context.create_enum_attribute(kind, 0));
        }
    };
    for (index, scalar) in arguments.iter().enumerate() {
        if let Some(scalar) = scalar {
            mark(AttributeLoc::Param(index as u32), *scalar);
        }
    }
    if let Some(scalar) = result {
        mark(AttributeLoc::Return, scalar);
    }
}

/// How one scalar leaf of an aggregate is held in C memory.
#[derive(Clone, Copy)]
enum LeafKind {
    /// An integer of `bytes` bytes, extended from the leaf by its sign.
    Int { bytes: u32, signed: bool },
    /// A `real`, stored as a `double`.
    Double,
}

/// One scalar leaf of an aggregate crossing to C: where it sits in the
/// packed siox value, and where in the C object.
#[derive(Clone)]
pub(crate) struct CLeaf {
    offset: u32,
    width: u32,
    kind: LeafKind,
    path: Vec<u32>,
}

/// A struct or array of C scalars, laid out as C lays it out: LLVM's own
/// struct layout follows the platform's natural alignment.
#[derive(Clone)]
pub(crate) struct CAggregate<'c> {
    pub(crate) ty: inkwell::types::BasicTypeEnum<'c>,
    leaves: Vec<CLeaf>,
}

/// The C object a struct or array layout crosses as, or `None` when a leaf
/// has no C scalar (an enum, a `Char`, a vector wider than 64 bits).
pub(crate) fn c_aggregate<'c>(
    context: &'c Context,
    layout: &crate::ir::SourceLayout,
) -> Option<CAggregate<'c>> {
    let mut leaves = Vec::new();
    let ty = aggregate_type(context, layout, 0, &mut Vec::new(), &mut leaves)?;
    Some(CAggregate { ty, leaves })
}

fn aggregate_type<'c>(
    context: &'c Context,
    layout: &crate::ir::SourceLayout,
    offset: u32,
    path: &mut Vec<u32>,
    leaves: &mut Vec<CLeaf>,
) -> Option<BasicTypeEnum<'c>> {
    use crate::ir::{LayoutKind, ScalarDomain};
    let int = |width: u32, signed: bool| {
        let bytes = [1, 2, 4, 8].into_iter().find(|bytes| bytes * 8 >= width)?;
        Some(LeafKind::Int { bytes, signed })
    };
    // A ranged integer is the smallest C integer holding its range (`c_int`
    // is 4 bytes), whatever width it is stored in.
    let ranged = |left: i64, right: i64| {
        let signed = left < 0;
        let bytes = [1u32, 2, 4, 8].into_iter().find(|bytes| {
            let bits = bytes * 8;
            if signed {
                let low = -(1i128 << (bits - 1));
                let high = (1i128 << (bits - 1)) - 1;
                i128::from(left) >= low && i128::from(right) <= high
            } else {
                i128::from(right) < (1i128 << bits)
            }
        })?;
        Some(LeafKind::Int { bytes, signed })
    };
    let kind = match &layout.kind {
        LayoutKind::Scalar {
            width,
            domain: ScalarDomain::Real,
            ..
        } if *width == 64 => Some(LeafKind::Double),
        LayoutKind::Scalar {
            domain: ScalarDomain::Integer,
            value_range: Some((left, right)),
            ..
        } => ranged(*left, *right),
        LayoutKind::Scalar {
            width,
            domain: ScalarDomain::Integer,
            ..
        } => int(*width, true),
        LayoutKind::Scalar {
            width,
            domain: ScalarDomain::Natural | ScalarDomain::Bits,
            ..
        } => int(*width, false),
        LayoutKind::Packed {
            width,
            family,
            element_enum: None,
            ..
        } => int(*width, family.rsplit("::").next() == Some("signed")),
        LayoutKind::Struct { fields, .. } => {
            let mut types = Vec::with_capacity(fields.len());
            let mut at = offset;
            for (index, field) in fields.iter().enumerate() {
                path.push(index as u32);
                types.push(aggregate_type(context, &field.layout, at, path, leaves)?);
                path.pop();
                at = at.checked_add(field.layout.packed_width()?)?;
            }
            return Some(context.struct_type(&types, false).into());
        }
        LayoutKind::Array {
            range: Some(range),
            element,
        } => {
            let length = u32::try_from(range.len()?).ok()?;
            let width = element.packed_width()?;
            let mut ty = None;
            for position in 0..length {
                path.push(position);
                ty = Some(aggregate_type(
                    context,
                    element,
                    offset.checked_add(position.checked_mul(width)?)?,
                    path,
                    leaves,
                )?);
                path.pop();
            }
            return Some(match ty? {
                BasicTypeEnum::IntType(t) => t.array_type(length).into(),
                BasicTypeEnum::FloatType(t) => t.array_type(length).into(),
                BasicTypeEnum::StructType(t) => t.array_type(length).into(),
                BasicTypeEnum::ArrayType(t) => t.array_type(length).into(),
                _ => return None,
            });
        }
        _ => None,
    }?;
    let width = layout.packed_width()?;
    leaves.push(CLeaf {
        offset,
        width,
        kind,
        path: path.clone(),
    });
    Some(match kind {
        LeafKind::Double => context.f64_type().into(),
        LeafKind::Int { bytes, .. } => context
            .custom_width_int_type(std::num::NonZeroU32::new(bytes * 8)?)
            .ok()?
            .into(),
    })
}

/// A buffer for `aggregate` in the function's entry block (a loop must not
/// take new stack on every pass), filled from the packed `value`.
pub(crate) fn c_aggregate_store<'c>(
    context: &'c Context,
    builder: &Builder<'c>,
    aggregate: &CAggregate<'c>,
    value: inkwell::values::IntValue<'c>,
) -> Option<inkwell::values::PointerValue<'c>> {
    let function = builder.get_insert_block()?.get_parent()?;
    let entry = function.get_first_basic_block()?;
    let at_entry = context.create_builder();
    match entry.get_first_instruction() {
        Some(first) => at_entry.position_before(&first),
        None => at_entry.position_at_end(entry),
    }
    let buffer = at_entry.build_alloca(aggregate.ty, "c.aggregate").ok()?;
    // Padding is zero, not whatever the stack held, before C sees it: an
    // aggregate store leaves padding undefined, so clear the bytes.
    builder
        .build_memset(
            buffer,
            8,
            context.i8_type().const_zero(),
            inkwell::types::BasicType::size_of(&aggregate.ty)?,
        )
        .ok()?;
    let word = value.get_type();
    for leaf in &aggregate.leaves {
        let shifted = builder
            .build_right_shift(
                value,
                word.const_int(u64::from(leaf.offset), false),
                false,
                "c.leaf.shift",
            )
            .ok()?;
        let bits = builder
            .build_int_truncate_or_bit_cast(
                shifted,
                context
                    .custom_width_int_type(std::num::NonZeroU32::new(leaf.width)?)
                    .ok()?,
                "c.leaf",
            )
            .ok()?;
        let stored: BasicValueEnum = match leaf.kind {
            LeafKind::Double => builder
                .build_bit_cast(bits, context.f64_type(), "c.double")
                .ok()?,
            LeafKind::Int { bytes, signed } => {
                let ty = context
                    .custom_width_int_type(std::num::NonZeroU32::new(bytes * 8)?)
                    .ok()?;
                builder
                    .build_int_cast_sign_flag(bits, ty, signed, "c.int")
                    .ok()?
                    .into()
            }
        };
        builder
            .build_store(
                leaf_pointer(context, builder, aggregate, buffer, leaf)?,
                stored,
            )
            .ok()?;
    }
    Some(buffer)
}

/// The packed siox value of what C left in `buffer`, `width` bits wide.
pub(crate) fn c_aggregate_load<'c>(
    context: &'c Context,
    builder: &Builder<'c>,
    aggregate: &CAggregate<'c>,
    buffer: inkwell::values::PointerValue<'c>,
    width: u32,
) -> Option<inkwell::values::IntValue<'c>> {
    let word = context
        .custom_width_int_type(std::num::NonZeroU32::new(width)?)
        .ok()?;
    let mut packed = word.const_zero();
    for leaf in &aggregate.leaves {
        let pointer = leaf_pointer(context, builder, aggregate, buffer, leaf)?;
        let leaf_type = context
            .custom_width_int_type(std::num::NonZeroU32::new(leaf.width)?)
            .ok()?;
        let bits = match leaf.kind {
            LeafKind::Double => {
                let value = builder
                    .build_load(context.f64_type(), pointer, "c.double")
                    .ok()?;
                builder
                    .build_bit_cast(value, leaf_type, "c.leaf")
                    .ok()?
                    .into_int_value()
            }
            LeafKind::Int { bytes, signed } => {
                let ty = context
                    .custom_width_int_type(std::num::NonZeroU32::new(bytes * 8)?)
                    .ok()?;
                let value = builder
                    .build_load(ty, pointer, "c.int")
                    .ok()?
                    .into_int_value();
                builder
                    .build_int_cast_sign_flag(value, leaf_type, signed, "c.leaf")
                    .ok()?
            }
        };
        let widened = builder
            .build_int_z_extend_or_bit_cast(bits, word, "c.leaf.word")
            .ok()?;
        let placed = builder
            .build_left_shift(
                widened,
                word.const_int(u64::from(leaf.offset), false),
                "c.leaf.place",
            )
            .ok()?;
        packed = builder.build_or(packed, placed, "c.packed").ok()?;
    }
    Some(packed)
}

fn leaf_pointer<'c>(
    context: &'c Context,
    builder: &Builder<'c>,
    aggregate: &CAggregate<'c>,
    buffer: inkwell::values::PointerValue<'c>,
    leaf: &CLeaf,
) -> Option<inkwell::values::PointerValue<'c>> {
    let i32 = context.i32_type();
    let indices: Vec<_> = std::iter::once(i32.const_zero())
        .chain(
            leaf.path
                .iter()
                .map(|index| i32.const_int(u64::from(*index), false)),
        )
        .collect();
    // SAFETY: every index comes from the layout the type was built from.
    unsafe {
        builder
            .build_in_bounds_gep(aggregate.ty, buffer, &indices, "c.leaf.at")
            .ok()
    }
}
