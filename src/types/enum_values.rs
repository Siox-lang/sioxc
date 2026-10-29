//! Computed enum values, shared by analysis and downstream phase products.

use super::*;

#[cfg(test)]
mod tests;

#[derive(Clone, Default)]
pub(crate) struct EnumValues {
    values: HashMap<DefId, Vec<(DefId, Option<i128>)>>,
}

impl EnumValues {
    pub(crate) fn collect(declarations: &DeclarationIndex<'_>) -> Self {
        Self::collect_with_functions(declarations, None)
    }

    pub(crate) fn collect_with_functions(
        declarations: &DeclarationIndex<'_>,
        functions: Option<&DeclarationIndex<'_>>,
    ) -> Self {
        let mut result = Self::default();
        result.refine(declarations, functions);
        result
    }

    fn refine(
        &mut self,
        declarations: &DeclarationIndex<'_>,
        functions: Option<&DeclarationIndex<'_>>,
    ) {
        let sources: Vec<_> = declarations
            .enumerations()
            .filter_map(|(owner, _)| {
                declarations
                    .enum_variants(owner)
                    .map(|members| (owner, members))
            })
            .collect();
        for (owner, members) in &sources {
            self.values
                .entry(*owner)
                .or_insert_with(|| members.iter().map(|(id, _)| (*id, None)).collect());
        }
        // Only resolved facts advance the fixed point; an unknown explicit value
        // makes following implicit values unknown, not zero-based guesses.
        loop {
            let mut changed = false;
            for (owner, members) in &sources {
                let mut next = Some(0i128);
                for (position, (_, member)) in members.iter().enumerate() {
                    if let Some(value) = self.values[owner][position].1 {
                        next = value.checked_add(1);
                        continue;
                    }
                    let value = match &member.value {
                        Some(expression) => {
                            let source = consteval::Source::new(declarations)
                                .with_enums(self)
                                .with_functions(functions);
                            consteval::evaluate(expression, &source, 0).ok()
                        }
                        None => next,
                    };
                    next = value.and_then(|value| value.checked_add(1));
                    if let Some(value) = value {
                        let entry = &mut self.values.get_mut(owner).unwrap()[position].1;
                        if entry.is_none() {
                            *entry = Some(value);
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn has_unknown(&self) -> bool {
        self.values
            .values()
            .flatten()
            .any(|(_, value)| value.is_none())
    }

    pub(crate) fn check_declaration(
        &self,
        declaration: &EnumDecl,
        resolved: &Resolved,
        sink: &mut DiagnosticSink,
        previously_known: Option<&HashSet<(DefId, DefId)>>,
    ) {
        let Some(owner) = resolved.declared(declaration.name.span) else {
            return;
        };
        let mut seen: HashMap<i128, (&str, DefId)> = HashMap::new();
        for variant in &declaration.variants {
            let Some(id) = resolved.enum_variant(owner, &variant.name.text) else {
                continue;
            };
            let Some(value) = self.number(owner, id) else {
                continue;
            };
            let newly_known =
                |id| previously_known.is_none_or(|known| !known.contains(&(owner, id)));
            if value < 0 {
                if newly_known(id) {
                    sink.emit(Diagnostic::error(format!(
                        "`{}::{}` has the negative discriminant {value}; an enum's discriminants start at 0",
                        declaration.name.text, variant.name.text,
                    )).with_code(codes::TYPE_MISMATCH)
                        .at(variant.value.as_ref().map(expr_span).unwrap_or(variant.name.span)));
                }
                continue;
            }
            if let Some((previous, previous_id)) = seen.insert(value, (&variant.name.text, id)) {
                if newly_known(id) || newly_known(previous_id) {
                    sink.emit(
                        Diagnostic::error(format!(
                            "`{}::{}` and `{}::{previous}` both have the value {value}",
                            declaration.name.text, variant.name.text, declaration.name.text,
                        ))
                        .with_code(codes::DUPLICATE_ITEM)
                        .at(variant.name.span),
                    );
                }
            }
        }
    }

    pub(crate) fn members(&self, owner: DefId) -> Option<&[(DefId, Option<i128>)]> {
        self.values.get(&owner).map(Vec::as_slice)
    }

    pub(crate) fn owners(&self) -> impl Iterator<Item = DefId> + '_ {
        self.values.keys().copied()
    }

    pub(crate) fn number(&self, owner: DefId, variant: DefId) -> Option<i128> {
        self.members(owner)?
            .iter()
            .find(|(id, _)| *id == variant)?
            .1
    }

    pub(crate) fn value(&self, owner: DefId, variant: DefId) -> Option<u64> {
        let value = self.number(owner, variant)?;
        u64::try_from(value).ok()
    }

    pub(crate) fn width(&self, owner: DefId) -> Option<u32> {
        let members = self.members(owner)?;
        let largest = members.iter().try_fold(0u128, |largest, (_, value)| {
            Some(largest.max(u128::try_from((*value)?).ok()?))
        })?;
        let largest = largest.max(members.len().saturating_sub(1) as u128);
        Some((u128::BITS - largest.leading_zeros()).max(1))
    }
}

/// Refine facts after semantic-preserving attribute/length normalization.
pub fn refresh_enum_values(
    modules: &[Module],
    resolved: &Resolved,
    typed: &mut Typed,
    sink: &mut DiagnosticSink,
) {
    if !typed.enum_values.has_unknown() {
        return;
    }
    let known: HashSet<_> = typed
        .enum_values
        .values
        .iter()
        .flat_map(|(&owner, members)| {
            members
                .iter()
                .filter_map(move |(variant, value)| value.map(|_| (owner, *variant)))
        })
        .collect();
    let declarations = DeclarationIndex::collect(resolved, modules);
    let values = std::sync::Arc::make_mut(&mut typed.enum_values);
    values.refine(&declarations, None);
    // Diagnostics retain source order, independent of the fact table's hashing.
    for item in modules.iter().flat_map(|module| &module.items) {
        if let Item::Enum(declaration) = item {
            values.check_declaration(declaration, resolved, sink, Some(&known));
        }
    }
}
