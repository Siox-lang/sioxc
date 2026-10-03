//! Derived hardware views delegate values to the canonical Process emitter.

use super::*;

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
    ) -> Option<IntValue<'ctx>> {
        let mut cache = ProcessValueCache::new(&self.checked, &self.supported.meta_free);
        process_value_at(
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
        )
    }
}
