//! Binary operators and dynamic indexing.

use super::*;

/// Emit a normalized scalar binary operation with the same defined corner
/// cases as the established hardware LLVM path.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_binary<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    operation: &ProcessBinaryOp,
    left: ProcessValueId,
    right: ProcessValueId,
    result_width: u32,
    active: Option<IntValue<'ctx>>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    let left_width = design.process_ir.values.get(left.0 as usize)?.bit_width?;
    let right_width = design.process_ir.values.get(right.0 as usize)?.bit_width?;

    if matches!(
        operation,
        ProcessBinaryOp::FloatAdd
            | ProcessBinaryOp::FloatSub
            | ProcessBinaryOp::FloatMul
            | ProcessBinaryOp::FloatDiv
    ) {
        let float = context.f64_type();
        let left = process_value_at(
            context,
            module,
            builder,
            design,
            left,
            64,
            false,
            active,
            index_sites,
            cache,
        )?;
        let right = process_value_at(
            context,
            module,
            builder,
            design,
            right,
            64,
            false,
            active,
            index_sites,
            cache,
        )?;
        let left = builder
            .build_bit_cast(left, float, "pv.fa")
            .ok()?
            .into_float_value();
        let right = builder
            .build_bit_cast(right, float, "pv.fb")
            .ok()?
            .into_float_value();
        let result = match operation {
            ProcessBinaryOp::FloatAdd => builder.build_float_add(left, right, "pv.fadd").ok()?,
            ProcessBinaryOp::FloatSub => builder.build_float_sub(left, right, "pv.fsub").ok()?,
            ProcessBinaryOp::FloatMul => builder.build_float_mul(left, right, "pv.fmul").ok()?,
            ProcessBinaryOp::FloatDiv => builder.build_float_div(left, right, "pv.fdiv").ok()?,
            _ => return None,
        };
        let bits = builder
            .build_bit_cast(result, context.i64_type(), "pv.fbits")
            .ok()?
            .into_int_value();
        return fit(builder, bits, result_width);
    }

    if matches!(
        operation,
        ProcessBinaryOp::FloatEq
            | ProcessBinaryOp::FloatNe
            | ProcessBinaryOp::FloatLt
            | ProcessBinaryOp::FloatLe
            | ProcessBinaryOp::FloatGt
            | ProcessBinaryOp::FloatGe
    ) {
        let float = context.f64_type();
        let left = process_value_at(
            context,
            module,
            builder,
            design,
            left,
            64,
            false,
            active,
            index_sites,
            cache,
        )?;
        let right = process_value_at(
            context,
            module,
            builder,
            design,
            right,
            64,
            false,
            active,
            index_sites,
            cache,
        )?;
        let left = builder
            .build_bit_cast(left, float, "pv.fa")
            .ok()?
            .into_float_value();
        let right = builder
            .build_bit_cast(right, float, "pv.fb")
            .ok()?
            .into_float_value();
        let predicate = match operation {
            ProcessBinaryOp::FloatEq => inkwell::FloatPredicate::OEQ,
            ProcessBinaryOp::FloatNe => inkwell::FloatPredicate::UNE,
            ProcessBinaryOp::FloatLt => inkwell::FloatPredicate::OLT,
            ProcessBinaryOp::FloatLe => inkwell::FloatPredicate::OLE,
            ProcessBinaryOp::FloatGt => inkwell::FloatPredicate::OGT,
            ProcessBinaryOp::FloatGe => inkwell::FloatPredicate::OGE,
            _ => return None,
        };
        let result = builder
            .build_float_compare(predicate, left, right, "pv.fcmp")
            .ok()?;
        return fit(builder, result, result_width);
    }

    let comparison = matches!(
        operation,
        ProcessBinaryOp::Eq
            | ProcessBinaryOp::Ne
            | ProcessBinaryOp::Lt
            | ProcessBinaryOp::Le
            | ProcessBinaryOp::Gt
            | ProcessBinaryOp::Ge
            | ProcessBinaryOp::SignedLt
            | ProcessBinaryOp::SignedLe
            | ProcessBinaryOp::SignedGt
            | ProcessBinaryOp::SignedGe
    );
    let signed = matches!(
        operation,
        ProcessBinaryOp::SignedAdd
            | ProcessBinaryOp::SignedSub
            | ProcessBinaryOp::SignedMul
            | ProcessBinaryOp::SignedDiv
            | ProcessBinaryOp::SignedLt
            | ProcessBinaryOp::SignedLe
            | ProcessBinaryOp::SignedGt
            | ProcessBinaryOp::SignedGe
            | ProcessBinaryOp::ArithmeticShr
    );
    let packed_comparison_width = comparison.then(|| {
        [left, right]
            .into_iter()
            .find_map(|operand| process_value_packed_width(design, operand))
    });
    let operand_width = packed_comparison_width.flatten().unwrap_or_else(|| {
        if comparison {
            left_width.max(right_width)
        } else {
            result_width
        }
    });
    // Equality has no signed LLVM predicate, but scalar mathematical values
    // still need signed widening before their bit patterns can be compared.
    // A constrained `integer<-16..15>` held in five bits must compare equal
    // to the i64 spelling `-2`, rather than being zero-extended to +30.
    // Packed families deliberately retain their selected-width bit-pattern
    // comparison (`signed[16](...) == 65520`), so they do not take this path.
    let signed_equality = matches!(operation, ProcessBinaryOp::Eq | ProcessBinaryOp::Ne)
        && packed_comparison_width.flatten().is_none()
        && (process_value_is_signed(design, left) || process_value_is_signed(design, right));
    let signed_operands = signed || signed_equality;
    let operand_width = if signed_operands {
        operand_width.checked_add(1)?
    } else {
        operand_width
    };
    if operand_width > super::super::emit::LLVM_MAX_INT_BITS {
        return None;
    }

    if matches!(
        operation,
        ProcessBinaryOp::Shl | ProcessBinaryOp::Shr | ProcessBinaryOp::ArithmeticShr
    ) {
        let shift_width = operand_width.max(right_width);
        let left = process_value_at(
            context,
            module,
            builder,
            design,
            left,
            shift_width,
            matches!(operation, ProcessBinaryOp::ArithmeticShr),
            active,
            index_sites,
            cache,
        )?;
        let right = process_value_at(
            context,
            module,
            builder,
            design,
            right,
            shift_width,
            false,
            active,
            index_sites,
            cache,
        )?;
        let ty = left.get_type();
        let limit = ty.const_int(u64::from(operand_width), false);
        let out_of_range = builder
            .build_int_compare(IntPredicate::UGE, right, limit, "pv.shift.oob")
            .ok()?;
        let zero = ty.const_zero();
        let safe = builder
            .build_select(out_of_range, zero, right, "pv.shift.amount")
            .ok()?
            .into_int_value();
        let shifted = if matches!(operation, ProcessBinaryOp::Shl) {
            builder.build_left_shift(left, safe, "pv.shl").ok()?
        } else {
            builder
                .build_right_shift(
                    left,
                    safe,
                    matches!(operation, ProcessBinaryOp::ArithmeticShr),
                    "pv.shr",
                )
                .ok()?
        };
        let out_of_range_value = if matches!(operation, ProcessBinaryOp::ArithmeticShr) {
            let negative = builder
                .build_int_compare(IntPredicate::SLT, left, zero, "pv.shift.negative")
                .ok()?;
            builder
                .build_select(negative, ty.const_all_ones(), zero, "pv.shift.fill")
                .ok()?
                .into_int_value()
        } else {
            zero
        };
        let result = builder
            .build_select(out_of_range, out_of_range_value, shifted, "pv.shift.result")
            .ok()?
            .into_int_value();
        return fit(builder, result, result_width);
    }

    let left = process_value_at(
        context,
        module,
        builder,
        design,
        left,
        operand_width,
        signed_operands,
        active,
        index_sites,
        cache,
    )?;
    let right_active = if left_width == 1
        && right_width == 1
        && cache.contains_check(right)
        && matches!(operation, ProcessBinaryOp::And | ProcessBinaryOp::Or)
    {
        let left_condition = as_condition(builder, left)?;
        let required = if matches!(operation, ProcessBinaryOp::And) {
            left_condition
        } else {
            builder
                .build_not(left_condition, "pv.or.right.active")
                .ok()?
        };
        Some(match active {
            Some(active) => builder
                .build_and(active, required, "pv.logical.right.active")
                .ok()?,
            None => required,
        })
    } else {
        active
    };
    let right = process_value_at(
        context,
        module,
        builder,
        design,
        right,
        operand_width,
        signed_operands,
        right_active,
        index_sites,
        cache,
    )?;
    let compare = |predicate, name| {
        let value = builder
            .build_int_compare(predicate, left, right, name)
            .ok()?;
        fit(builder, value, result_width)
    };
    let result = match operation {
        ProcessBinaryOp::Add | ProcessBinaryOp::SignedAdd => {
            builder.build_int_add(left, right, "pv.add").ok()?
        }
        ProcessBinaryOp::Sub | ProcessBinaryOp::SignedSub => {
            builder.build_int_sub(left, right, "pv.sub").ok()?
        }
        ProcessBinaryOp::Mul | ProcessBinaryOp::SignedMul => {
            builder.build_int_mul(left, right, "pv.mul").ok()?
        }
        ProcessBinaryOp::Div => {
            let zero = left.get_type().const_zero();
            let one = left.get_type().const_int(1, false);
            let is_zero = builder
                .build_int_compare(IntPredicate::EQ, right, zero, "pv.div.zero")
                .ok()?;
            let safe = builder
                .build_select(is_zero, one, right, "pv.div.denominator")
                .ok()?
                .into_int_value();
            let quotient = builder.build_int_unsigned_div(left, safe, "pv.div").ok()?;
            builder
                .build_select(is_zero, zero, quotient, "pv.div.result")
                .ok()?
                .into_int_value()
        }
        ProcessBinaryOp::SignedDiv => {
            let ty = left.get_type();
            let zero = ty.const_zero();
            let one = ty.const_int(1, false);
            let negative_one = ty.const_all_ones();
            let minimum = builder
                .build_left_shift(
                    one,
                    ty.const_int(u64::from(operand_width - 1), false),
                    "pv.sdiv.minimum",
                )
                .ok()?;
            let is_zero = builder
                .build_int_compare(IntPredicate::EQ, right, zero, "pv.sdiv.zero")
                .ok()?;
            let is_minimum = builder
                .build_int_compare(IntPredicate::EQ, left, minimum, "pv.sdiv.is_minimum")
                .ok()?;
            let is_negative_one = builder
                .build_int_compare(
                    IntPredicate::EQ,
                    right,
                    negative_one,
                    "pv.sdiv.is_negative_one",
                )
                .ok()?;
            let overflow = builder
                .build_and(is_minimum, is_negative_one, "pv.sdiv.overflow")
                .ok()?;
            let unsafe_divisor = builder.build_or(is_zero, overflow, "pv.sdiv.unsafe").ok()?;
            let safe = builder
                .build_select(unsafe_divisor, one, right, "pv.sdiv.denominator")
                .ok()?
                .into_int_value();
            let quotient = builder.build_int_signed_div(left, safe, "pv.sdiv").ok()?;
            let quotient = builder
                .build_select(overflow, minimum, quotient, "pv.sdiv.overflow.result")
                .ok()?
                .into_int_value();
            builder
                .build_select(is_zero, zero, quotient, "pv.sdiv.result")
                .ok()?
                .into_int_value()
        }
        ProcessBinaryOp::And => builder.build_and(left, right, "pv.and").ok()?,
        ProcessBinaryOp::Or => builder.build_or(left, right, "pv.or").ok()?,
        ProcessBinaryOp::Xor => builder.build_xor(left, right, "pv.xor").ok()?,
        ProcessBinaryOp::Eq => return compare(IntPredicate::EQ, "pv.eq"),
        ProcessBinaryOp::Ne => return compare(IntPredicate::NE, "pv.ne"),
        ProcessBinaryOp::Lt => return compare(IntPredicate::ULT, "pv.lt"),
        ProcessBinaryOp::Le => return compare(IntPredicate::ULE, "pv.le"),
        ProcessBinaryOp::Gt => return compare(IntPredicate::UGT, "pv.gt"),
        ProcessBinaryOp::Ge => return compare(IntPredicate::UGE, "pv.ge"),
        ProcessBinaryOp::SignedLt => return compare(IntPredicate::SLT, "pv.slt"),
        ProcessBinaryOp::SignedLe => return compare(IntPredicate::SLE, "pv.sle"),
        ProcessBinaryOp::SignedGt => return compare(IntPredicate::SGT, "pv.sgt"),
        ProcessBinaryOp::SignedGe => return compare(IntPredicate::SGE, "pv.sge"),
        ProcessBinaryOp::Shl
        | ProcessBinaryOp::Shr
        | ProcessBinaryOp::ArithmeticShr
        | ProcessBinaryOp::FloatAdd
        | ProcessBinaryOp::FloatSub
        | ProcessBinaryOp::FloatMul
        | ProcessBinaryOp::FloatDiv
        | ProcessBinaryOp::FloatEq
        | ProcessBinaryOp::FloatNe
        | ProcessBinaryOp::FloatLt
        | ProcessBinaryOp::FloatLe
        | ProcessBinaryOp::FloatGt
        | ProcessBinaryOp::FloatGe
        | ProcessBinaryOp::Custom(_) => return None,
    };
    fit(builder, result, result_width)
}

