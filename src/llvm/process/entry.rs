//! Process entry points and the entry table.

use super::*;

/// Emit a process entry that resumes at any CFG block by its stable block ID.
///
/// Blocks containing directly supported assignments execute and publish
/// staged writes into the object-owned pending state. Unsupported
/// executable nodes return [`PROCESS_UNSUPPORTED`] before a block performs any
/// calls or writes, so an unsupported form cannot produce partial effects.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_entry<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    design: &Design,
    process: &ProcessCfg,
    index_sites: &HashMap<IndexSite, u32>,
    range_sites: &HashMap<siox::diag::Span, u32>,
    checked_values: &[bool],
    supported_values: &ProcessValueSupport,
    schedule_sites: &HashMap<(ProcessId, siox::ir::ProcessBlockId, usize), ScheduleSite>,
) -> FunctionValue<'ctx> {
    let i8 = context.i8_type();
    let i32 = context.i32_type();
    let function = module.add_function(
        &format!("sx.process.{}", process.id.0),
        i8.fn_type(&[i32.into()], false),
        Some(Linkage::Internal),
    );
    let builder = context.create_builder();
    let dispatch = context.append_basic_block(function, "dispatch");
    let invalid = context.append_basic_block(function, "invalid");
    let runtime_failed = context.append_basic_block(function, "runtime.failed");
    let blocks = process
        .blocks
        .iter()
        .map(|block| context.append_basic_block(function, &format!("bb{}", block.id.0)))
        .collect::<Vec<_>>();

    builder.position_at_end(dispatch);
    let resume = function
        .get_first_param()
        .expect("process entry has a resume block")
        .into_int_value();
    let cases = process
        .blocks
        .iter()
        .zip(&blocks)
        .map(|(block, llvm)| (i32.const_int(u64::from(block.id.0), false), *llvm))
        .collect::<Vec<_>>();
    builder.build_switch(resume, invalid, &cases).unwrap();

    builder.position_at_end(invalid);
    builder
        .build_return(Some(&i8.const_int(u64::from(PROCESS_UNSUPPORTED), false)))
        .unwrap();

    builder.position_at_end(runtime_failed);
    builder
        .build_return(Some(&i8.const_int(u64::from(PROCESS_STOPPED), false)))
        .unwrap();

    for (block, llvm) in process.blocks.iter().zip(&blocks) {
        builder.position_at_end(*llvm);
        if !block_is_supported(design, process, block, supported_values) {
            builder
                .build_return(Some(&i8.const_int(u64::from(PROCESS_UNSUPPORTED), false)))
                .unwrap();
            continue;
        }

        let mut cache = ProcessValueCache::new(checked_values, &supported_values.meta_free);
        let mut failed = false;
        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            let emitted = match instruction {
                ProcessInstruction::Declare {
                    local, initializer, ..
                } => local_width(design, process.id, *local).and_then(|width| {
                    let metadata = design
                        .process_ir
                        .processes
                        .get(process.id.0 as usize)?
                        .locals
                        .get(local.0 as usize)?;
                    let value = match initializer {
                        Some(initializer) => {
                            if let Some(layout) = metadata.layout.as_ref() {
                                process_value_in_layout(
                                    context,
                                    module,
                                    &builder,
                                    design,
                                    *initializer,
                                    layout,
                                    None,
                                    index_sites,
                                    &mut cache,
                                )?
                            } else {
                                process_value_at(
                                    context,
                                    module,
                                    &builder,
                                    design,
                                    *initializer,
                                    width,
                                    false,
                                    None,
                                    index_sites,
                                    &mut cache,
                                )?
                            }
                        }
                        None => {
                            if let Some(layout) = metadata.layout.as_ref() {
                                layout_default_value(context, &builder, design, layout)?
                            } else {
                                context
                                    .custom_width_int_type(std::num::NonZeroU32::new(width)?)
                                    .ok()?
                                    .const_int(local_default(design, process.id, *local), false)
                            }
                        }
                    };
                    let metadata = metadata.layout.as_ref().and_then(|layout| {
                        let meta_width = local_meta_width(design, process.id, *local)?;
                        let value = match initializer {
                            Some(initializer) => process_packed_meta_in_layout(
                                context,
                                module,
                                &builder,
                                design,
                                *initializer,
                                layout,
                                None,
                                index_sites,
                                &mut cache,
                            )?,
                            None => context
                                .custom_width_int_type(std::num::NonZeroU32::new(meta_width)?)
                                .ok()?
                                .const_zero(),
                        };
                        Some((meta_width, value))
                    });
                    store_state(
                        module,
                        &builder,
                        &local_state_name(process.id, *local),
                        width,
                        value,
                    )?;
                    if let Some((meta_width, metadata)) = metadata {
                        store_state(
                            module,
                            &builder,
                            &local_meta_name(process.id, *local),
                            meta_width,
                            metadata,
                        )?;
                    }
                    Some(())
                }),
                ProcessInstruction::Assign {
                    semantics: ProcessAssignment::ImmediateLocal,
                    target,
                    value,
                    span,
                    ..
                } => emit_place_assignment(
                    context,
                    module,
                    &builder,
                    design,
                    process.id,
                    ProcessAssignment::ImmediateLocal,
                    *target,
                    *value,
                    *span,
                    index_sites,
                    range_sites,
                    &mut cache,
                ),
                ProcessInstruction::Assign {
                    semantics: ProcessAssignment::ImmediateStorage,
                    target,
                    value,
                    span,
                    ..
                } => emit_place_assignment(
                    context,
                    module,
                    &builder,
                    design,
                    process.id,
                    ProcessAssignment::ImmediateStorage,
                    *target,
                    *value,
                    *span,
                    index_sites,
                    range_sites,
                    &mut cache,
                ),
                ProcessInstruction::Assign {
                    semantics: ProcessAssignment::StagedSignal,
                    target,
                    value,
                    span,
                    ..
                } => (|| {
                    if let Some(signal) = staged_signal_target(design, *target) {
                        let width = design.signal_width(signal)?;
                        let source_width =
                            design.process_ir.values.get(value.0 as usize)?.bit_width?;
                        let ranged = design.signals.get(signal.0 as usize)?.range.is_some();
                        let checked_width = if ranged {
                            width.max(source_width).max(64)
                        } else {
                            width
                        };
                        let value = process_value_at(
                            context,
                            module,
                            &builder,
                            design,
                            *value,
                            checked_width,
                            ranged,
                            None,
                            index_sites,
                            &mut cache,
                        )?;
                        if ranged {
                            latch_range_failure(
                                context,
                                module,
                                &builder,
                                design,
                                signal,
                                value,
                                *span,
                                range_sites,
                            )?;
                        }
                        stage_signal(module, &builder, signal, fit(&builder, value, width)?)
                    } else if let Some(signals) = staged_signal_group(design, *target) {
                        let width = signals.iter().try_fold(0u32, |width, signal| {
                            width.checked_add(design.signal_width(*signal)?)
                        })?;
                        let value = assignment_value(
                            context,
                            module,
                            &builder,
                            design,
                            *target,
                            *value,
                            width,
                            index_sites,
                            &mut cache,
                        )?;
                        stage_signal_group(
                            context,
                            module,
                            &builder,
                            design,
                            signals,
                            value,
                            *span,
                            range_sites,
                        )
                    } else {
                        emit_place_assignment(
                            context,
                            module,
                            &builder,
                            design,
                            process.id,
                            ProcessAssignment::StagedSignal,
                            *target,
                            *value,
                            *span,
                            index_sites,
                            range_sites,
                            &mut cache,
                        )
                    }
                })(),
                ProcessInstruction::Assign {
                    semantics: ProcessAssignment::PerPlace,
                    target,
                    value,
                    span,
                    ..
                } => per_place_targets(design, *target).and_then(|places| {
                    let width = places
                        .iter()
                        .try_fold(0u32, |width, place| width.checked_add(place.width))?;
                    let value = process_value_at(
                        context,
                        module,
                        &builder,
                        design,
                        *value,
                        width,
                        false,
                        None,
                        index_sites,
                        &mut cache,
                    )?;
                    let mut offset = width;
                    for place in places {
                        offset = offset.checked_sub(place.width)?;
                        let part = extract_region(&builder, value, offset, place.width)?;
                        write_static_place(
                            context,
                            module,
                            &builder,
                            design,
                            place,
                            part,
                            None,
                            *span,
                            range_sites,
                        )?;
                    }
                    (offset == 0).then_some(())
                }),
                ProcessInstruction::Schedule {
                    target,
                    value,
                    delay,
                    ..
                } => schedule_sites
                    .get(&(process.id, block.id, instruction_index))
                    .and_then(|site| {
                        emit_schedule_call(
                            context,
                            module,
                            &builder,
                            design,
                            site,
                            *target,
                            *value,
                            *delay,
                            index_sites,
                            &mut cache,
                        )
                    }),
                ProcessInstruction::Runtime {
                    operation,
                    arguments,
                    format,
                    span,
                } => emit_runtime_instruction(
                    context,
                    module,
                    &builder,
                    function,
                    runtime_failed,
                    design,
                    process.id,
                    block.id,
                    instruction_index,
                    operation,
                    arguments,
                    format,
                    *span,
                    index_sites,
                    &mut cache,
                ),
            };
            if emitted.is_none() {
                failed = true;
                break;
            }
            if matches!(
                instruction,
                ProcessInstruction::Declare { .. }
                    | ProcessInstruction::Assign {
                        semantics: ProcessAssignment::ImmediateLocal
                            | ProcessAssignment::ImmediateStorage
                            | ProcessAssignment::PerPlace,
                        ..
                    }
            ) {
                // Immediate writes must invalidate an earlier arena load of
                // the same place. Staged signal writes deliberately do not.
                cache.clear();
            }
        }
        let branch_condition = match &block.terminator {
            ProcessTerminator::Branch { condition, .. } => process_value(
                context,
                module,
                &builder,
                design,
                *condition,
                None,
                index_sites,
                &mut cache,
            )
            .and_then(|condition| as_condition(&builder, condition)),
            _ => None,
        };
        let match_scrutinee = match &block.terminator {
            ProcessTerminator::Match { scrutinee, .. } => process_value(
                context,
                module,
                &builder,
                design,
                *scrutinee,
                None,
                index_sites,
                &mut cache,
            ),
            _ => None,
        };
        if failed
            || matches!(block.terminator, ProcessTerminator::Branch { .. })
                && branch_condition.is_none()
            || matches!(block.terminator, ProcessTerminator::Match { .. })
                && match_scrutinee.is_none()
        {
            builder
                .build_return(Some(&i8.const_int(u64::from(PROCESS_UNSUPPORTED), false)))
                .unwrap();
            continue;
        }

        match &block.terminator {
            ProcessTerminator::Return { value: None, .. } => {
                builder
                    .build_return(Some(&i8.const_int(u64::from(PROCESS_COMPLETED), false)))
                    .unwrap();
            }
            ProcessTerminator::Goto(target) => {
                builder
                    .build_unconditional_branch(blocks[target.0 as usize])
                    .unwrap();
            }
            ProcessTerminator::Stop { .. } => {
                builder
                    .build_return(Some(&i8.const_int(u64::from(PROCESS_STOPPED), false)))
                    .unwrap();
            }
            ProcessTerminator::Finish { .. } => {
                builder
                    .build_return(Some(&i8.const_int(u64::from(PROCESS_FINISHED), false)))
                    .unwrap();
            }
            ProcessTerminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                builder
                    .build_conditional_branch(
                        branch_condition.expect("supported branch condition was emitted"),
                        blocks[then_block.0 as usize],
                        blocks[else_block.0 as usize],
                    )
                    .unwrap();
            }
            ProcessTerminator::Suspend {
                operation: siox::ir::ProcessSuspendOp::AwaitTime,
                arguments,
                resume,
                ..
            } => {
                let emitted = arguments.first().copied().and_then(|delay| {
                    emit_timed_suspend(
                        context,
                        module,
                        &builder,
                        design,
                        process.id,
                        *resume,
                        delay,
                        index_sites,
                        &mut cache,
                    )
                });
                builder
                    .build_return(Some(&i8.const_int(
                        u64::from(if emitted.is_some() {
                            PROCESS_SUSPENDED
                        } else {
                            PROCESS_UNSUPPORTED
                        }),
                        false,
                    )))
                    .unwrap();
            }
            ProcessTerminator::Suspend {
                operation: siox::ir::ProcessSuspendOp::AwaitCondition,
                arguments,
                resume,
                ..
            } => {
                let emitted = arguments
                    .is_empty()
                    .then(|| emit_condition_suspend(context, module, &builder, process.id, *resume))
                    .flatten();
                builder
                    .build_return(Some(&i8.const_int(
                        u64::from(if emitted.is_some() {
                            PROCESS_SUSPENDED
                        } else {
                            PROCESS_UNSUPPORTED
                        }),
                        false,
                    )))
                    .unwrap();
            }
            ProcessTerminator::Suspend {
                operation: siox::ir::ProcessSuspendOp::Settle,
                arguments,
                resume,
                ..
            } => {
                let emitted = arguments
                    .is_empty()
                    .then(|| emit_settle_suspend(context, module, &builder, process.id, *resume))
                    .flatten();
                builder
                    .build_return(Some(&i8.const_int(
                        u64::from(if emitted.is_some() {
                            PROCESS_SETTLING
                        } else {
                            PROCESS_UNSUPPORTED
                        }),
                        false,
                    )))
                    .unwrap();
            }
            ProcessTerminator::For {
                local,
                iterable,
                body,
                exit,
                ..
            } => {
                let emitted = if range_loop_bounds(design, *iterable).is_some() {
                    emit_range_loop(
                        context,
                        module,
                        &builder,
                        function,
                        design,
                        process.id,
                        block.id,
                        *local,
                        *iterable,
                        blocks[body.0 as usize],
                        blocks[exit.0 as usize],
                        index_sites,
                        &mut cache,
                    )
                } else if dynamic_string_value(design, *iterable) {
                    emit_dynamic_string_loop(
                        context,
                        module,
                        &builder,
                        function,
                        design,
                        process.id,
                        block.id,
                        *local,
                        *iterable,
                        blocks[body.0 as usize],
                        blocks[exit.0 as usize],
                        index_sites,
                        &mut cache,
                    )
                } else {
                    emit_array_loop(
                        context,
                        module,
                        &builder,
                        function,
                        design,
                        process.id,
                        block.id,
                        *local,
                        *iterable,
                        blocks[body.0 as usize],
                        blocks[exit.0 as usize],
                        index_sites,
                        &mut cache,
                    )
                };
                if emitted.is_none() {
                    builder
                        .build_return(Some(&i8.const_int(u64::from(PROCESS_UNSUPPORTED), false)))
                        .unwrap();
                }
            }
            ProcessTerminator::Match {
                scrutinee,
                arms,
                fallback,
            } => {
                if emit_process_match(
                    context,
                    &builder,
                    function,
                    design,
                    *scrutinee,
                    match_scrutinee.expect("supported match scrutinee was emitted"),
                    arms,
                    *fallback,
                    &blocks,
                    invalid,
                )
                .is_none()
                {
                    builder
                        .build_return(Some(&i8.const_int(u64::from(PROCESS_UNSUPPORTED), false)))
                        .unwrap();
                }
            }
            ProcessTerminator::Return { value: Some(_), .. } => {
                builder
                    .build_return(Some(&i8.const_int(u64::from(PROCESS_UNSUPPORTED), false)))
                    .unwrap();
            }
        }
    }
    function
}

