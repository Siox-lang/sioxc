//! Block support checks and suspension points.

use super::*;

/// Whether a block is transactional for the currently implemented direct
/// subset. Unsupported blocks execute no foreign calls and publish no staged
/// writes before returning status 255.
pub(super) fn process_pattern_supported(pattern: &ProcessPattern) -> bool {
    match pattern {
        ProcessPattern::Wildcard
        | ProcessPattern::Number(_)
        | ProcessPattern::Range { .. }
        | ProcessPattern::Char(_) => true,
        ProcessPattern::BitMask { mask, value } => !mask.is_empty() && mask.len() == value.len(),
        ProcessPattern::Or(alternatives) => {
            !alternatives.is_empty() && alternatives.iter().all(process_pattern_supported)
        }
        ProcessPattern::Path { .. } | ProcessPattern::BitPattern(_) => false,
    }
}

pub(super) fn block_is_supported(
    design: &Design,
    process: &ProcessCfg,
    block: &siox::ir::ProcessBlock,
    values: &ProcessValueSupport,
) -> bool {
    let value = |id: ProcessValueId| values.get(id.0 as usize).copied().unwrap_or(false);
    let assignment_supported =
        |target, assigned| process_value_supported_for_target(design, target, assigned, values);
    let instructions = block.instructions.iter().all(|instruction| {
        let supported = match instruction {
            ProcessInstruction::Declare {
                local, initializer, ..
            } => {
                local_width(design, process.id, *local).is_some()
                    && initializer.is_none_or(|initializer| {
                        process
                            .locals
                            .get(local.0 as usize)
                            .and_then(|local| local.layout.as_ref())
                            .map_or_else(
                                || value(initializer),
                                |layout| {
                                    process_value_supported_in_layout(
                                        design,
                                        initializer,
                                        layout,
                                        values,
                                    ) && (packed_logic_layout(design, layout).is_none()
                                        || process_packed_meta_supported(
                                            design,
                                            initializer,
                                            layout,
                                            values,
                                        ))
                                },
                            )
                    })
            }
            ProcessInstruction::Assign {
                semantics: ProcessAssignment::ImmediateLocal,
                target,
                value: assigned,
                ..
            } => {
                assignment_place_supported(
                    design,
                    *target,
                    process.id,
                    ProcessAssignment::ImmediateLocal,
                    values,
                ) && assignment_supported(*target, *assigned)
            }
            ProcessInstruction::Assign {
                semantics: ProcessAssignment::ImmediateStorage,
                target,
                value: assigned,
                ..
            } => {
                assignment_place_supported(
                    design,
                    *target,
                    process.id,
                    ProcessAssignment::ImmediateStorage,
                    values,
                ) && assignment_supported(*target, *assigned)
            }
            ProcessInstruction::Assign {
                semantics: ProcessAssignment::StagedSignal,
                target,
                value: assigned,
                ..
            } => {
                (staged_signal_group(design, *target).is_some()
                    || assignment_place_supported(
                        design,
                        *target,
                        process.id,
                        ProcessAssignment::StagedSignal,
                        values,
                    ))
                    && assignment_supported(*target, *assigned)
            }
            ProcessInstruction::Assign {
                semantics: ProcessAssignment::PerPlace,
                target,
                value: assigned,
                ..
            } => {
                supported_per_place_assignment(design, process.id, *target, *assigned)
                    && value(*assigned)
            }
            ProcessInstruction::Schedule {
                target,
                value: assigned,
                delay,
                ..
            } => {
                delayed_place(design, *target).is_some()
                    && assignment_supported(*target, *assigned)
                    && value(*delay)
                    && design
                        .process_ir
                        .values
                        .get(delay.0 as usize)
                        .and_then(|value| value.bit_width)
                        .is_some_and(|width| width <= 64)
            }
            ProcessInstruction::Runtime {
                operation,
                arguments,
                format,
                ..
            } => runtime_instruction_supported(design, operation, arguments, format, values),
        };
        supported
    });
    let terminator = match &block.terminator {
        ProcessTerminator::Return { value: None, .. }
        | ProcessTerminator::Goto(_)
        | ProcessTerminator::Stop { .. }
        | ProcessTerminator::Finish { .. } => true,
        ProcessTerminator::Branch { condition, .. } => value(*condition),
        ProcessTerminator::Suspend {
            operation: siox::ir::ProcessSuspendOp::AwaitTime,
            arguments,
            ..
        } => {
            let [delay] = arguments.as_slice() else {
                return false;
            };
            value(*delay)
                && design
                    .process_ir
                    .values
                    .get(delay.0 as usize)
                    .is_some_and(|delay| {
                        delay.bit_width.is_some_and(|width| width <= 64)
                            && matches!(
                                delay.kind,
                                ProcessValueKind::Number(ProcessNumber::Integer(_))
                            )
                    })
        }
        ProcessTerminator::Suspend {
            operation: siox::ir::ProcessSuspendOp::AwaitCondition,
            arguments,
            ..
        } => arguments.is_empty(),
        ProcessTerminator::Suspend {
            operation: siox::ir::ProcessSuspendOp::Settle,
            arguments,
            ..
        } => arguments.is_empty(),
        ProcessTerminator::For {
            local, iterable, ..
        } => {
            let range = range_loop_bounds(design, *iterable).is_some_and(|(left, right)| {
                local_width(design, process.id, *local) == Some(64)
                    && value(left)
                    && value(right)
                    && [left, right].into_iter().all(|bound| {
                        design
                            .process_ir
                            .values
                            .get(bound.0 as usize)
                            .and_then(|value| value.bit_width)
                            .is_some_and(|width| width <= 64)
                    })
            });
            let array = array_loop_shape(design, *iterable).is_some_and(|(layout, element, _)| {
                layout_width(element) == local_width(design, process.id, *local)
                    && layout_width(layout)
                        .is_some_and(|width| width <= super::super::emit::LLVM_MAX_INT_BITS)
                    && process_value_supported_in_layout(design, *iterable, layout, values)
            });
            let dynamic_string = dynamic_string_value(design, *iterable)
                && local_width(design, process.id, *local) == Some(32)
                && value(*iterable);
            range || array || dynamic_string
        }
        ProcessTerminator::Match {
            scrutinee, arms, ..
        } => {
            value(*scrutinee)
                && design
                    .process_ir
                    .values
                    .get(scrutinee.0 as usize)
                    .and_then(|value| value.bit_width)
                    .is_some_and(|width| {
                        width > 0 && width <= super::super::emit::LLVM_MAX_INT_BITS
                    })
                && !arms.is_empty()
                && arms
                    .iter()
                    .all(|arm| process_pattern_supported(&arm.pattern))
        }
        ProcessTerminator::Return { value: Some(_), .. } => false,
    };
    instructions && terminator
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_timed_suspend<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    process: ProcessId,
    resume: siox::ir::ProcessBlockId,
    delay: ProcessValueId,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<()> {
    let delay = process_value_at(
        context,
        module,
        builder,
        design,
        delay,
        64,
        false,
        None,
        index_sites,
        cache,
    )?;
    let i32 = context.i32_type();
    let i64 = context.i64_type();
    let function = module
        .get_function("sx_runtime_suspend_time")
        .unwrap_or_else(|| {
            module.add_function(
                "sx_runtime_suspend_time",
                context
                    .void_type()
                    .fn_type(&[i32.into(), i32.into(), i64.into()], false),
                Some(Linkage::External),
            )
        });
    builder
        .build_call(
            function,
            &[
                i32.const_int(u64::from(process.0), false).into(),
                i32.const_int(u64::from(resume.0), false).into(),
                delay.into(),
            ],
            "",
        )
        .ok()?;
    Some(())
}

pub(super) fn emit_condition_suspend<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    process: ProcessId,
    resume: siox::ir::ProcessBlockId,
) -> Option<()> {
    let i32 = context.i32_type();
    let suspend = module
        .get_function("sx_runtime_suspend_condition")
        .unwrap_or_else(|| {
            module.add_function(
                "sx_runtime_suspend_condition",
                context
                    .void_type()
                    .fn_type(&[i32.into(), i32.into()], false),
                Some(Linkage::External),
            )
        });
    builder
        .build_call(
            suspend,
            &[
                i32.const_int(u64::from(process.0), false).into(),
                i32.const_int(u64::from(resume.0), false).into(),
            ],
            "",
        )
        .ok()?;
    Some(())
}

pub(super) fn emit_settle_suspend<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    process: ProcessId,
    resume: siox::ir::ProcessBlockId,
) -> Option<()> {
    let i32 = context.i32_type();
    let function = module.get_function("sx_runtime_settle").unwrap_or_else(|| {
        module.add_function(
            "sx_runtime_settle",
            context
                .void_type()
                .fn_type(&[i32.into(), i32.into()], false),
            Some(Linkage::External),
        )
    });
    builder
        .build_call(
            function,
            &[
                i32.const_int(u64::from(process.0), false).into(),
                i32.const_int(u64::from(resume.0), false).into(),
            ],
            "",
        )
        .ok()?;
    Some(())
}