/// Width of the packed-vector domain governing a comparison. Integer
/// literals are polymorphic at a vector comparison site: both
/// `signed[8](x) == -56` and `signed[16](x) == 65520` compare the low 8/16-bit
/// patterns. Widening both sides to the fallback kernel-integer width would
/// make one of those equivalent spellings fail.
pub(super) fn process_value_packed_width(design: &Design, id: ProcessValueId) -> Option<u32> {
    let value = design.process_ir.values.get(id.0 as usize)?;
    if matches!(
        value.ty,
        Some(siox::types::Ty::Array {
            family: Some(_),
            ..
        })
    ) {
        return value.bit_width;
    }
    match &process_value_layout(design, id)?.kind {
        LayoutKind::Packed { width, .. } => Some(*width),
        _ => None,
    }
}

pub(super) fn aggregate_signal_value<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    signals: &[SignalId],
    state: ProcessSignalState,
    width: u32,
) -> Option<IntValue<'ctx>> {
    if matches!(state, ProcessSignalState::Event) {
        let mut event = context.bool_type().const_zero();
        for signal in signals {
            let leaf = signal_value(context, module, builder, design, &[*signal], state, 1)?;
            event = builder.build_or(event, leaf, "pv.aggregate.event").ok()?;
        }
        return Some(event);
    }
    let ty = context
        .custom_width_int_type(std::num::NonZeroU32::new(width)?)
        .ok()?;
    let mut packed = ty.const_zero();
    let mut offset = 0u32;
    for signal in signals {
        let leaf_width = design.signal_width(*signal)?;
        let leaf = signal_value(
            context,
            module,
            builder,
            design,
            &[*signal],
            state,
            leaf_width,
        )?;
        packed = insert_region(builder, packed, leaf, offset, leaf_width)?;
        offset = offset.checked_add(leaf_width)?;
    }
    (offset == width).then_some(packed)
}

