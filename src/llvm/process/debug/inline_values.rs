//! Native source frames over the same canonical expressions as ordinary builds.

use super::*;
use inkwell::basic_block::BasicBlock;
use inkwell::debug_info::DILocalVariable;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::BasicValueEnum;
use siox::ir::ProcessValueInline;

/// Use the source real ABI only at the native boundary. The common emitter
/// and its exact consumer formats continue to exchange the same integer bits.
pub(in crate::llvm::process) fn source_return_type<'ctx>(
    debug: &DebugInfo<'ctx, '_>,
    design: &Design,
    source: &ProcessValueInline,
    operand: ProcessValueId,
    width: u32,
) -> Option<BasicTypeEnum<'ctx>> {
    let layout = source
        .result_layout
        .as_ref()
        .map(std::borrow::Cow::Borrowed)
        .or_else(|| debug.value_debug_layout(design, operand));
    if width == 64
        && layout.is_some_and(|layout| {
            matches!(
                layout.kind,
                LayoutKind::Scalar {
                    domain: ScalarDomain::Real,
                    ..
                }
            )
        })
    {
        Some(debug.context.f64_type().into())
    } else {
        debug
            .context
            .custom_width_int_type(std::num::NonZeroU32::new(width)?)
            .ok()
            .map(Into::into)
    }
}

pub(in crate::llvm::process) fn return_value<'ctx>(
    builder: &Builder<'ctx>,
    value: IntValue<'ctx>,
) -> Option<BasicValueEnum<'ctx>> {
    let ty = builder
        .get_insert_block()?
        .get_parent()?
        .get_type()
        .get_return_type()?;
    if ty.is_float_type() {
        builder.build_bit_cast(value, ty, "debug.return.real").ok()
    } else {
        Some(value.into())
    }
}

pub(in crate::llvm::process) fn return_bits<'ctx>(
    builder: &Builder<'ctx>,
    value: BasicValueEnum<'ctx>,
) -> Option<IntValue<'ctx>> {
    match value {
        BasicValueEnum::FloatValue(value) => builder
            .build_bit_cast(
                value,
                value.get_type().get_context().i64_type(),
                "debug.return.bits",
            )
            .ok()
            .map(|value| value.into_int_value()),
        BasicValueEnum::IntValue(value) => Some(value),
        _ => None,
    }
}

pub(in crate::llvm::process) struct ValueFrame<'ctx> {
    function: Span,
    scope: DISubprogram<'ctx>,
    caller: Option<DILocation<'ctx>>,
    bindings: Vec<ValueBinding<'ctx>>,
    branches: Vec<(ProcessValueId, [Span; 2])>,
}

struct ValueBinding<'ctx> {
    id: ProcessValueId,
    variable: DILocalVariable<'ctx>,
    location: DILocation<'ctx>,
    reference: PointerValue<'ctx>,
    buffer: GlobalValue<'ctx>,
    inspection: ValueInspection<'ctx>,
    signed: bool,
    parameter: bool,
    captured: Cell<Option<(IntValue<'ctx>, BasicBlock<'ctx>)>>,
}

enum ValueInspection<'ctx> {
    Packed(SourceLayout),
    String(u32, FunctionValue<'ctx>),
}

/// Discover the native ABI from actual consumer requests, not inspection types.
/// Temporary entry phis are replaced by formal arguments before verification;
/// they never execute and introduce no argument buffers or simulation copies.
pub(in crate::llvm::process) struct ValueArguments<'ctx> {
    ids: std::collections::HashSet<ProcessValueId>,
    values: Vec<(ValueFunctionKey, IntValue<'ctx>)>,
}

