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
    /// A `std::ffi::c_float`, stored as a `float`.
    Float,
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
/// has no C scalar (an enum, a `Char`).
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
        // `c_float` is a `real` by type; its name says it is a C `float`.
        LayoutKind::Scalar {
            width: 64,
            domain: ScalarDomain::Real,
            nominal: Some(name),
            ..
        } if name.rsplit("::").next() == Some("c_float") => Some(LeafKind::Float),
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
        // A packed vector's value plane holds one bit per element, whatever
        // its element type (`unsigned`'s are `Bit`).
        LayoutKind::Packed { width, family, .. } if *width <= 64 => {
            int(*width, family.rsplit("::").next() == Some("signed"))
        }
        // Wider than a word: little-endian 64-bit words, the low word first.
        LayoutKind::Packed { width, .. } => {
            let words = width.div_ceil(64);
            for word in 0..words {
                path.push(word);
                leaves.push(CLeaf {
                    offset: offset.checked_add(word * 64)?,
                    width: (width - word * 64).min(64),
                    kind: LeafKind::Int {
                        bytes: 8,
                        signed: false,
                    },
                    path: path.clone(),
                });
                path.pop();
            }
            return Some(context.i64_type().array_type(words).into());
        }
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
        LeafKind::Float => context.f32_type().into(),
        LeafKind::Int { bytes, .. } => context
            .custom_width_int_type(std::num::NonZeroU32::new(bytes * 8)?)
            .ok()?
            .into(),
    })
}

/// Size and alignment of a C object of `ty`, under the natural alignment
/// C gives these scalars on x86-64 and AArch64.
fn size_align(ty: BasicTypeEnum<'_>) -> (u64, u64) {
    match ty {
        BasicTypeEnum::IntType(t) => {
            let bytes = u64::from(t.get_bit_width()).div_ceil(8);
            (bytes, bytes)
        }
        BasicTypeEnum::ArrayType(t) => {
            let (size, align) = size_align(t.get_element_type());
            (size * u64::from(t.len()), align)
        }
        BasicTypeEnum::StructType(t) => {
            let (mut end, mut align) = (0u64, 1u64);
            for field in t.get_field_types() {
                let (size, field_align) = size_align(field);
                end = end.next_multiple_of(field_align) + size;
                align = align.max(field_align);
            }
            (end.next_multiple_of(align), align)
        }
        _ => (8, 8),
    }
}

/// The byte offset of the element `path` names inside a C object of `ty`.
fn byte_offset(ty: BasicTypeEnum<'_>, path: &[u32]) -> u64 {
    let Some((&first, rest)) = path.split_first() else {
        return 0;
    };
    match ty {
        BasicTypeEnum::ArrayType(t) => {
            let element = t.get_element_type();
            size_align(element).0 * u64::from(first) + byte_offset(element, rest)
        }
        BasicTypeEnum::StructType(t) => {
            let fields = t.get_field_types();
            let mut at = 0u64;
            for field in &fields[..first as usize] {
                let (size, align) = size_align(*field);
                at = at.next_multiple_of(align) + size;
            }
            let field = fields[first as usize];
            at.next_multiple_of(size_align(field).1) + byte_offset(field, rest)
        }
        _ => 0,
    }
}

/// How the platform returns a C aggregate.
pub(crate) enum CReturn<'c> {
    /// In registers, as this LLVM type (stored to memory to read the fields).
    Direct(BasicTypeEnum<'c>),
    /// Through a caller-allocated buffer passed as a hidden `sret` pointer.
    Indirect,
}

