//! Derived hardware views delegate values to the canonical Process emitter.

use super::*;

/// Owned cache entries shared by roots in one straight-line state epoch.
/// Checked/metadata facts remain object-owned; no SSA value survives a state
/// write or a basic-block boundary. Captured foreign results survive state
/// invalidation, just as they do inside the procedural emitter.
#[derive(Default)]
pub(in crate::llvm) struct HardwareValueCache<'ctx> {
    evaluated_calls: HashMap<(ProcessValueId, Option<IntValue<'ctx>>), IntValue<'ctx>>,
    emitted: HashMap<(ProcessValueId, Option<IntValue<'ctx>>, Option<u32>), IntValue<'ctx>>,
    contextual: HashMap<(ProcessValueId, Option<IntValue<'ctx>>, u32, bool), IntValue<'ctx>>,
    metadata: HashMap<(ProcessValueId, Option<IntValue<'ctx>>, SourceLayout), IntValue<'ctx>>,
    signals: HashMap<(SignalId, ProcessSignalState, u32), IntValue<'ctx>>,
}

impl HardwareValueCache<'_> {
    pub(in crate::llvm) fn clear(&mut self) {
        self.emitted.clear();
        self.contextual.clear();
        self.metadata.clear();
        self.signals.clear();
    }
}

/// Per-object facts, not a second hardware expression representation.
pub(in crate::llvm) struct HardwareValueFacts {
    pub(super) supported: ProcessValueSupport,
    pub(super) activity_sensitive: Vec<bool>,
    effects: Vec<bool>,
    calls: Vec<ProcessValueId>,
}

impl HardwareValueFacts {
    pub(in crate::llvm) fn new(design: &Design) -> Self {
        let mut effects = Vec::with_capacity(design.process_ir.values.len());
        for value in &design.process_ir.values {
            let effect = matches!(
                value.kind,
                ProcessValueKind::ForeignCall { .. } | ProcessValueKind::HostCall { .. }
            ) || crate::ir::process::process_value_dependencies(&value.kind)
                .iter()
                .any(|id| effects.get(id.0 as usize).copied().unwrap_or(false));
            effects.push(effect);
        }
        let mut pending = design
            .drivers
            .iter()
            .flat_map(|write| write.cond.iter().chain(std::iter::once(&write.expr)))
            .chain(design.event_blocks.iter().flat_map(|block| {
                std::iter::once(&block.condition).chain(
                    block
                        .updates
                        .iter()
                        .flat_map(|write| write.cond.iter().chain(std::iter::once(&write.expr))),
                )
            }))
            .filter_map(|expr| match expr {
                crate::ir::Expr::Canonical { value, .. } => Some(*value),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut visited = vec![false; design.process_ir.values.len()];
        let mut calls = Vec::new();
        while let Some(id) = pending.pop() {
            if !effects[id.0 as usize] {
                continue;
            }
            if std::mem::replace(&mut visited[id.0 as usize], true) {
                continue;
            }
            let value = &design.process_ir.values[id.0 as usize];
            if matches!(
                value.kind,
                ProcessValueKind::ForeignCall { .. } | ProcessValueKind::HostCall { .. }
            ) {
                calls.push(id);
            }
            pending.extend(crate::ir::process::process_value_dependencies(&value.kind));
        }
        calls.sort_unstable_by_key(|id| id.0);
        Self {
            supported: supported_process_values(design),
            activity_sensitive: activity_sensitive_process_values(design),
            effects,
            calls,
        }
    }

    pub(in crate::llvm) fn declare_calls<'ctx>(
        &self,
        context: &'ctx Context,
        module: &Module<'ctx>,
        design: &Design,
    ) {
        for id in &self.calls {
            let width = design.process_ir.values[id.0 as usize]
                .bit_width
                .expect("supported hardware call width");
            let ty = context
                .custom_width_int_type(std::num::NonZeroU32::new(width).unwrap())
                .expect("validated call width");
            let global = module.add_global(ty, None, &format!("sx.hardware.call.{}", id.0));
            global.set_initializer(&ty.const_zero());
            global.set_linkage(Linkage::Internal);
            let ready = module.add_global(
                context.bool_type(),
                None,
                &format!("sx.hardware.call.{}.ready", id.0),
            );
            ready.set_initializer(&context.bool_type().const_zero());
            ready.set_linkage(Linkage::Internal);
        }
    }

    pub(in crate::llvm) fn reset_calls<'ctx>(
        &self,
        context: &'ctx Context,
        module: &Module<'ctx>,
        builder: &Builder<'ctx>,
    ) {
        for id in &self.calls {
            let ready = module
                .get_global(&format!("sx.hardware.call.{}.ready", id.0))
                .expect("declared hardware call capture");
            builder
                .build_store(ready.as_pointer_value(), context.bool_type().const_zero())
                .unwrap();
        }
    }

    pub(in crate::llvm) fn supports(&self, id: ProcessValueId) -> bool {
        self.supported.get(id.0 as usize).copied().unwrap_or(false)
    }

    pub(in crate::llvm) fn has_effects(&self, id: ProcessValueId) -> bool {
        self.effects.get(id.0 as usize).copied().unwrap_or(false)
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::llvm) fn emit<'ctx>(
        &self,
        context: &'ctx Context,
        module: &Module<'ctx>,
        builder: &Builder<'ctx>,
        design: &Design,
        id: ProcessValueId,
        width: u32,
        signed: bool,
        active: Option<IntValue<'ctx>>,
        index_sites: &HashMap<IndexSite, u32>,
        values: &mut HardwareValueCache<'ctx>,
    ) -> Option<IntValue<'ctx>> {
        let mut cache = ProcessValueCache::new(&self.activity_sensitive, &self.supported.meta_free);
        cache.hardware_calls = true;
        cache.evaluated_calls = std::mem::take(&mut values.evaluated_calls);
        cache.emitted = std::mem::take(&mut values.emitted);
        cache.contextual = std::mem::take(&mut values.contextual);
        cache.metadata = std::mem::take(&mut values.metadata);
        cache.signals = std::mem::take(&mut values.signals);
        let result = process_value_at(
            context,
            module,
            builder,
            design,
            id,
            width,
            signed,
            active,
            index_sites,
            &mut cache,
        );
        values.evaluated_calls = cache.evaluated_calls;
        values.emitted = cache.emitted;
        values.contextual = cache.contextual;
        values.metadata = cache.metadata;
        values.signals = cache.signals;
        result
    }
}

