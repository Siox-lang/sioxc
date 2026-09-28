//! Formatted output and runtime instructions.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_format_integer<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    value: ProcessValueId,
    signed: bool,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<()> {
    let width = design.process_ir.values.get(value.0 as usize)?.bit_width?;
    let emitted = process_value_at(
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
    )?;
    let i32 = context.i32_type();
    let i64 = context.i64_type();
    let pointer = context.ptr_type(AddressSpace::default());
    let word_count = super::super::words_for(width);
    let array = i64.array_type(word_count);
    let words = builder.build_alloca(array, "process.format.words").ok()?;
    for word in 0..word_count {
        let offset = word.checked_mul(super::super::ABI_WORD_BITS)?;
        let part = if offset == 0 {
            emitted
        } else {
            builder
                .build_right_shift(
                    emitted,
                    emitted.get_type().const_int(u64::from(offset), false),
                    false,
                    "process.format.word.shift",
                )
                .ok()?
        };
        let destination = unsafe {
            builder
                .build_in_bounds_gep(
                    array,
                    words,
                    &[i32.const_zero(), i32.const_int(u64::from(word), false)],
                    "process.format.word.pointer",
                )
                .ok()?
        };
        builder
            .build_store(destination, fit(builder, part, 64)?)
            .ok()?;
    }
    let name = if signed {
        "sx_runtime_format_signed"
    } else {
        "sx_runtime_format_unsigned"
    };
    let runtime = module.get_function(name).unwrap_or_else(|| {
        module.add_function(
            name,
            context
                .void_type()
                .fn_type(&[pointer.into(), i32.into(), i32.into()], false),
            Some(Linkage::External),
        )
    });
    builder
        .build_call(
            runtime,
            &[
                words.into(),
                i32.const_int(u64::from(word_count), false).into(),
                i32.const_int(u64::from(width), false).into(),
            ],
            "",
        )
        .ok()?;
    Some(())
}