pub(in crate::llvm::process) fn argument_value<'ctx>(
    builder: &Builder<'ctx>,
    design: &Design,
    cache: &mut ProcessValueCache<'ctx, '_>,
    mut key: ValueFunctionKey,
) -> Option<IntValue<'ctx>> {
    let arguments = cache.debug_arguments.as_mut()?;
    if !arguments.ids.contains(&key.0) {
        return None;
    }
    let (id, width, signed) = (key.0, key.1, key.2);
    if key.3.is_none() {
        if let Some(width) = argument_boundary_width(design, id) {
            key.1 = width;
            key.2 = false;
        }
    }
    let existing = arguments
        .values
        .iter()
        .find(|(format, _)| *format == key)
        .map(|(_, value)| *value);
    let context = cache.debug?.context;
    let value = if let Some(value) = existing {
        value
    } else {
        let entry = builder
            .get_insert_block()?
            .get_parent()?
            .get_first_basic_block()?;
        let argument_builder = context.create_builder();
        if let Some(first) = entry.get_first_instruction() {
            argument_builder.position_before(&first);
        } else {
            argument_builder.position_at_end(entry);
        }
        let ty = context
            .custom_width_int_type(std::num::NonZeroU32::new(key.1)?)
            .ok()?;
        let value = argument_builder
            .build_phi(ty, "debug.argument.format")
            .ok()?
            .as_basic_value()
            .into_int_value();
        arguments.values.push((key, value));
        value
    };
    if signed && process_value_is_signed(design, id) {
        fit_signed(builder, value, width)
    } else {
        fit(builder, value, width)
    }
}

fn argument_boundary_width(design: &Design, id: ProcessValueId) -> Option<u32> {
    let mut boundary = id;
    while let ProcessValueKind::InlineCall { operand, .. } =
        &design.process_ir.values.get(boundary.0 as usize)?.kind
    {
        boundary = *operand;
    }
    let node = design.process_ir.values.get(boundary.0 as usize)?;
    // These operands already have an executable width boundary. The ordinary
    // emitter reads them once at that width and only fits the resulting bits;
    // passing a new ABI argument for every consumer needlessly re-expands the
    // caller. Contextual arithmetic and expected-layout conversions stay exact.
    if node.bit_width == design.process_ir.values[id.0 as usize].bit_width
        && process_value_is_signed(design, boundary) == process_value_is_signed(design, id)
        && matches!(
            node.kind,
            ProcessValueKind::Number(_)
                | ProcessValueKind::BitString { .. }
                | ProcessValueKind::Char(_)
                | ProcessValueKind::Signal { .. }
                | ProcessValueKind::Local { .. }
                | ProcessValueKind::Storage(_)
                | ProcessValueKind::Parameter { .. }
                | ProcessValueKind::RawResize { .. }
                | ProcessValueKind::BitSlice { .. }
                | ProcessValueKind::PackedSlice { .. }
        )
    {
        node.bit_width
    } else {
        None
    }
}

impl DebugInfo<'_, '_> {
    pub(in crate::llvm::process) fn value_requires_activity(&self, id: ProcessValueId) -> bool {
        self.value_activity
            .get(id.0 as usize)
            .copied()
            .unwrap_or(false)
    }
}

/// Child emission restores its parent's location before the parent operation.
pub(in crate::llvm::process) struct ValueLocationGuard<'a, 'ctx> {
    builder: &'a Builder<'ctx>,
    previous: Option<inkwell::llvm_sys::prelude::LLVMMetadataRef>,
}

impl<'a, 'ctx> ValueLocationGuard<'a, 'ctx> {
    pub(in crate::llvm::process) fn new(
        builder: &'a Builder<'ctx>,
        cache: &ProcessValueCache<'ctx, '_>,
        span: Span,
    ) -> Self {
        // Inkwell 0.9 uses the deprecated LLVMValueRef getter; a missing
        // location can become an empty MDTuple. Preserve the metadata directly.
        let previous = cache.debug.map(|_| unsafe {
            inkwell::llvm_sys::core::LLVMGetCurrentDebugLocation2(builder.as_mut_ptr())
        });
        if let Some(location) = value_location(cache, span) {
            builder.set_current_debug_location(location);
        }
        Self { builder, previous }
    }
}