/// The build host's convention for returning `aggregate` (x86-64 SysV or
/// AAPCS64), or `None` on another platform.
pub(crate) fn c_return<'c>(
    context: &'c Context,
    aggregate: &CAggregate<'c>,
) -> Option<CReturn<'c>> {
    let triple = inkwell::targets::TargetMachine::get_default_triple();
    let triple = triple.as_str().to_string_lossy().into_owned();
    let (size, _) = size_align(aggregate.ty);
    let all = |kind: fn(&LeafKind) -> bool| aggregate.leaves.iter().all(|leaf| kind(&leaf.kind));
    let doubles = all(|kind| matches!(kind, LeafKind::Double));
    let floats = all(|kind| matches!(kind, LeafKind::Float));
    if triple.starts_with("x86_64") && !triple.contains("windows") {
        if size > 16 {
            return Some(CReturn::Indirect);
        }
        // Each eightbyte is SSE when only doubles or floats fill it, else
        // INTEGER. An SSE eightbyte comes back in a vector register's low 64
        // bits, read here as a `double` whatever floats it holds.
        let eightbytes = size.div_ceil(8);
        let parts = (0..eightbytes)
            .map(|index| -> Option<BasicTypeEnum> {
                let (start, end) = (index * 8, index * 8 + 8);
                let mut inside = aggregate.leaves.iter().filter(|leaf| {
                    let at = byte_offset(aggregate.ty, &leaf.path);
                    at >= start && at < end
                });
                if inside.all(|leaf| matches!(leaf.kind, LeafKind::Double | LeafKind::Float)) {
                    Some(context.f64_type().into())
                } else {
                    let bits = std::num::NonZeroU32::new((size - start).min(8) as u32 * 8)?;
                    Some(context.custom_width_int_type(bits).ok()?.into())
                }
            })
            .collect::<Option<Vec<_>>>()?;
        return Some(CReturn::Direct(match parts.as_slice() {
            [one] => *one,
            _ => context.struct_type(&parts, false).into(),
        }));
    }
    if triple.starts_with("aarch64") || triple.starts_with("arm64") {
        // A homogeneous aggregate of up to four doubles (or floats) returns
        // in v0-v3.
        let count = aggregate.leaves.len() as u32;
        if doubles && count <= 4 {
            return Some(CReturn::Direct(context.f64_type().array_type(count).into()));
        }
        if floats && count <= 4 {
            return Some(CReturn::Direct(context.f32_type().array_type(count).into()));
        }
        return Some(match size {
            0..=8 => CReturn::Direct(context.i64_type().into()),
            9..=16 => CReturn::Direct(context.i64_type().array_type(2).into()),
            _ => CReturn::Indirect,
        });
    }
    None
}

/// A buffer of `ty` in the function's entry block: a loop must not take
/// new stack on every pass.
pub(crate) fn entry_alloca<'c>(
    context: &'c Context,
    builder: &Builder<'c>,
    ty: BasicTypeEnum<'c>,
) -> Option<inkwell::values::PointerValue<'c>> {
    let function = builder.get_insert_block()?.get_parent()?;
    let entry = function.get_first_basic_block()?;
    let at_entry = context.create_builder();
    match entry.get_first_instruction() {
        Some(first) => at_entry.position_before(&first),
        None => at_entry.position_at_end(entry),
    }
    at_entry.build_alloca(ty, "c.aggregate").ok()
}

/// A buffer for `aggregate`, filled from the packed `value`.
pub(crate) fn c_aggregate_store<'c>(
    context: &'c Context,
    builder: &Builder<'c>,
    aggregate: &CAggregate<'c>,
    value: inkwell::values::IntValue<'c>,
) -> Option<inkwell::values::PointerValue<'c>> {
    let buffer = entry_alloca(context, builder, aggregate.ty)?;
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
            LeafKind::Float => {
                let double = builder
                    .build_bit_cast(bits, context.f64_type(), "c.double")
                    .ok()?
                    .into_float_value();
                builder
                    .build_float_trunc(double, context.f32_type(), "c.float")
                    .ok()?
                    .into()
            }
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
            LeafKind::Float => {
                let value = builder
                    .build_load(context.f32_type(), pointer, "c.float")
                    .ok()?
                    .into_float_value();
                let double = builder
                    .build_float_ext(value, context.f64_type(), "c.double")
                    .ok()?;
                builder
                    .build_bit_cast(double, leaf_type, "c.leaf")
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