/// Convert a checked logical index into a bounded zero-based storage position.
/// The modulo is semantically inert for a valid index and keeps later LLVM
/// shifts defined on an invalid path while the checked operand latches the
/// source diagnostic.
#[allow(clippy::too_many_arguments)]
pub(super) fn dynamic_index_position<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    index: ProcessValueId,
    range: LayoutRange,
    source_order: bool,
    arithmetic_width: u32,
    active: Option<IntValue<'ctx>>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    let count = u32::try_from(range.len()?).ok()?;
    if count == 0 {
        return None;
    }
    let index = process_value_at(
        context,
        module,
        builder,
        design,
        index,
        arithmetic_width,
        false,
        active,
        index_sites,
        cache,
    )?;
    let ty = index.get_type();
    let origin = if source_order && !range.ascending() {
        range.left
    } else {
        range.left.min(range.right)
    };
    let origin = ty.const_int(origin as u64, true);
    let position = if source_order && !range.ascending() {
        builder
            .build_int_sub(origin, index, "pv.index.position")
            .ok()?
    } else {
        builder
            .build_int_sub(index, origin, "pv.index.position")
            .ok()?
    };
    builder
        .build_int_unsigned_rem(
            position,
            ty.const_int(u64::from(count), false),
            "pv.index.safe_position",
        )
        .ok()
}

