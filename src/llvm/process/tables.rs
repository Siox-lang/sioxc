//! Constant tables and strings in the design object.

use super::*;

/// Emit one externally visible, immutable `u32` value.
pub(super) fn u32_global<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    name: &str,
    value: u32,
) {
    let ty = context.i32_type();
    let global = module.add_global(ty, None, name);
    global.set_initializer(&ty.const_int(u64::from(value), false));
    global.set_constant(true);
}

/// Emit an externally visible `u32[]`, retaining one zero sentinel for an
/// empty logical table because C cannot portably declare a zero-sized array.
pub(super) fn u32_table<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    name: &str,
    values: &[u32],
) {
    let ty = context.i32_type();
    let values = values
        .iter()
        .copied()
        .map(|value| ty.const_int(u64::from(value), false))
        .collect::<Vec<_>>();
    let fallback = [ty.const_zero()];
    let initializer = ty.const_array(if values.is_empty() {
        &fallback
    } else {
        &values
    });
    let global = module.add_global(initializer.get_type(), None, name);
    global.set_initializer(&initializer);
    global.set_constant(true);
}

/// Emit an externally visible `u8[]`, with the same empty-table convention as
/// [`u32_table`].
pub(super) fn u8_table<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    name: &str,
    values: &[u8],
) {
    let ty = context.i8_type();
    let values = values
        .iter()
        .copied()
        .map(|value| ty.const_int(u64::from(value), false))
        .collect::<Vec<_>>();
    let fallback = [ty.const_zero()];
    let initializer = ty.const_array(if values.is_empty() {
        &fallback
    } else {
        &values
    });
    let global = module.add_global(initializer.get_type(), None, name);
    global.set_initializer(&initializer);
    global.set_constant(true);
}

/// Emit an externally visible `u64[]`, retaining one zero sentinel for an
/// empty logical table.
pub(super) fn u64_table<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    name: &str,
    values: &[u64],
) {
    let ty = context.i64_type();
    let values = values
        .iter()
        .copied()
        .map(|value| ty.const_int(value, false))
        .collect::<Vec<_>>();
    let fallback = [ty.const_zero()];
    let initializer = ty.const_array(if values.is_empty() {
        &fallback
    } else {
        &values
    });
    let global = module.add_global(initializer.get_type(), None, name);
    global.set_initializer(&initializer);
    global.set_constant(true);
}

/// Emit a private NUL-terminated string and return its constant address.
pub(super) fn private_string<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    name: &str,
    value: &str,
) -> PointerValue<'ctx> {
    if let Some(global) = module.get_global(name) {
        return global.as_pointer_value();
    }
    let initializer = context.const_string(value.as_bytes(), true);
    let global = module.add_global(initializer.get_type(), None, name);
    global.set_initializer(&initializer);
    global.set_constant(true);
    global.set_linkage(Linkage::Private);
    global.as_pointer_value()
}

/// Record the source read before leaving an entry on a host failure. The
/// legacy reset helper has no entry failure block; its caller checks the error.
pub(super) fn emit_host_read_check<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    span: siox::diag::Span,
) -> Option<()> {
    let i32 = context.i32_type();
    let note = module
        .get_function("sx_runtime_note_location")
        .unwrap_or_else(|| {
            module.add_function(
                "sx_runtime_note_location",
                context
                    .void_type()
                    .fn_type(&[i32.into(), i32.into()], false),
                Some(Linkage::External),
            )
        });
    builder
        .build_call(
            note,
            &[
                i32.const_int(u64::from(span.file.0), false).into(),
                i32.const_int(u64::from(span.start), false).into(),
            ],
            "",
        )
        .ok()?;
    let function = builder.get_insert_block()?.get_parent()?;
    let Some(failed) = function
        .get_basic_blocks()
        .into_iter()
        .find(|block| block.get_name().to_bytes() == b"runtime.failed")
    else {
        return Some(());
    };
    let error = module.get_function("sx_runtime_error").unwrap_or_else(|| {
        module.add_function(
            "sx_runtime_error",
            context
                .ptr_type(AddressSpace::default())
                .fn_type(&[], false),
            Some(Linkage::External),
        )
    });
    let inkwell::values::ValueKind::Basic(error) = builder
        .build_call(error, &[], "pv.read.error")
        .ok()?
        .try_as_basic_value()
    else {
        return None;
    };
    let failed_read = builder
        .build_is_not_null(error.into_pointer_value(), "pv.read.failed")
        .ok()?;
    let continuation = context.append_basic_block(function, "read.ready");
    builder
        .build_conditional_branch(failed_read, failed, continuation)
        .ok()?;
    builder.position_at_end(continuation);
    Some(())
}