impl Drop for ValueLocationGuard<'_, '_> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous {
            unsafe {
                inkwell::llvm_sys::core::LLVMSetCurrentDebugLocation2(
                    self.builder.as_mut_ptr(),
                    previous,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::llvm::process) fn inline_value<'ctx>(
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    id: ProcessValueId,
    operand: ProcessValueId,
    source: &ProcessValueInline,
    width: u32,
    signed: bool,
    layout: Option<&SourceLayout>,
    active: Option<IntValue<'ctx>>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
    emit: impl FnOnce(
        &Builder<'ctx>,
        &mut ProcessValueCache<'ctx, '_>,
        Option<IntValue<'ctx>>,
    ) -> Option<IntValue<'ctx>>,
) -> Option<IntValue<'ctx>> {
    let Some(debug) = cache.debug else {
        return emit(builder, cache, active);
    };
    if let Some(active) = active {
        return active_value(
            debug.context,
            builder,
            width,
            active,
            cache,
            |builder, cache| {
                inline_value(
                    module,
                    builder,
                    design,
                    id,
                    operand,
                    source,
                    width,
                    signed,
                    layout,
                    None,
                    index_sites,
                    cache,
                    emit,
                )
            },
        );
    }
    let key = (id, width, signed, layout.cloned());
    let existing = debug.value_functions.borrow().get(&key).cloned();
    let (function, formats) = if let Some(existing) = existing {
        existing
    } else {
        let ty = source_return_type(debug, design, source, operand, width)?;
        // Let the common emitter request its real operand formats once. A
        // source argument can be used at several widths inside this body;
        // capturing it at its minimum width first changes contextual arithmetic.
        let draft = module.add_function(
            &format!("sx.debug.call.{}", id.0),
            ty.fn_type(&[], false),
            Some(Linkage::Internal),
        );
        let body_builder = debug.context.create_builder();
        body_builder.position_at_end(debug.context.append_basic_block(draft, "entry"));
        let mut body_cache = ProcessValueCache::new(cache.activity_sensitive, cache.meta_free);
        body_cache.debug = Some(debug);
        body_cache.debug_root = cache.debug_root;
        body_cache.hardware_calls = cache.hardware_calls;
        body_cache.debug_arguments = Some(ValueArguments {
            ids: source
                .parameters
                .iter()
                .filter_map(|parameter| parameter.value)
                // Literal strings, including zero-storage strings, are still
                // compile-time operands rather than an unconstrained ABI.
                .filter(|value| process_string(design, *value).is_none())
                .collect(),
            values: Vec::new(),
        });
        let returned = inline_body(
            module,
            &body_builder,
            design,
            draft,
            operand,
            source,
            &mut body_cache,
            emit,
        )?;
        let returned = return_value(&body_builder, returned)?;
        body_builder.build_return(Some(&returned)).ok()?;
        let values = body_cache.debug_arguments.take()?.values;
        let types = values
            .iter()
            .map(|(_, value)| value.get_type().into())
            .collect::<Vec<_>>();
        let name = draft.get_name().to_string_lossy().into_owned();
        draft.as_global_value().set_name(&format!("{name}.draft"));
        let function =
            module.add_function(&name, ty.fn_type(&types, false), Some(Linkage::Internal));
        function.set_subprogram(draft.get_subprogram()?);
        for (index, (_, placeholder)) in values.iter().enumerate() {
            placeholder
                .replace_all_uses_with(function.get_nth_param(index as u32)?.into_int_value());
            placeholder.as_instruction()?.erase_from_basic_block();
        }
        for block in draft.get_basic_blocks() {
            block.remove_from_function().ok()?;
            function.append_existing_basic_block(block);
        }
        // No call or executable reference to the draft has been emitted.
        unsafe { draft.delete() };
        let formats = values
            .into_iter()
            .map(|(format, _)| format)
            .collect::<Vec<_>>();
        debug
            .value_functions
            .borrow_mut()
            .insert(key.clone(), (function, formats.clone()));
        (function, formats)
    };
    // Metadata never requests arguments. Emit only formats actually consumed
    // by the body, through exactly the same paths as an ordinary inline call.
    call_value(debug.context, builder, key, cache, |builder, cache| {
        let mut arguments = Vec::with_capacity(formats.len());
        for (value, width, signed, layout) in formats {
            let argument = if let Some(layout) = layout {
                process_value_in_layout(
                    debug.context,
                    module,
                    builder,
                    design,
                    value,
                    &layout,
                    None,
                    index_sites,
                    cache,
                )?
            } else {
                process_value_at(
                    debug.context,
                    module,
                    builder,
                    design,
                    value,
                    width,
                    signed,
                    None,
                    index_sites,
                    cache,
                )?
            };
            arguments.push(argument.into());
        }
        builder
            .build_call(function, &arguments, "debug.call")
            .ok()?
            .try_as_basic_value()
            .basic()
            .and_then(|value| return_bits(builder, value))
    })
}