/// Select one runtime-indexed region from a recursively packed process value.
#[allow(clippy::too_many_arguments)]
pub(super) fn dynamic_index_region<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    base: ProcessValueId,
    index: ProcessValueId,
    layout: &SourceLayout,
    result_width: u32,
    active: Option<IntValue<'ctx>>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    let base_width = layout_width(layout)?;
    let (range, element_width, source_order) = match &layout.kind {
        LayoutKind::Array {
            range: Some(range),
            element,
        } => (*range, layout_width(element)?, true),
        LayoutKind::Packed {
            range: Some(range), ..
        } => (*range, 1, false),
        _ => return None,
    };
    if element_width != result_width {
        return None;
    }
    let safe_position = dynamic_index_position(
        context,
        module,
        builder,
        design,
        index,
        range,
        source_order,
        base_width,
        active,
        index_sites,
        cache,
    )?;
    let ty = safe_position.get_type();
    let offset = if element_width == 1 {
        safe_position
    } else {
        builder
            .build_int_mul(
                safe_position,
                ty.const_int(u64::from(element_width), false),
                "pv.index.offset",
            )
            .ok()?
    };
    let base = process_value_in_layout(
        context,
        module,
        builder,
        design,
        base,
        layout,
        active,
        index_sites,
        cache,
    )?;
    let shifted = builder
        .build_right_shift(base, offset, false, "pv.index.extract")
        .ok()?;
    fit(builder, shifted, result_width)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn packed_index_discriminant<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    base: ProcessValueId,
    index: ProcessValueId,
    layout: &SourceLayout,
    result_width: u32,
    active: Option<IntValue<'ctx>>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    let (base_width, encoding) = packed_logic_layout(design, layout)?;
    let range = layout.index_range()?;
    let value_bit = dynamic_index_region(
        context,
        module,
        builder,
        design,
        base,
        index,
        layout,
        1,
        active,
        index_sites,
        cache,
    )?;
    let metadata = process_packed_meta_in_layout(
        context,
        module,
        builder,
        design,
        base,
        layout,
        active,
        index_sites,
        cache,
    )?;
    let position = dynamic_index_position(
        context,
        module,
        builder,
        design,
        index,
        range,
        false,
        base_width,
        active,
        index_sites,
        cache,
    )?;
    let offset = builder
        .build_int_mul(
            position,
            position.get_type().const_int(4, false),
            "pv.meta.index.offset",
        )
        .ok()?;
    let offset = fit(builder, offset, metadata.get_type().get_bit_width())?;
    let shifted = builder
        .build_right_shift(metadata, offset, false, "pv.meta.index.extract")
        .ok()?;
    let metadata = fit(builder, shifted, 4)?;
    packed_discriminant(
        context,
        builder,
        encoding,
        value_bit,
        Some(metadata),
        result_width,
    )
}
