//! Native source frames over the same canonical expressions as ordinary builds.

use super::*;
use inkwell::basic_block::BasicBlock;
use inkwell::debug_info::DILocalVariable;
use siox::ir::ProcessValueInline;

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
    parameter: bool,
    captured: Cell<Option<(IntValue<'ctx>, BasicBlock<'ctx>)>>,
}

enum ValueInspection<'ctx> {
    Packed(SourceLayout),
    String(u32, FunctionValue<'ctx>),
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
    if active.is_some_and(|active| active.get_zero_extended_constant() == Some(0)) {
        return Some(
            debug
                .context
                .custom_width_int_type(std::num::NonZeroU32::new(width)?)
                .ok()?
                .const_zero(),
        );
    }
    let active = active.filter(|active| active.get_zero_extended_constant() != Some(1));
    if let Some(active) = active {
        let previous = builder.get_insert_block()?;
        let function = previous.get_parent()?;
        let execute = debug
            .context
            .append_basic_block(function, "debug.call.execute");
        let join = debug
            .context
            .append_basic_block(function, "debug.call.join");
        builder
            .build_conditional_branch(active, execute, join)
            .ok()?;
        let dominating = cache.dominating_values();
        builder.position_at_end(execute);
        let returned = inline_value(
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
        )?;
        let exit = builder.get_insert_block()?;
        builder.build_unconditional_branch(join).ok()?;
        builder.position_at_end(join);
        cache.retain_dominating(&dominating);
        let result = builder
            .build_phi(returned.get_type(), "debug.call.result")
            .ok()?;
        result.add_incoming(&[
            (&returned.get_type().const_zero(), previous),
            (&returned, exit),
        ]);
        return Some(result.as_basic_value().into_int_value());
    }
    // A binding retained elsewhere in the arena need not be used by this
    // callee. Metadata must never introduce executable roots. Stop at the
    // source argument boundaries, not their caller-side subexpressions.
    let parameters = source
        .parameters
        .iter()
        .filter_map(|parameter| parameter.value)
        .collect::<std::collections::HashSet<_>>();
    let mut needed = std::collections::HashSet::new();
    let mut visited = std::collections::HashSet::new();
    let mut pending = if parameters.is_empty() {
        Vec::new()
    } else {
        vec![operand]
    };
    while let Some(value) = pending.pop() {
        if !visited.insert(value) {
            continue;
        }
        if parameters.contains(&value) {
            needed.insert(value);
        } else {
            pending.extend(crate::ir::process_value_dependencies(
                &design.process_ir.values.get(value.0 as usize)?.kind,
            ));
        }
    }
    let mut arguments = Vec::new();
    let mut parameters = Vec::new();
    for parameter in &source.parameters {
        let Some(value) = parameter.value else {
            continue;
        };
        // Literal strings remain compile-time operands, including empty
        // zero-storage values. The body already owns their contents; an
        // unconstrained source type is not an ABI for their literal bits.
        if !needed.contains(&value) || process_string(design, value).is_some() {
            continue;
        }
        let argument = if let Some(layout) = debug.value_debug_layout(design, value) {
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
            process_value(
                debug.context,
                module,
                builder,
                design,
                value,
                None,
                index_sites,
                cache,
            )?
        };
        arguments.push(argument.into());
        parameters.push((value, argument.get_type().into()));
    }
    let key = (id, width, signed, layout.cloned());
    let existing = debug.value_functions.borrow().get(&key).copied();
    let function = if let Some(function) = existing {
        function
    } else {
        let ty = debug
            .context
            .custom_width_int_type(std::num::NonZeroU32::new(width)?)
            .ok()?;
        let types = parameters.iter().map(|(_, ty)| *ty).collect::<Vec<_>>();
        let function = module.add_function(
            &format!("sx.debug.call.{}", id.0),
            ty.fn_type(&types, false),
            Some(Linkage::Internal),
        );
        debug.value_functions.borrow_mut().insert(key, function);
        let body_builder = debug.context.create_builder();
        body_builder.position_at_end(debug.context.append_basic_block(function, "entry"));
        let mut body_cache = ProcessValueCache::new(cache.activity_sensitive, cache.meta_free);
        body_cache.debug = Some(debug);
        body_cache.debug_root = cache.debug_root;
        body_cache.hardware_calls = cache.hardware_calls;
        for (index, (parameter, _)) in parameters.iter().enumerate() {
            body_cache.parameters.insert(
                *parameter,
                function.get_nth_param(index as u32)?.into_int_value(),
            );
        }
        let returned = inline_body(
            module,
            &body_builder,
            design,
            function,
            operand,
            source,
            &mut body_cache,
            emit,
        )?;
        body_builder.build_return(Some(&returned)).ok()?;
        function
    };
    builder
        .build_call(function, &arguments, "debug.call")
        .ok()?
        .try_as_basic_value()
        .basic()
        .map(|value| value.into_int_value())
}

#[allow(clippy::too_many_arguments)]
fn inline_body<'ctx>(
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
            binding
                .value
                .and_then(|id| debug.value_debug_type(design, id))
        })
        .collect::<Vec<_>>();
    let result_type = debug.value_debug_type(design, operand);
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
            debug.value_debug_layout(design, id)
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
            let place = if cache.parameters.contains_key(&id) {
                None
            } else {
                match design.process_ir.values[id.0 as usize].kind {
                    ProcessValueKind::Local { process, local } => {
                        Some(local_state_name(process, local))
                    }
                    ProcessValueKind::Storage(storage) => Some(storage_state_name(storage)),
                    _ => None,
                }
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
            if let Some(value) = cache.emitted.get(&cache.key(id, None, None)).copied() {
                capture_value(builder, cache, id, value);
            }
            if let Some(value) = cache.parameters.get(&id).copied() {
                capture_value(builder, cache, id, value);
            }
        }
    }
    let result = emit(builder, cache, None);
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
    let mut results = Vec::with_capacity(2);
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
        cache.retain_dominating(&dominating);
    }
    builder.position_at_end(join);
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
                    let Some(value) = fit(builder, value, layout.packed_width().unwrap()) else {
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
            local_captured |= !binding.parameter;
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