/// A selected alias is evaluated once per actual consumer format. Readiness
/// travels with its value through SSA joins, never through runtime storage.
pub(in crate::llvm::process) fn call_value<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    key: ValueFunctionKey,
    cache: &mut ProcessValueCache<'ctx, '_>,
    emit: impl FnOnce(&Builder<'ctx>, &mut ProcessValueCache<'ctx, '_>) -> Option<IntValue<'ctx>>,
) -> Option<IntValue<'ctx>> {
    if !cache
        .debug
        .is_some_and(|debug| debug.reused_calls.contains(&key.0))
    {
        return emit(builder, cache);
    }
    let ready = context.bool_type().const_int(1, false);
    if let Some((value, available)) = cache.source_results.get(&key).copied() {
        if available.get_zero_extended_constant() == Some(1) {
            return Some(value);
        }
        let missing = builder.build_not(available, "debug.call.missing").ok()?;
        active_value(context, builder, key.1, missing, cache, |builder, cache| {
            let value = emit(builder, cache)?;
            cache.source_results.insert(key.clone(), (value, ready));
            Some(value)
        })?;
        let value = cache.source_results.get(&key)?.0;
        cache.source_results.insert(key, (value, ready));
        Some(value)
    } else {
        let value = emit(builder, cache)?;
        cache.source_results.insert(key, (value, ready));
        Some(value)
    }
}

/// Keep both a result and whether its call actually ran on each predecessor.
fn merge_results<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    arms: [(&ValueResults<'ctx>, BasicBlock<'ctx>); 2],
) -> Option<ValueResults<'ctx>> {
    let mut results = HashMap::new();
    for key in arms[0].0.keys().chain(arms[1].0.keys()) {
        if results.contains_key(key) {
            continue;
        }
        let (value, _) = arms[0].0.get(key).or_else(|| arms[1].0.get(key))?;
        let absent = (
            value.get_type().const_zero(),
            context.bool_type().const_zero(),
        );
        let left = arms[0].0.get(key).copied().unwrap_or(absent);
        let right = arms[1].0.get(key).copied().unwrap_or(absent);
        let merge = |left: IntValue<'ctx>, right: IntValue<'ctx>, name| {
            if left == right {
                Some(left)
            } else {
                let phi = builder.build_phi(left.get_type(), name).ok()?;
                phi.add_incoming(&[(&left, arms[0].1), (&right, arms[1].1)]);
                Some(phi.as_basic_value().into_int_value())
            }
        };
        results.insert(
            key.clone(),
            (
                merge(left.0, right.0, "debug.call.saved")?,
                merge(left.1, right.1, "debug.call.ready")?,
            ),
        );
    }
    Some(results)
}

