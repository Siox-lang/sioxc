//! Formal parameters reference captured caller state, never simulation copies.

use super::*;
use inkwell::debug_info::{DIExpression, DILocalVariable};
use inkwell::values::PointerValue;
use std::borrow::Cow;

pub(super) struct ParameterLocation<'ctx> {
    variable: DILocalVariable<'ctx>,
    reference: PointerValue<'ctx>,
    expression: DIExpression<'ctx>,
    span: Span,
    snapshot: Option<(ProcessValueId, SourceLayout, GlobalValue<'ctx>)>,
}

impl<'ctx> DebugInfo<'ctx, '_> {
    pub(super) fn value_debug_layout<'a>(
        &self,
        design: &'a Design,
        id: ProcessValueId,
    ) -> Option<Cow<'a, SourceLayout>> {
        if let Some(layout) = process_value_layout(design, id) {
            return Some(Cow::Borrowed(layout));
        }
        let value = design.process_ir.values.get(id.0 as usize)?;
        let width = value
            .ty
            .as_ref()
            .and_then(siox::types::Ty::bit_width)
            .filter(|width| *width != 0)
            .or(value.bit_width)?;
        let domain = match value.ty.as_ref() {
            Some(siox::types::Ty::Integer) => ScalarDomain::Integer,
            Some(siox::types::Ty::Real) => ScalarDomain::Real,
            Some(siox::types::Ty::Char) => ScalarDomain::Character,
            _ => ScalarDomain::Bits,
        };
        Some(Cow::Owned(SourceLayout {
            span: value.span,
            kind: LayoutKind::Scalar {
                width,
                domain,
                nominal: None,
                value_range: None,
            },
        }))
    }

    pub(super) fn value_debug_type(
        &self,
        design: &Design,
        id: ProcessValueId,
    ) -> Option<DIType<'ctx>> {
        if dynamic_string_value(design, id) {
            return Some(self.string_debug_type(
                self.namespace(""),
                design.process_ir.values[id.0 as usize].span,
            ));
        }
        let layout = self.value_debug_layout(design, id)?;
        self.ty(design, &layout, needs_mirror(&layout))
    }

    pub(super) fn parameters(
        &self,
        module: &Module<'ctx>,
        design: &Design,
        process: &ProcessCfg,
        builder: &Builder<'ctx>,
        root: DISubprogram<'ctx>,
    ) {
        let key = root.as_mut_ptr() as usize;
        let frames = self.inline_scopes.borrow()[&key].clone();
        let mut locations = Vec::new();
        for (call_id, call) in process.inline_calls.iter().enumerate() {
            let scope = frames[call_id].0.as_debug_info_scope();
            let mut parameters = Vec::new();
            for (index, parameter) in call.parameters.iter().enumerate() {
                let Some(layout) = self.value_debug_layout(design, parameter.value) else {
                    continue;
                };
                let snapshot = matches!(
                    design.process_ir.values[parameter.value.0 as usize].kind,
                    ProcessValueKind::Field { .. }
                        | ProcessValueKind::Index { .. }
                        | ProcessValueKind::BitSlice { .. }
                        | ProcessValueKind::PackedSlice { .. }
                        | ProcessValueKind::Concat(_)
                        | ProcessValueKind::Signal { .. }
                );
                let string = dynamic_string_value(design, parameter.value);
                let normalized = string || snapshot || needs_mirror(&layout);
                let ty = if string {
                    Some(self.string_debug_type(scope, parameter.span))
                } else {
                    self.ty(design, &layout, normalized)
                };
                let Some(ty) = ty else {
                    continue;
                };
                let node = &design.process_ir.values[parameter.value.0 as usize];
                let name = match &node.kind {
                    ProcessValueKind::Local { process, local } => {
                        local_state_name(*process, *local)
                    }
                    ProcessValueKind::Storage(storage) => storage_state_name(*storage),
                    ProcessValueKind::Number(_)
                    | ProcessValueKind::Default
                    | ProcessValueKind::BitString { .. }
                    | ProcessValueKind::Char(_)
                    | ProcessValueKind::String(_) => {
                        let mut cache = ProcessValueCache::new(&[], &[]);
                        let Some(value) = process_value_at(
                            self.context,
                            module,
                            builder,
                            design,
                            parameter.value,
                            layout.packed_width().unwrap(),
                            process_layout_is_signed(&layout),
                            None,
                            &HashMap::new(),
                            &mut cache,
                        ) else {
                            continue;
                        };
                        if !value.is_const() {
                            continue;
                        }
                        let name =
                            format!("sx.debug.argument.{}.{}.{}", process.id.0, call_id, index);
                        let global = module.add_global(value.get_type(), None, &name);
                        global.set_initializer(&value);
                        global.set_linkage(Linkage::Internal);
                        global.set_constant(true);
                        name
                    }
                    ProcessValueKind::Field { .. }
                    | ProcessValueKind::Index { .. }
                    | ProcessValueKind::BitSlice { .. }
                    | ProcessValueKind::PackedSlice { .. }
                    | ProcessValueKind::Concat(_)
                    | ProcessValueKind::Signal { .. } => {
                        let name =
                            format!("sx.debug.argument.{}.{}.{}", process.id.0, call_id, index);
                        let Some(bytes) =
                            inspection_bits(&layout).and_then(|bits| u32::try_from(bits / 8).ok())
                        else {
                            continue;
                        };
                        self.inspection_buffer(module, &name, bytes);
                        name
                    }
                    // Only captured constants/places are inspected, never an
                    // unknown expression with possible language effects.
                    _ => continue,
                };
                if module.get_global(&name).is_none() {
                    continue;
                }
                let name = if string {
                    self.string_mirror(module, &name, process.root.0)
                } else if normalized && !snapshot {
                    let mirror = format!("{name}.debug");
                    if module.get_global(&mirror).is_some() {
                        mirror
                    } else {
                        self.mirror(module, &name, &layout)
                    }
                } else {
                    name
                };
                // LLVM 22 drops dbg.declare directly on global storage. A
                // stack-held reference retains a real formal-argument location
                // and still points at the existing mutable/persistent state.
                let reference = builder
                    .build_alloca(
                        self.context.ptr_type(AddressSpace::default()),
                        "debug.argument.reference",
                    )
                    .unwrap();
                builder
                    .build_store(
                        reference,
                        module.get_global(&name).unwrap().as_pointer_value(),
                    )
                    .unwrap();
                let mut ops = vec![0x06 /* DW_OP_deref */];
                if !normalized {
                    ops.extend(self.expression_ops(&layout, 0, false));
                }
                parameters.push(ParameterLocation {
                    variable: self.builder.create_parameter_variable(
                        scope,
                        &parameter.name,
                        index as u32 + 1,
                        self.file(parameter.span.file),
                        self.sources
                            .line_col(parameter.span.file, parameter.span.start)
                            .0,
                        ty,
                        true,
                        DIFlags::ZERO,
                    ),
                    reference,
                    expression: self.builder.create_expression(ops),
                    span: parameter.span,
                    snapshot: snapshot.then(|| {
                        (
                            parameter.value,
                            layout.into_owned(),
                            module.get_global(&name).unwrap(),
                        )
                    }),
                });
            }
            locations.push(parameters);
        }
        self.parameter_locations.borrow_mut().insert(key, locations);
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::llvm::process) fn sync_parameters(
        &self,
        module: &Module<'ctx>,
        builder: &Builder<'ctx>,
        design: &Design,
        root: DISubprogram<'ctx>,
        mut inline: Option<u32>,
        index_sites: &HashMap<IndexSite, u32>,
        activity: &[bool],
        meta_free: &[bool],
    ) {
        let key = root.as_mut_ptr() as usize;
        let mut cache = ProcessValueCache::new(activity, meta_free);
        while let Some(id) = inline {
            for parameter in &self.parameter_locations.borrow()[&key][id as usize] {
                if let Some((value, layout, buffer)) = &parameter.snapshot {
                    // Bindings are already captured places. False activity
                    // suppresses bounds side effects in the existing reader;
                    // no foreign calls or IO are reachable through these roots.
                    if let Some(value) = process_value_in_layout(
                        self.context,
                        module,
                        builder,
                        design,
                        *value,
                        layout,
                        Some(self.context.bool_type().const_zero()),
                        index_sites,
                        &mut cache,
                    ) {
                        self.copy_fields(builder, *buffer, value, layout, &mut 0, &mut 0);
                    }
                }
            }
            inline = self.parameter_parents.borrow()[&key][id as usize];
        }
    }

    pub(in crate::llvm::process) fn declare_parameters(
        &self,
        builder: &Builder<'ctx>,
        root: DISubprogram<'ctx>,
        mut inline: Option<u32>,
    ) {
        let key = root.as_mut_ptr() as usize;
        while let Some(id) = inline {
            let locations = self.parameter_locations.borrow();
            if let Some(parameters) = locations.get(&key).and_then(|calls| calls.get(id as usize)) {
                for parameter in parameters {
                    // Parameters belong to the whole callee, not the synthetic
                    // file scope used for locations outside its body extent.
                    // That file block has no machine range and drops locations.
                    let (scope, call) = self.inline_scopes.borrow()[&key][id as usize];
                    let (line, column) = self
                        .sources
                        .line_col(parameter.span.file, parameter.span.start);
                    let location = self.builder.create_debug_location(
                        self.context,
                        line,
                        column,
                        scope.as_debug_info_scope(),
                        call,
                    );
                    // LLVMDbgRecordRef is not an LLVMValueRef/InstructionValue.
                    unsafe {
                        inkwell::llvm_sys::debuginfo::LLVMDIBuilderInsertDeclareRecordAtEnd(
                            self.builder.as_mut_ptr(),
                            parameter.reference.as_value_ref(),
                            parameter.variable.as_mut_ptr(),
                            parameter.expression.as_mut_ptr(),
                            location.as_mut_ptr(),
                            builder.get_insert_block().unwrap().as_mut_ptr(),
                        );
                    }
                }
            }
            inline = self.parameter_parents.borrow()[&key][id as usize];
        }
    }
}
