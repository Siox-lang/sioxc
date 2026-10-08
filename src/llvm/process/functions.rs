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
    match builder
        .build_call(function, &arguments, "pv.shared")
        .ok()?
        .try_as_basic_value()
    {
        inkwell::values::ValueKind::Basic(value) => Some(value.into_int_value()),
        _ => None,
    }
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
    let function = module.add_function(
        &name,
        int(shared.result)?.fn_type(&parameters, false),
        Some(Linkage::Internal),
    );
    let builder = context.create_builder();
    builder.position_at_end(context.append_basic_block(function, "entry"));
    let mut body = ProcessValueCache::new(cache.activity_sensitive, cache.meta_free);
    for (index, parameter) in shared.parameters.iter().enumerate() {
        let argument = function.get_nth_param(index as u32)?.into_int_value();
        body.parameters.insert(*parameter, argument);
    }
    let returned = process_value(
        context,
        module,
        &builder,
        design,
        shared.result,
        None,
        index_sites,
        &mut body,
    )
    .and_then(|result| builder.build_return(Some(&result)).ok());
    if returned.is_none() {
        // SAFETY: the function was created above and nothing calls it yet.
        unsafe { function.delete() };
        return None;
    }
    Some(function)
}