/// Native source calls execute only on their selected source path. Both
/// shared Process functions and expression frames use this same guard.
pub(in crate::llvm::process) fn active_value<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    width: u32,
    active: IntValue<'ctx>,
    cache: &mut ProcessValueCache<'ctx, '_>,
    emit: impl FnOnce(&Builder<'ctx>, &mut ProcessValueCache<'ctx, '_>) -> Option<IntValue<'ctx>>,
) -> Option<IntValue<'ctx>> {
    match active.get_zero_extended_constant() {
        Some(0) => {
            return Some(
                context
                    .custom_width_int_type(std::num::NonZeroU32::new(width)?)
                    .ok()?
                    .const_zero(),
            )
        }
        Some(1) => return emit(builder, cache),
        _ => {}
    }
    let previous = builder.get_insert_block()?;
    let function = previous.get_parent()?;
    let execute = context.append_basic_block(function, "debug.call.execute");
    let join = context.append_basic_block(function, "debug.call.join");
    builder
        .build_conditional_branch(active, execute, join)
        .ok()?;
    let dominating = cache.dominating_values();
    let saved = cache.source_results.clone();
    builder.position_at_end(execute);
    let returned = emit(builder, cache)?;
    let exit = builder.get_insert_block()?;
    let executed = std::mem::take(&mut cache.source_results);
    builder.build_unconditional_branch(join).ok()?;
    builder.position_at_end(join);
    cache.retain_dominating(&dominating);
    cache.source_results =
        merge_results(context, builder, [(&saved, previous), (&executed, exit)])?;
    let result = builder
        .build_phi(returned.get_type(), "debug.call.result")
        .ok()?;
    result.add_incoming(&[
        (&returned.get_type().const_zero(), previous),
        (&returned, exit),
    ]);
    Some(result.as_basic_value().into_int_value())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::llvm::process) fn inline_body<'ctx>(
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    function: FunctionValue<'ctx>,
    operand: ProcessValueId,
    source: &ProcessValueInline,
    cache: &mut ProcessValueCache<'ctx, '_>,
    emit: impl FnOnce(
        &Builder<'ctx>,
        &mut ProcessValueCache<'ctx, '_>,
        Option<IntValue<'ctx>>,
    ) -> Option<IntValue<'ctx>>,
) -> Option<IntValue<'ctx>> {
    let debug = cache.debug?;
    let (owner, name) = source.name.rsplit_once("::").unwrap_or(("", &source.name));
    let file = debug.file(source.function.file);
    let line = debug
        .sources
        .line_col(source.function.file, source.function.start)
        .0;
    let binding_types = source
        .parameters
        .iter()
        .chain(&source.locals)
        .map(|binding| {
            let id = binding.value?;
            if dynamic_string_value(design, id) || process_string(design, id).is_some() {
                return debug.value_debug_type(design, id);
            }
            match &binding.layout {
                Some(layout) => debug.ty(design, layout, needs_mirror(layout)),
                None => debug.value_debug_type(design, id),
            }
        })
        .collect::<Vec<_>>();
    let result_type = source
        .result_layout
        .as_ref()
        .and_then(|layout| debug.ty(design, layout, needs_mirror(layout)))
        .or_else(|| debug.value_debug_type(design, operand));
    let types = binding_types[..source.parameters.len()]
        .iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    let scope = debug.builder.create_function(
        debug.namespace(&owner.replace("::", ".")),
        name,
        Some(""),
        file,
        line,
        debug
            .builder
            .create_subroutine_type(file, result_type, &types, DIFlags::ZERO),
        true,
        true,
        line,
        DIFlags::ZERO,
        false,
    );
    function.set_subprogram(scope);
    debug.declare_lexical_scopes(
        scope,
        source
            .locals
            .iter()
            .filter_map(|binding| binding.scope.map(|extent| (None, extent)))
            .collect(),
        &[],
    );
    let caller = None;
    cache.source_location = Some(debug.builder.create_debug_location(
        debug.context,
        line,
        0,
        scope.as_debug_info_scope(),
        None,
    ));
    builder.set_current_debug_location(cache.source_location.unwrap());
    let mut bindings = Vec::new();
    for (index, (binding, ty)) in source
        .parameters
        .iter()
        .chain(&source.locals)
        .zip(binding_types)
        .enumerate()
    {
        let Some(id) = binding.value else { continue };
        let Some(ty) = ty else {
            continue;
        };
        let literal = process_string(design, id);
        let string = literal.is_some() || dynamic_string_value(design, id);
        let layout = if string {
            None
        } else {
            binding
                .layout
                .as_ref()
                .map(std::borrow::Cow::Borrowed)
                .or_else(|| debug.value_debug_layout(design, id))
        };
        if !string && layout.is_none() {
            continue;
        }
        let (line, column) = debug
            .sources
            .line_col(binding.span.file, binding.span.start);
        let scope = if index < source.parameters.len() {
            scope.as_debug_info_scope()
        } else {
            debug.source_scope(scope, binding.span, None).0
        };
        let variable = if index < source.parameters.len() {
            debug.builder.create_parameter_variable(
                scope,
                &binding.name,
                index as u32 + 1,
                debug.file(binding.span.file),
                line,
                ty,
                true,
                DIFlags::ZERO,
            )
        } else {
            debug.builder.create_auto_variable(
                scope,
                &binding.name,
                debug.file(binding.span.file),
                line,
                ty,
                true,
                DIFlags::ZERO,
                0,
            )
        };
        let location =
            debug
                .builder
                .create_debug_location(debug.context, line, column, scope, caller);
        let name = format!(
            "{}.debug.value.{index}",
            function.get_name().to_string_lossy()
        );
        let mut available = false;
        let (buffer, inspection) = if let Some(text) = literal {
            debug.string_constant_view(module, &name, text);
            available = true;
            (module.get_global(&name).unwrap(), None)
        } else if string {
            let Some(root) = cache.debug_root else {
                continue;
            };
            // A CFG-normalized host call can consume the argument before
            // this native frame. Borrow its existing persistent place;
            // inspection never reexecutes the source expression.
            let place = match design.process_ir.values[id.0 as usize].kind {
                ProcessValueKind::Local { process, local } => {
                    Some(local_state_name(process, local))
                }
                ProcessValueKind::Storage(storage) => Some(storage_state_name(storage)),
                _ => None,
            };
            if let Some(place) = place {
                let name = debug.string_mirror(module, &place, root);
                available = true;
                (module.get_global(&name).unwrap(), None)
            } else {
                (
                    debug.string_buffer(module, &name),
                    Some(ValueInspection::String(root, debug.string_query(module))),
                )
            }
        } else {
            let layout = layout.unwrap();
            let Some(bytes) =
                inspection_bits(&layout).and_then(|bits| u32::try_from(bits / 8).ok())
            else {
                continue;
            };
            (
                debug.inspection_buffer(module, &name, bytes),
                Some(ValueInspection::Packed(layout.into_owned())),
            )
        };
        // Allocate references once per process invocation, never in a source
        // loop. Their contents are inspection-only and do not own source state.
        let entry = builder
            .get_insert_block()
            .unwrap()
            .get_parent()
            .unwrap()
            .get_first_basic_block()
            .unwrap();
        let entry_builder = debug.context.create_builder();
        if let Some(first) = entry.get_first_instruction() {
            entry_builder.position_before(&first);
        } else {
            entry_builder.position_at_end(entry);
        }
        let pointer = debug.context.ptr_type(AddressSpace::default());
        let reference = entry_builder
            .build_alloca(pointer, "debug.value.reference")
            .unwrap();
        builder
            .build_store(
                reference,
                if available {
                    buffer.as_pointer_value()
                } else {
                    pointer.const_null()
                },
            )
            .unwrap();
        unsafe {
            inkwell::llvm_sys::debuginfo::LLVMDIBuilderInsertDeclareRecordAtEnd(
                debug.builder.as_mut_ptr(),
                reference.as_value_ref(),
                variable.as_mut_ptr(),
                debug
                    .builder
                    .create_expression(vec![0x06 /* DW_OP_deref */])
                    .as_mut_ptr(),
                location.as_mut_ptr(),
                builder.get_insert_block().unwrap().as_mut_ptr(),
            );
        }
        if let Some(inspection) = inspection {
            bindings.push(ValueBinding {
                id,
                variable,
                location,
                reference,
                buffer,
                inspection,
                signed: process_value_is_signed(design, id),
                parameter: index < source.parameters.len(),
                captured: Cell::new(None),
            });
        }
    }
    cache.value_frames.push(ValueFrame {
        function: source.function,
        scope,
        caller,
        bindings,
        branches: source.branches.clone(),
    });
    // Constants need no execution. Other bindings become available only when
    // their ordinary SSA value is actually emitted (never inspect by reevaluation).
    for binding in source.parameters.iter().chain(&source.locals) {
        if let Some(id) = binding.value {
            if let Some(number) = process_constant_i64(design, id) {
                let width = design.process_ir.values[id.0 as usize]
                    .bit_width
                    .unwrap_or(64);
                if let Some(width) = std::num::NonZeroU32::new(width) {
                    if let Ok(ty) = debug.context.custom_width_int_type(width) {
                        capture_value(builder, cache, id, ty.const_int(number as u64, true));
                    }
                }
            }
            if let Some(value) = cache
                .parameters
                .get(&id)
                .copied()
                .or_else(|| cache.emitted.get(&cache.key(id, None, None)).copied())
            {
                capture_value(builder, cache, id, value);
            }
        }
    }
    let argument_entry = builder.get_insert_block()?;
    let argument_anchor = argument_entry.get_last_instruction();
    let result = emit(builder, cache, None);
    if let Some(arguments) = &cache.debug_arguments {
        // Native inputs are available at entry, even if their first consumer
        // follows the instruction where GDB stops. Aliases of these same IDs
        // need no execution either. Inspection must not create ABI arguments.
        let argument_builder = debug.context.create_builder();
        if let Some(first) = argument_anchor
            .and_then(|anchor| anchor.get_next_instruction())
            .or_else(|| {
                argument_anchor
                    .is_none()
                    .then(|| argument_entry.get_first_instruction())
                    .flatten()
            })
        {
            argument_builder.position_before(&first);
        } else {
            argument_builder.position_at_end(argument_entry);
        }
        argument_builder.set_current_debug_location(debug.location(scope, source.function, None));
        let mut seen = std::collections::HashSet::new();
        for ((id, ..), value) in &arguments.values {
            if seen.insert(*id) {
                capture_binding(&argument_builder, cache, *id, *value, true);
            }
        }
    }
    let result_span = source.result_span;
    if let Some(value) = result {
        capture_result(module, builder, design, operand, value, result_span, cache);
    }
    cache.value_frames.pop();
    builder.set_current_debug_location(debug.location(scope, result_span, None));
    result
}

