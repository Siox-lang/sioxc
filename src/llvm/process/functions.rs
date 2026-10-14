//! Shared value functions: one internal LLVM function per Process IR
//! function, called wherever a recorded call can replace its inlined copy.

use super::*;
use inkwell::types::BasicMetadataTypeEnum;

/// Whether `call` may be emitted as a call of its shared function. Its
/// value must have no metavalues, and an argument may carry metavalues only
/// when no node of the body reads them to compute its value: then the body
/// computes the same value from the value planes alone.
pub(super) fn shared_call_eligible(
    design: &Design,
    call: &siox::ir::ProcessCall,
    meta_free: &[bool],
) -> bool {
    let free = |id: ProcessValueId| meta_free.get(id.0 as usize).copied().unwrap_or(false);
    let Some(shared) = design.process_ir.functions.get(call.function.0 as usize) else {
        return false;
    };
    free(call.value)
        && free(shared.result)
        && (call.arguments.iter().all(|argument| free(*argument))
            || body_ignores_parameter_metadata(design, shared))
}

/// Whether no value in `shared`'s body is computed from a parameter's
/// metavalue plane. A value carries its parameters' planes through ordinary
/// operations; converting to a kernel integer or real leaves the logic
/// domain and drops them.
fn body_ignores_parameter_metadata(design: &Design, shared: &siox::ir::ProcessFunction) -> bool {
    let values = &design.process_ir.values;
    let first = shared
        .parameters
        .iter()
        .map(|parameter| parameter.0)
        .min()
        .unwrap_or(shared.result.0) as usize;
    let last = shared.result.0 as usize;
    let mut carries = vec![false; last + 1 - first];
    for index in first..=last {
        let value = &values[index];
        let mut from_parameter = false;
        siox::ir::for_each_process_value_dependency(&value.kind, |dependency| {
            let dependency = dependency.0 as usize;
            from_parameter |= dependency >= first && carries[dependency - first];
        });
        let reads_metadata = matches!(
            value.kind,
            ProcessValueKind::MetaCompare { .. }
                | ProcessValueKind::TableLookup { .. }
                | ProcessValueKind::Index { .. }
                | ProcessValueKind::BitSlice { .. }
                | ProcessValueKind::PackedSlice { .. }
                | ProcessValueKind::Field { .. }
                | ProcessValueKind::Match { .. }
        );
        if reads_metadata && from_parameter {
            return false;
        }
        let leaves_logic = matches!(
            value.ty,
            Some(siox::types::Ty::Integer | siox::types::Ty::Real)
        );
        carries[index - first] = match value.kind {
            ProcessValueKind::Parameter { .. } => true,
            _ => from_parameter && !leaves_logic,
        };
    }
    true
}

/// Emit the value of `call` as a call of its shared function, or `None` when
/// its inlined copy must be emitted instead.
#[allow(clippy::too_many_arguments)]
pub(super) fn shared_call<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    call: &siox::ir::ProcessCall,
    active: Option<IntValue<'ctx>>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    if !shared_call_eligible(design, call, cache.meta_free) {
        return None;
    }
    let shared = design.process_ir.functions.get(call.function.0 as usize)?;
    if cache.debug.is_some() {
        if let Some(active) = active {
            let width = design
                .process_ir
                .values
                .get(call.value.0 as usize)?
                .bit_width?;
            return debug::active_value(
                context,
                builder,
                width,
                active,
                cache,
                |builder, cache| {
                    shared_call(
                        context,
                        module,
                        builder,
                        design,
                        call,
                        None,
                        index_sites,
                        cache,
                    )
                },
            );
        }
    }
    let width = design
        .process_ir
        .values
        .get(call.value.0 as usize)?
        .bit_width?;
    debug::call_value(
        context,
        builder,
        (call.value, width, false, None),
        cache,
        |builder, cache| {
            let function = shared_function(context, module, design, shared, index_sites, cache)?;
            // Each argument at its parameter's width, extended as its own type is.
            let mut arguments = Vec::with_capacity(call.arguments.len());
            for (argument, parameter) in call.arguments.iter().zip(&shared.parameters) {
                let width = design
                    .process_ir
                    .values
                    .get(parameter.0 as usize)?
                    .bit_width?;
                let value = process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    *argument,
                    width,
                    process_value_is_signed(design, *argument),
                    active,
                    index_sites,
                    cache,
                )?;
                arguments.push(value.into());
            }
            let returned = debug::call_return(
                context,
                builder,
                function,
                &arguments,
                "pv.shared",
                cache.debug,
            )?;
            // A recursion too deep for the stack set the test's error; the
            // calling process stops on it. Inside a shared body there is no
            // failure block, and the check is a no-op.
            if recursive_function(design, call.function) {
                let span = design.process_ir.values.get(call.value.0 as usize)?.span;
                emit_host_read_check(context, module, builder, span)?;
            }
            Some(returned)
        },
    )
}