pub(super) fn scheduled_value_from_words<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    words: PointerValue<'ctx>,
    width: u32,
) -> Option<IntValue<'ctx>> {
    let i32 = context.i32_type();
    let i64 = context.i64_type();
    let ty = context
        .custom_width_int_type(std::num::NonZeroU32::new(width)?)
        .ok()?;
    let mut value = ty.const_zero();
    for word in 0..super::super::words_for(width) {
        let source = unsafe {
            builder
                .build_in_bounds_gep(
                    i64,
                    words,
                    &[i32.const_int(u64::from(word), false)],
                    "process.scheduled.word.pointer",
                )
                .ok()?
        };
        let part = builder
            .build_load(i64, source, "process.scheduled.word")
            .ok()?
            .into_int_value();
        let part = fit(builder, part, width)?;
        let offset = word.checked_mul(super::super::ABI_WORD_BITS)?;
        let part = if offset == 0 {
            part
        } else {
            builder
                .build_left_shift(
                    part,
                    ty.const_int(u64::from(offset), false),
                    "process.scheduled.word.place",
                )
                .ok()?
        };
        value = builder
            .build_or(value, part, "process.scheduled.value")
            .ok()?;
    }
    Some(value)
}

/// Apply one expired delayed-write site. The fixed runtime owns time and
/// copies ABI words; this emitted dispatcher owns the target's concrete LLVM
/// layout and stages the write into the ordinary commit boundary.
pub(super) fn emit_scheduled_apply<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    design: &Design,
    sites: &[ScheduleSite],
    range_sites: &HashMap<siox::diag::Span, u32>,
) {
    let i8 = context.i8_type();
    let i32 = context.i32_type();
    let pointer = context.ptr_type(AddressSpace::default());
    let function = module.add_function(
        "sx_process_apply_scheduled",
        i8.fn_type(
            &[i32.into(), pointer.into(), pointer.into(), i32.into()],
            false,
        ),
        None,
    );
    let builder = context.create_builder();
    let dispatch = context.append_basic_block(function, "dispatch");
    let invalid = context.append_basic_block(function, "invalid");
    let blocks = sites
        .iter()
        .map(|site| context.append_basic_block(function, &format!("site{}", site.id)))
        .collect::<Vec<_>>();
    let applies = sites
        .iter()
        .map(|site| context.append_basic_block(function, &format!("site{}.apply", site.id)))
        .collect::<Vec<_>>();

    builder.position_at_end(dispatch);
    let site_id = function
        .get_nth_param(0)
        .expect("scheduled callback has a site id")
        .into_int_value();
    let words = function
        .get_nth_param(1)
        .expect("scheduled callback has value words")
        .into_pointer_value();
    let masks = function
        .get_nth_param(2)
        .expect("scheduled callback has mask words")
        .into_pointer_value();
    let count = function
        .get_nth_param(3)
        .expect("scheduled callback has a word count")
        .into_int_value();
    let cases = sites
        .iter()
        .zip(&blocks)
        .map(|(site, block)| (i32.const_int(u64::from(site.id), false), *block))
        .collect::<Vec<_>>();
    builder.build_switch(site_id, invalid, &cases).unwrap();

    builder.position_at_end(invalid);
    builder
        .build_return(Some(&i8.const_int(u64::from(PROCESS_UNSUPPORTED), false)))
        .unwrap();

    for ((site, block), apply) in sites.iter().zip(&blocks).zip(&applies) {
        builder.position_at_end(*block);
        let expected = i32.const_int(u64::from(super::super::words_for(site.width)), false);
        let valid = builder
            .build_int_compare(
                IntPredicate::EQ,
                count,
                expected,
                "process.scheduled.word_count",
            )
            .unwrap();
        builder
            .build_conditional_branch(valid, *apply, invalid)
            .unwrap();

        builder.position_at_end(*apply);
        let emitted = scheduled_value_from_words(context, &builder, words, site.width)
            .zip(scheduled_value_from_words(
                context, &builder, masks, site.width,
            ))
            .and_then(|(value, mask)| {
                write_static_place_masked(
                    context,
                    module,
                    &builder,
                    design,
                    site.place,
                    value,
                    mask,
                    site.span,
                    range_sites,
                )
            });
        builder
            .build_return(Some(&i8.const_int(
                u64::from(if emitted.is_some() {
                    PROCESS_COMPLETED
                } else {
                    PROCESS_UNSUPPORTED
                }),
                false,
            )))
            .unwrap();
    }
}