pub(super) fn emit_format_character<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    value: IntValue<'ctx>,
) -> Option<()> {
    let value = fit(builder, value, 32)?;
    let runtime = module
        .get_function("sx_runtime_format_char")
        .unwrap_or_else(|| {
            module.add_function(
                "sx_runtime_format_char",
                context
                    .void_type()
                    .fn_type(&[context.i32_type().into()], false),
                Some(Linkage::External),
            )
        });
    builder.build_call(runtime, &[value.into()], "").ok()?;
    Some(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_process_format<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    process: ProcessId,
    block: siox::ir::ProcessBlockId,
    instruction: usize,
    format: &[ProcessFormatPart],
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<PointerValue<'ctx>> {
    let pointer = context.ptr_type(AddressSpace::default());
    let begin = module
        .get_function("sx_runtime_format_begin")
        .unwrap_or_else(|| {
            module.add_function(
                "sx_runtime_format_begin",
                context.void_type().fn_type(&[], false),
                Some(Linkage::External),
            )
        });
    builder.build_call(begin, &[], "").ok()?;
    let append_text = module
        .get_function("sx_runtime_format_text")
        .unwrap_or_else(|| {
            module.add_function(
                "sx_runtime_format_text",
                context.void_type().fn_type(&[pointer.into()], false),
                Some(Linkage::External),
            )
        });
    for (part_index, part) in format.iter().enumerate() {
        let text = match part {
            ProcessFormatPart::Text(text) => Some(text.as_str()),
            ProcessFormatPart::Value {
                value,
                kind: ProcessDisplayKind::String,
            } => process_string(design, *value),
            _ => None,
        };
        if let Some(text) = text {
            let text = private_string(
                context,
                module,
                &format!(
                    "sx.process.format.{}.{}.{}.{}",
                    process.0, block.0, instruction, part_index
                ),
                text,
            );
            builder.build_call(append_text, &[text.into()], "").ok()?;
            continue;
        }
        let ProcessFormatPart::Value { value, kind } = part else {
            return None;
        };
        match kind {
            ProcessDisplayKind::Unsigned | ProcessDisplayKind::Signed => {
                emit_format_integer(
                    context,
                    module,
                    builder,
                    design,
                    *value,
                    matches!(kind, ProcessDisplayKind::Signed),
                    index_sites,
                    cache,
                )?;
            }
            ProcessDisplayKind::Real => {
                let value = process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    *value,
                    64,
                    false,
                    None,
                    index_sites,
                    cache,
                )?;
                let runtime = module
                    .get_function("sx_runtime_format_real")
                    .unwrap_or_else(|| {
                        module.add_function(
                            "sx_runtime_format_real",
                            context
                                .void_type()
                                .fn_type(&[context.i64_type().into()], false),
                            Some(Linkage::External),
                        )
                    });
                builder.build_call(runtime, &[value.into()], "").ok()?;
            }
            ProcessDisplayKind::Character => {
                let value = process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    *value,
                    32,
                    false,
                    None,
                    index_sites,
                    cache,
                )?;
                emit_format_character(context, module, builder, value)?;
            }
            ProcessDisplayKind::Enum(name) => {
                let value = process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    *value,
                    64,
                    false,
                    None,
                    index_sites,
                    cache,
                )?;
                let fallback = private_string(
                    context,
                    module,
                    &format!(
                        "sx.process.format.enum.{}.{}.{}.{}.fallback",
                        process.0, block.0, instruction, part_index
                    ),
                    "?",
                );
                let mut selected = fallback;
                let mut symbols = design.enum_syms.get(name)?.iter().collect::<Vec<_>>();
                symbols.sort_by_key(|(discriminant, _)| **discriminant);
                for (symbol_index, (discriminant, symbol)) in symbols.into_iter().enumerate() {
                    let symbol = private_string(
                        context,
                        module,
                        &format!(
                            "sx.process.format.enum.{}.{}.{}.{}.{}",
                            process.0, block.0, instruction, part_index, symbol_index
                        ),
                        symbol,
                    );
                    let matches = builder
                        .build_int_compare(
                            IntPredicate::EQ,
                            value,
                            context.i64_type().const_int(*discriminant, false),
                            "process.format.enum.matches",
                        )
                        .ok()?;
                    selected = builder
                        .build_select(matches, symbol, selected, "process.format.enum.symbol")
                        .ok()?
                        .into_pointer_value();
                }
                builder
                    .build_call(append_text, &[selected.into()], "")
                    .ok()?;
            }
            ProcessDisplayKind::String => {
                if process_empty_string(design, *value) {
                    continue;
                }
                let layout = process_value_layout(design, *value)?;
                let (element, length) = process_fixed_string_layout(layout)?;
                let element_width = layout_width(element)?;
                let aggregate = process_value_in_layout(
                    context,
                    module,
                    builder,
                    design,
                    *value,
                    layout,
                    None,
                    index_sites,
                    cache,
                )?;
                for position in 0..length {
                    let character = extract_region(
                        builder,
                        aggregate,
                        position.checked_mul(element_width)?,
                        element_width,
                    )?;
                    emit_format_character(context, module, builder, character)?;
                }
            }
        }
    }
    let end = module
        .get_function("sx_runtime_format_end")
        .unwrap_or_else(|| {
            module.add_function(
                "sx_runtime_format_end",
                pointer.fn_type(&[], false),
                Some(Linkage::External),
            )
        });
    match builder
        .build_call(end, &[], "process.format.message")
        .ok()?
        .try_as_basic_value()
    {
        inkwell::values::ValueKind::Basic(value) => Some(value.into_pointer_value()),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_runtime_instruction<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: FunctionValue<'ctx>,
    failed_block: BasicBlock<'ctx>,
    design: &Design,
    process: ProcessId,
    block: siox::ir::ProcessBlockId,
    instruction: usize,
    operation: &ProcessRuntimeOp,
    arguments: &[ProcessValueId],
    format: &Option<Vec<ProcessFormatPart>>,
    span: siox::diag::Span,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<()> {
    if matches!(operation, ProcessRuntimeOp::Print) {
        let message = match format {
            Some(format) => emit_process_format(
                context,
                module,
                builder,
                design,
                process,
                block,
                instruction,
                format,
                index_sites,
                cache,
            )?,
            None => {
                let [message] = arguments else { return None };
                let message = process_string(design, *message)?;
                private_string(
                    context,
                    module,
                    &format!(
                        "sx.process.runtime.message.{}.{}.{}",
                        process.0, block.0, instruction
                    ),
                    message,
                )
            }
        };
        let runtime = module.get_function("sx_runtime_print").unwrap_or_else(|| {
            module.add_function(
                "sx_runtime_print",
                context
                    .void_type()
                    .fn_type(&[context.ptr_type(AddressSpace::default()).into()], false),
                Some(Linkage::External),
            )
        });
        builder.build_call(runtime, &[message.into()], "").ok()?;
        return Some(());
    }

    if matches!(operation, ProcessRuntimeOp::Seed) {
        let [seed] = arguments else { return None };
        let seed = process_value_at(
            context,
            module,
            builder,
            design,
            *seed,
            64,
            false,
            None,
            index_sites,
            cache,
        )?;
        let runtime = module.get_function("sx_runtime_seed").unwrap_or_else(|| {
            module.add_function(
                "sx_runtime_seed",
                context
                    .void_type()
                    .fn_type(&[context.i64_type().into()], false),
                Some(Linkage::External),
            )
        });
        builder.build_call(runtime, &[seed.into()], "").ok()?;
        return Some(());
    }

    let condition = process_value(
        context,
        module,
        builder,
        design,
        *arguments.first()?,
        None,
        index_sites,
        cache,
    )
    .and_then(|value| as_condition(builder, value))?;
    let formatted_continuation = format.as_ref().map(|_| {
        let report = context.append_basic_block(
            function,
            &format!("bb{}.runtime{}.report", block.0, instruction),
        );
        let continuation = context.append_basic_block(
            function,
            &format!("bb{}.runtime{}.continue", block.0, instruction),
        );
        builder
            .build_conditional_branch(condition, continuation, report)
            .ok()?;
        builder.position_at_end(report);
        Some(continuation)
    });
    let formatted_continuation = match formatted_continuation {
        Some(continuation) => Some(continuation?),
        None => None,
    };
    let condition_byte = builder
        .build_int_z_extend(condition, context.i8_type(), "process.runtime.condition")
        .ok()?;
    let fallback = match operation {
        ProcessRuntimeOp::Assert => "assertion failed",
        ProcessRuntimeOp::Warn => "warning",
        ProcessRuntimeOp::Print | ProcessRuntimeOp::Seed | ProcessRuntimeOp::Call(_) => {
            return None;
        }
    };
    let message = match format {
        Some(format) => emit_process_format(
            context,
            module,
            builder,
            design,
            process,
            block,
            instruction,
            format,
            index_sites,
            cache,
        )?,
        None => {
            let message = arguments
                .get(1)
                .and_then(|message| process_string(design, *message))
                .unwrap_or(fallback);
            private_string(
                context,
                module,
                &format!(
                    "sx.process.runtime.message.{}.{}.{}",
                    process.0, block.0, instruction
                ),
                message,
            )
        }
    };
    let i32 = context.i32_type();
    let arguments = &[
        condition_byte.into(),
        message.into(),
        i32.const_int(u64::from(span.file.0), false).into(),
        i32.const_int(u64::from(span.start), false).into(),
    ];
    match operation {
        ProcessRuntimeOp::Assert => {
            let runtime = module.get_function("sx_runtime_assert").unwrap_or_else(|| {
                module.add_function(
                    "sx_runtime_assert",
                    context.i8_type().fn_type(
                        &[
                            context.i8_type().into(),
                            context.ptr_type(AddressSpace::default()).into(),
                            i32.into(),
                            i32.into(),
                        ],
                        false,
                    ),
                    Some(Linkage::External),
                )
            });
            let failed = match builder
                .build_call(runtime, arguments, "process.runtime.assert")
                .ok()?
                .try_as_basic_value()
            {
                inkwell::values::ValueKind::Basic(value) => value.into_int_value(),
                _ => return None,
            };
            let failed = builder
                .build_int_compare(
                    IntPredicate::NE,
                    failed,
                    context.i8_type().const_zero(),
                    "process.runtime.assert.failed",
                )
                .ok()?;
            let continuation = formatted_continuation.unwrap_or_else(|| {
                context.append_basic_block(
                    function,
                    &format!("bb{}.runtime{}.continue", block.0, instruction),
                )
            });
            builder
                .build_conditional_branch(failed, failed_block, continuation)
                .ok()?;
            builder.position_at_end(continuation);
        }
        ProcessRuntimeOp::Warn => {
            let runtime = module.get_function("sx_runtime_warn").unwrap_or_else(|| {
                module.add_function(
                    "sx_runtime_warn",
                    context.void_type().fn_type(
                        &[
                            context.i8_type().into(),
                            context.ptr_type(AddressSpace::default()).into(),
                            i32.into(),
                            i32.into(),
                        ],
                        false,
                    ),
                    Some(Linkage::External),
                )
            });
            builder.build_call(runtime, arguments, "").ok()?;
            if let Some(continuation) = formatted_continuation {
                builder.build_unconditional_branch(continuation).ok()?;
                builder.position_at_end(continuation);
            }
        }
        ProcessRuntimeOp::Print | ProcessRuntimeOp::Seed | ProcessRuntimeOp::Call(_) => {
            return None;
        }
    }
    Some(())
}