/// Read one exact-width value through a fixed word-buffer runtime ABI. LLVM
/// assembles those words into the actual integer type, so neither raw binary
/// nor fixed UTF-8 construction needs a per-design C type or width.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_buffered_read_value<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    path: ProcessValueId,
    width: u32,
    runtime_name: &str,
    capacity: u32,
    span: siox::diag::Span,
) -> Option<IntValue<'ctx>> {
    let text = process_string(design, path)?;
    let path = private_string(
        context,
        module,
        &format!("sx.host.buffered.path.{}.{}", path.0, runtime_name),
        text,
    );
    let i8 = context.i8_type();
    let i32 = context.i32_type();
    let i64 = context.i64_type();
    let word_count = super::super::words_for(width);
    let buffer = builder
        .build_array_alloca(
            i64,
            i32.const_int(u64::from(word_count), false),
            "pv.buffered.words",
        )
        .ok()?;
    let runtime = module.get_function(runtime_name).unwrap_or_else(|| {
        module.add_function(
            runtime_name,
            i8.fn_type(
                &[
                    path.get_type().into(),
                    path.get_type().into(),
                    i32.into(),
                    i32.into(),
                ],
                false,
            ),
            Some(Linkage::External),
        )
    });
    builder
        .build_call(
            runtime,
            &[
                path.into(),
                buffer.into(),
                i32.const_int(u64::from(word_count), false).into(),
                i32.const_int(u64::from(capacity), false).into(),
            ],
            "pv.buffered.read",
        )
        .ok()?;
    emit_host_read_check(context, module, builder, span)?;

    let ty = context
        .custom_width_int_type(std::num::NonZeroU32::new(width)?)
        .ok()?;
    let mut result = ty.const_zero();
    for word in 0..word_count {
        let pointer = unsafe {
            builder
                .build_in_bounds_gep(
                    i64,
                    buffer,
                    &[i32.const_int(u64::from(word), false)],
                    "pv.buffered.word.ptr",
                )
                .ok()?
        };
        let part = builder
            .build_load(i64, pointer, "pv.buffered.word")
            .ok()?
            .into_int_value();
        let part = fit(builder, part, width)?;
        let shift = word.checked_mul(64)?;
        let part = if shift == 0 {
            part
        } else {
            builder
                .build_left_shift(
                    part,
                    ty.const_int(u64::from(shift), false),
                    "pv.buffered.shift",
                )
                .ok()?
        };
        result = builder.build_or(result, part, "pv.buffered.value").ok()?;
    }
    Some(result)
}

/// Emit an externally visible NUL-terminated string.
pub(super) fn public_string<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    name: &str,
    value: &str,
) {
    let initializer = context.const_string(value.as_bytes(), true);
    let global = module.add_global(initializer.get_type(), None, name);
    global.set_initializer(&initializer);
    global.set_constant(true);
}

/// Emit an externally visible `const char *const[]`. The strings themselves
/// remain private object data; the fixed runtime consumes only their pointers.
pub(super) fn string_table<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    name: &str,
    value_prefix: &str,
    values: &[String],
) {
    let pointer = context.ptr_type(AddressSpace::default());
    let values = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            private_string(context, module, &format!("{value_prefix}.{index}"), value)
        })
        .collect::<Vec<_>>();
    let fallback = [pointer.const_null()];
    let initializer = pointer.const_array(if values.is_empty() {
        &fallback
    } else {
        &values
    });
    let global = module.add_global(initializer.get_type(), None, name);
    global.set_initializer(&initializer);
    global.set_constant(true);
}

/// A source string carried as a Process IR operand. Runtime messages are data
/// in the object, not fragments of generated source code.
pub(super) fn process_string(design: &Design, id: ProcessValueId) -> Option<&str> {
    let value = design.process_ir.values.get(id.0 as usize)?;
    match &value.kind {
        ProcessValueKind::String(value) => Some(value),
        _ => None,
    }
}

/// A fixed `Char[N]` value can use the formatting ABI directly without a
/// runtime-owned dynamic string object. Return its element/count only when the
/// recursive layout proves every packed slice is one character.
pub(super) fn process_fixed_string_layout(layout: &SourceLayout) -> Option<(&SourceLayout, u32)> {
    let LayoutKind::Array {
        range: Some(range),
        element,
    } = &layout.kind
    else {
        return None;
    };
    if !matches!(
        element.kind,
        LayoutKind::Scalar {
            domain: siox::ir::ScalarDomain::Character,
            ..
        }
    ) {
        return None;
    }
    let length = u32::try_from(range.len()?).ok()?;
    (layout_width(element)? <= 32).then_some((element, length))
}

/// Empty arrays have no nonzero LLVM integer frame. They are nevertheless a
/// complete string value and formatting them correctly means appending no
/// characters rather than rejecting the enclosing runtime instruction.
pub(super) fn process_empty_string(design: &Design, id: ProcessValueId) -> bool {
    let value = match design.process_ir.values.get(id.0 as usize) {
        Some(value) => value,
        None => return false,
    };
    let ty = value.ty.as_ref().or_else(|| match &value.kind {
        ProcessValueKind::Storage(storage) => design
            .process_ir
            .storages
            .get(storage.0 as usize)
            .and_then(|storage| storage.ty.as_ref()),
        ProcessValueKind::Local { process, local } => design
            .process_ir
            .processes
            .get(process.0 as usize)
            .and_then(|process| process.locals.get(local.0 as usize))
            .and_then(|local| local.ty.as_ref()),
        _ => None,
    });
    matches!(
        ty,
        Some(siox::types::Ty::Array {
            elem,
            len: 0,
            family: None,
        }) if matches!(elem.as_ref(), siox::types::Ty::Char)
    )
}

/// Equality over two zero-element character arrays is a complete constant
/// operation even though neither operand has a nonzero LLVM storage frame.
pub(super) fn process_empty_string_comparison(
    design: &Design,
    operation: &ProcessBinaryOp,
    left: ProcessValueId,
    right: ProcessValueId,
) -> Option<bool> {
    if !process_empty_string(design, left) || !process_empty_string(design, right) {
        return None;
    }
    match operation {
        ProcessBinaryOp::Eq => Some(true),
        ProcessBinaryOp::Ne => Some(false),
        _ => None,
    }
}