/// Emit the opaque function-pointer table consumed by the native scheduler.
pub(super) fn process_entry_table<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    design: &Design,
    supported_values: &ProcessValueSupport,
    checked_values: &[bool],
) {
    let pointer = context.ptr_type(AddressSpace::default());
    let index_sites = design
        .index_sites()
        .into_iter()
        .enumerate()
        .map(|(index, site)| (site, index as u32 + 1))
        .collect::<HashMap<_, _>>();
    let range_sites = design
        .range_sites()
        .into_iter()
        .enumerate()
        .map(|(index, span)| (span, index as u32 + 1))
        .collect::<HashMap<_, _>>();
    let schedule_sites = schedule_sites(design);
    let schedule_by_location = schedule_sites
        .iter()
        .map(|site| ((site.process, site.block, site.instruction), site.clone()))
        .collect::<HashMap<_, _>>();
    emit_scheduled_apply(context, module, design, &schedule_sites, &range_sites);
    let values = design
        .process_ir
        .processes
        .iter()
        .map(|process| {
            process_entry(
                context,
                module,
                design,
                process,
                &index_sites,
                &range_sites,
                checked_values,
                supported_values,
                &schedule_by_location,
            )
            .as_global_value()
            .as_pointer_value()
        })
        .collect::<Vec<_>>();
    let fallback = [pointer.const_null()];
    let initializer = pointer.const_array(if values.is_empty() {
        &fallback
    } else {
        &values
    });
    let global = module.add_global(initializer.get_type(), None, "sx_process_entries");
    global.set_initializer(&initializer);
    global.set_constant(true);
}