fn capture_result<'ctx>(
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    id: ProcessValueId,
    value: IntValue<'ctx>,
    span: Span,
    cache: &ProcessValueCache<'ctx, '_>,
) {
    let Some(debug) = cache.debug else { return };
    let Some(layout) = debug.value_debug_layout(design, id) else {
        return;
    };
    // A constant/arithmetic return can otherwise have no machine range even
    // at -O0. These inspection writes never become simulation operands.
    let _location = ValueLocationGuard::new(builder, cache, span);
    if let Some(bytes) = inspection_bits(&layout).and_then(|bits| u32::try_from(bits / 8).ok()) {
        let buffer = debug.inspection_buffer(module, "sx.debug.return", bytes);
        if let Some(value) = fit(builder, value, layout.packed_width().unwrap()) {
            debug.copy_fields(builder, buffer, value, &layout, &mut 0, &mut 0);
        }
    }
}

pub(in crate::llvm::process) fn selected_value<'ctx>(
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    selection: ProcessValueId,
    condition: IntValue<'ctx>,
    cache: &mut ProcessValueCache<'ctx, '_>,
    mut emit: impl FnMut(
        &Builder<'ctx>,
        ProcessValueId,
        &mut ProcessValueCache<'ctx, '_>,
    ) -> Option<IntValue<'ctx>>,
) -> Option<IntValue<'ctx>> {
    let debug = cache.debug?;
    let ProcessValueKind::Select {
        then_value,
        else_value,
        ..
    } = design.process_ir.values.get(selection.0 as usize)?.kind
    else {
        return None;
    };
    let arms = [then_value, else_value];
    let spans = cache
        .value_frames
        .last()?
        .branches
        .iter()
        .find_map(|(id, spans)| (*id == selection).then_some(*spans))
        .unwrap_or_else(|| arms.map(|id| design.process_ir.values[id.0 as usize].span));
    let function = builder.get_insert_block()?.get_parent()?;
    let then_block = debug
        .context
        .append_basic_block(function, "debug.select.then");
    let else_block = debug
        .context
        .append_basic_block(function, "debug.select.else");
    let join = debug
        .context
        .append_basic_block(function, "debug.select.join");
    builder
        .build_conditional_branch(condition, then_block, else_block)
        .ok()?;
    let dominating = cache.dominating_values();
    let saved = cache.source_results.clone();
    let mut results = Vec::with_capacity(2);
    let mut calls = Vec::with_capacity(2);
    for ((block, id), span) in [(then_block, arms[0]), (else_block, arms[1])]
        .into_iter()
        .zip(spans)
    {
        builder.position_at_end(block);
        let value = emit(builder, id, cache)?;
        capture_result(module, builder, design, id, value, span, cache);
        let exit = builder.get_insert_block()?;
        builder.build_unconditional_branch(join).ok()?;
        results.push((value, exit));
        calls.push(std::mem::take(&mut cache.source_results));
        cache.retain_dominating(&dominating);
        cache.source_results = saved.clone();
    }
    builder.position_at_end(join);
    cache.source_results = merge_results(
        debug.context,
        builder,
        [(&calls[0], results[0].1), (&calls[1], results[1].1)],
    )?;
    let result = builder
        .build_phi(results[0].0.get_type(), "debug.select.result")
        .ok()?;
    result.add_incoming(&[(&results[0].0, results[0].1), (&results[1].0, results[1].1)]);
    Some(result.as_basic_value().into_int_value())
}

