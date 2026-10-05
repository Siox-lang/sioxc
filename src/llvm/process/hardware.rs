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
    pub(super) checked: Vec<bool>,
    effects: Vec<bool>,
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
        Self {
            supported: supported_process_values(design),
            checked: checked_process_values(design),
            effects,
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
        let mut cache = ProcessValueCache::new(&self.checked, &self.supported.meta_free);
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
