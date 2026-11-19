//! Derived widths, array families, and enum representation metadata.

use super::*;

/// Every derived type's inherited width: `struct Byte : Logic[8]` -> 8,
/// `struct Word : Byte` -> 8 (following the base chain). A derived type reuses
/// its base array's size/range (spec: nominal derivation). Testbench evaluators
/// consult this so a local of a derived vector type masks to the right width.
pub fn derived_widths(modules: &[Module], fns: &FunctionIndex<'_>) -> HashMap<String, u32> {
    let mut structs: HashMap<String, &ast::StructDecl> = HashMap::new();
    for m in modules {
        for it in &m.items {
            if let ast::Item::Struct(s) = it {
                structs.insert(fns.struct_decl_key(&s.name), s);
            }
        }
    }
    let empty_env = HashMap::new();
    structs
        .iter()
        .filter_map(|(name, s)| {
            let w = s
                .base
                .as_ref()
                .map(|b| type_width(b, &empty_env, fns, &structs, &HashMap::new()))
                .unwrap_or(0);
            (w > 0).then_some((name.clone(), w))
        })
        .collect()
}

/// The vector families declared across the loaded modules, used to recognize
/// a library newtype over an array (`unsigned`, `signed`, or a user
/// equivalent) as a vector rather than a plain aggregate.
pub fn array_families(
    modules: &[Module],
    fns: &FunctionIndex<'_>,
) -> std::collections::HashSet<String> {
    let declarations =
        crate::resolve::declarations::DeclarationIndex::collect(fns.resolved(), modules);
    declarations
        .structures()
        .filter(|(_, structure)| {
            structure.fields.is_empty()
                && structure
                    .base
                    .as_ref()
                    .and_then(|base| declarations.array_representation(base))
                    .is_some()
        })
        .filter_map(|(owner, _)| fns.nominal_type_key(owner))
        .collect()
}

/// Every enum's `variant -> discriminant` map, *including inherited variants*
/// from a derivation base (`enum Extended : Base` gets Base's variants too).
/// Consumers (runner, native emitter) share this so derived-enum variant
/// references resolve identically.
pub fn enum_discriminants(
    modules: &[Module],
    fns: &FunctionIndex<'_>,
) -> HashMap<String, HashMap<String, u64>> {
    let declarations =
        crate::resolve::declarations::DeclarationIndex::collect(fns.resolved(), modules);
    let values = crate::types::enum_values::EnumValues::collect_with_functions(
        &declarations,
        Some(fns.declarations()),
    );
    enum_discriminants_from_values(&values, fns.resolved(), fns)
}

/// Every enum's first-variant discriminant — the derived `new()` default.
/// Keep declaration order (base chain first), not the smallest numeric value.
pub fn enum_first_discriminants(
    modules: &[Module],
    fns: &FunctionIndex<'_>,
) -> HashMap<String, u64> {
    let declarations =
        crate::resolve::declarations::DeclarationIndex::collect(fns.resolved(), modules);
    let values = crate::types::enum_values::EnumValues::collect_with_functions(
        &declarations,
        Some(fns.declarations()),
    );
    enum_first_from_values(&values, fns)
}

/// Serialize resolved phase facts only at the legacy public IR boundary.
pub(in crate::ir) fn enum_discriminants_from_values(
    values: &crate::types::enum_values::EnumValues,
    resolved: &Resolved,
    fns: &FunctionIndex<'_>,
) -> HashMap<String, HashMap<String, u64>> {
    values
        .owners()
        .filter_map(|owner| {
            let key = fns.enum_id_key(owner)?;
            let members = values
                .members(owner)?
                .iter()
                .filter_map(|(variant, _)| {
                    Some((
                        resolved.def(*variant)?.name.clone(),
                        values.value(owner, *variant)?,
                    ))
                })
                .collect();
            Some((key, members))
        })
        .collect()
}

pub(in crate::ir) fn enum_first_from_values(
    values: &crate::types::enum_values::EnumValues,
    fns: &FunctionIndex<'_>,
) -> HashMap<String, u64> {
    values
        .owners()
        .filter_map(|owner| {
            let first = values.members(owner)?.first()?.0;
            Some((fns.enum_id_key(owner)?, values.value(owner, first)?))
        })
        .collect()
}