/// Whether `function` calls itself: a recursive call of it is registered as
/// a call whose value is still a `Call` node.
pub(super) fn recursive_function(design: &Design, function: siox::ir::ProcessFunctionId) -> bool {
    design.process_ir.calls.iter().any(|call| {
        call.function == function
            && matches!(
                design
                    .process_ir
                    .values
                    .get(call.value.0 as usize)
                    .map(|value| &value.kind),
                Some(ProcessValueKind::Call { .. })
            )
    })
}

/// The LLVM function for `shared`, emitted on first use: its parameters bind
/// the body's `Parameter` values and it returns the body's result.
fn shared_function<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    design: &Design,
    shared: &siox::ir::ProcessFunction,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &ProcessValueCache<'ctx, '_>,
) -> Option<FunctionValue<'ctx>> {
    let name = format!("sx.fn.{}.{}", shared.id.0, shared.name);
    if let Some(function) = module.get_function(&name) {
        return Some(function);
    }
    let int = |value: ProcessValueId| {
        let width = design.process_ir.values.get(value.0 as usize)?.bit_width?;
        context
            .custom_width_int_type(std::num::NonZeroU32::new(width)?)
            .ok()
    };
    let parameters = shared
        .parameters
        .iter()
        .map(|parameter| int(*parameter).map(BasicMetadataTypeEnum::from))
        .collect::<Option<Vec<_>>>()?;
    let result = if let (Some(debug), ProcessValueKind::InlineCall { operand, source }) = (
        cache.debug,
        &design.process_ir.values[shared.result.0 as usize].kind,
    ) {
        debug::source_return_type(
            debug,
            module,
            design,
            source,
            *operand,
            int(shared.result)?.get_bit_width(),
            cache.debug_root,
        )?
    } else {
        debug::SourceReturn::Direct(int(shared.result)?.into())
    };
    let function = module.add_function(
        &name,
        result.fn_type(context, &parameters),
        Some(Linkage::Internal),
    );
    result.declare(context, function, cache.debug);
    let builder = context.create_builder();
    builder.position_at_end(context.append_basic_block(function, "entry"));
    let mut body = ProcessValueCache::new(cache.activity_sensitive, cache.meta_free);
    body.debug = cache.debug;
    body.debug_root = cache.debug_root;
    for (index, parameter) in shared.parameters.iter().enumerate() {
        let argument = function
            .get_nth_param(index as u32 + result.parameter_offset())?
            .into_int_value();
        body.parameters.insert(*parameter, argument);
    }
    // A recursive body first asks the runtime for stack room: recursion too
    // deep for the native stack returns at once with the test's error set,
    // which the calling process reports, instead of crashing the executable.
    if recursive_function(design, shared.id) {
        let name = private_string(
            context,
            module,
            &format!("sx.fn.name.{}", shared.id.0),
            &shared.name,
        );
        let room = module
            .get_function("sx_runtime_frame_room")
            .unwrap_or_else(|| {
                module.add_function(
                    "sx_runtime_frame_room",
                    context
                        .i8_type()
                        .fn_type(&[context.ptr_type(AddressSpace::default()).into()], false),
                    Some(Linkage::External),
                )
            });
        let inkwell::values::ValueKind::Basic(room) = builder
            .build_call(room, &[name.into()], "pv.frame.room")
            .ok()?
            .try_as_basic_value()
        else {
            return None;
        };
        let room = builder
            .build_int_compare(
                IntPredicate::NE,
                room.into_int_value(),
                context.i8_type().const_zero(),
                "pv.frame.ok",
            )
            .ok()?;
        let enter = context.append_basic_block(function, "frame.enter");
        let refuse = context.append_basic_block(function, "frame.refuse");
        builder.build_conditional_branch(room, enter, refuse).ok()?;
        builder.position_at_end(refuse);
        result.emit_return(body.debug, &builder, int(shared.result)?.const_zero())?;
        builder.position_at_end(enter);
    }
    let returned = if body.debug.is_some() {
        if let ProcessValueKind::InlineCall { operand, source } =
            &design.process_ir.values[shared.result.0 as usize].kind
        {
            debug::inline_body(
                module,
                &builder,
                design,
                Some(function),
                *operand,
                source,
                &mut body,
                |builder, cache, active| {
                    process_value(
                        context,
                        module,
                        builder,
                        design,
                        *operand,
                        active,
                        index_sites,
                        cache,
                    )
                },
            )
        } else {
            // Hand-built IR may have no source-call descriptor. Do not invent
            // formal names or source statements for a generated native body.
            body.source_location = body.debug?.generated_location(&builder, shared.span);
            process_value(
                context,
                module,
                &builder,
                design,
                shared.result,
                None,
                index_sites,
                &mut body,
            )
        }
    } else {
        process_value(
            context,
            module,
            &builder,
            design,
            shared.result,
            None,
            index_sites,
            &mut body,
        )
    }
    .and_then(|value| result.emit_return(body.debug, &builder, value));
    if returned.is_none() {
        // SAFETY: the function was created above and nothing calls it yet.
        unsafe { function.delete() };
        return None;
    }
    Some(function)
}