fn value_location<'ctx>(
    cache: &ProcessValueCache<'ctx, '_>,
    span: Span,
) -> Option<DILocation<'ctx>> {
    let debug = cache.debug?;
    cache
        .value_frames
        .iter()
        .rev()
        .find(|frame| contains(frame.function, span))
        .map(|frame| {
            let (line, column) = debug.sources.line_col(span.file, span.start);
            debug.builder.create_debug_location(
                debug.context,
                line,
                column,
                debug.source_scope(frame.scope, span, None).0,
                frame.caller,
            )
        })
        .or(cache.source_location)
}

pub(in crate::llvm::process) fn capture_value<'ctx>(
    builder: &Builder<'ctx>,
    cache: &ProcessValueCache<'ctx, '_>,
    id: ProcessValueId,
    value: IntValue<'ctx>,
) {
    capture_binding(builder, cache, id, value, false);
}

fn capture_binding<'ctx>(
    builder: &Builder<'ctx>,
    cache: &ProcessValueCache<'ctx, '_>,
    id: ProcessValueId,
    value: IntValue<'ctx>,
    at_entry: bool,
) {
    let Some(debug) = cache.debug else { return };
    let mut local_captured = false;
    for frame in &cache.value_frames {
        for binding in &frame.bindings {
            if binding.id != id {
                continue;
            }
            let capture = (value, builder.get_insert_block().unwrap());
            if binding.captured.get() == Some(capture) {
                continue;
            }
            // Arithmetic may use wider carry temporaries than the source
            // binding. Describe its declared value, not the carry bits.
            match &binding.inspection {
                ValueInspection::Packed(layout) => {
                    let width = layout.packed_width().unwrap();
                    let value = if binding.signed && process_layout_is_signed(layout) {
                        fit_signed(builder, value, width)
                    } else {
                        fit(builder, value, width)
                    };
                    let Some(value) = value else {
                        continue;
                    };
                    debug.copy_fields(builder, binding.buffer, value, layout, &mut 0, &mut 0);
                }
                ValueInspection::String(root, query) => {
                    let Some(value) = fit(builder, value, 64) else {
                        continue;
                    };
                    // The module owns this global and the runtime query.
                    // Keep borrowed content behind the existing inspection ABI.
                    debug.capture_string(builder, *query, *root, value, binding.buffer);
                }
            }
            builder
                .build_store(binding.reference, binding.buffer.as_pointer_value())
                .unwrap();
            // LLVM 22 inserts a debug record, not an LLVM instruction.
            unsafe {
                inkwell::llvm_sys::debuginfo::LLVMDIBuilderInsertDeclareRecordAtEnd(
                    debug.builder.as_mut_ptr(),
                    binding.reference.as_value_ref(),
                    binding.variable.as_mut_ptr(),
                    debug.builder.create_expression(vec![0x06]).as_mut_ptr(),
                    binding.location.as_mut_ptr(),
                    builder.get_insert_block().unwrap().as_mut_ptr(),
                );
            }
            binding.captured.set(Some(capture));
            local_captured |= !binding.parameter && !at_entry;
        }
    }
    if local_captured {
        // SelectionDAG can schedule later source arithmetic before these
        // inspection stores even at -O0. A debug-only block boundary keeps
        // a completed local visible at the next source statement's stop.
        let previous = builder.get_insert_block().unwrap();
        let next = debug
            .context
            .append_basic_block(previous.get_parent().unwrap(), "debug.value.available");
        builder.build_unconditional_branch(next).unwrap();
        builder.position_at_end(next);
        for frame in &cache.value_frames {
            for binding in &frame.bindings {
                if let Some((value, block)) = binding.captured.get() {
                    if block == previous {
                        binding.captured.set(Some((value, next)));
                    }
                }
            }
        }
    }
}
