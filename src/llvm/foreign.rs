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