/// One canonical call is evaluated once per compatibility scheduling phase,
/// even when its aggregate result is projected by several bounded helpers.
/// Activity is tested by the common emitter before entering this capture.
#[allow(clippy::too_many_arguments)]
pub(super) fn hardware_call_value<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    id: ProcessValueId,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    let global = module.get_global(&format!("sx.hardware.call.{}", id.0))?;
    let ready = module.get_global(&format!("sx.hardware.call.{}.ready", id.0))?;
    let function = builder.get_insert_block()?.get_parent()?;
    let execute = context.append_basic_block(function, "pv.capture.execute");
    let join = context.append_basic_block(function, "pv.capture.join");
    let captured = builder
        .build_load(
            context.bool_type(),
            ready.as_pointer_value(),
            "pv.capture.ready",
        )
        .ok()?
        .into_int_value();
    builder
        .build_conditional_branch(captured, join, execute)
        .ok()?;
    let dominating_calls = cache
        .evaluated_calls
        .keys()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    cache.clear();
    builder.position_at_end(execute);
    let previous = cache.evaluating_hardware_call.replace(id);
    let result = process_value(
        context,
        module,
        builder,
        design,
        id,
        None,
        index_sites,
        cache,
    );
    cache.evaluating_hardware_call = previous;
    let result = result?;
    builder
        .build_store(global.as_pointer_value(), result)
        .ok()?;
    builder
        .build_store(
            ready.as_pointer_value(),
            context.bool_type().const_int(1, false),
        )
        .ok()?;
    builder.build_unconditional_branch(join).ok()?;
    builder.position_at_end(join);
    cache.clear();
    cache
        .evaluated_calls
        .retain(|key, _| dominating_calls.contains(key));
    let result = builder
        .build_load(
            result.get_type(),
            global.as_pointer_value(),
            "pv.capture.value",
        )
        .ok()?
        .into_int_value();
    cache.evaluated_calls.insert((id, None), result);
    Some(result)
}
