//! Derived widths, array families, and enum representation metadata.

use super::*;

/// Build `enum name -> variant name -> discriminant`. Explicit `= n` values are
/// honoured; unspecified variants continue from the previous discriminant + 1.
/// Index every enum declaration by name (for base-chain resolution).
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
    // A nominal newtype directly over `T[]` is an array family. Compute
    // inheritance to a fixpoint so `struct Byte(unsigned[8])` joins it too,
    // without a representation marker trait.
    let structs: Vec<&ast::StructDecl> = modules
        .iter()
        .flat_map(|m| &m.items)
        .filter_map(|it| match it {
            ast::Item::Struct(st) => Some(st),
            _ => None,
        })
        .collect();
    let mut out = std::collections::HashSet::new();
    loop {
        let mut changed = false;
        for st in &structs {
            let key = fns.struct_decl_key(&st.name);
            if !out.contains(&key) && is_array_family_struct(st, &out, fns) {
                out.insert(key);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    out
}

/// A nominal newtype directly over `T[]`, or deriving from an already-known
/// nominal array family, inherits the base array representation.
pub(in crate::ir) fn is_array_family_struct(
    st: &ast::StructDecl,
    families: &std::collections::HashSet<String>,
    fns: &FunctionIndex<'_>,
) -> bool {
    if !st.fields.is_empty() {
        return false;
    }
    match &st.base {
        Some(ast::Type::Indexed { .. }) => true,
        // A bare derived base (`struct Byte(unsigned)`) reuses the base family.
        Some(ast::Type::Path(p)) => fns
            .struct_path_key(p)
            .or_else(|| p.segments.last().map(|segment| segment.text.clone()))
            .is_some_and(|head| families.contains(&head)),
        _ => false,
    }
}

/// Index every enum's variants and discriminants across the modules.
pub(in crate::ir) fn enum_index<'a>(
    modules: &'a [Module],
    fns: &FunctionIndex<'_>,
) -> HashMap<String, &'a ast::EnumDecl> {
    let mut out = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Enum(e) = item {
                out.insert(fns.enum_decl_key(&e.name), e);
            }
        }
    }
    out
}

/// The `: Type` head name when it names another enum — i.e. a derivation
/// base rather than a numeric repr.
pub(in crate::ir) fn enum_base_name(
    e: &ast::EnumDecl,
    enums: &HashMap<String, &ast::EnumDecl>,
    fns: &FunctionIndex<'_>,
) -> Option<String> {
    let name = fns.type_head_key(e.repr.as_ref()?)?;
    enums.contains_key(&name).then_some(name)
}

/// An enum's effective variants, base chain first then its own declared ones
/// (spec: nominal derivation). `(name, explicit discriminant)`.
pub(in crate::ir) fn effective_variants(
    name: &str,
    enums: &HashMap<String, &ast::EnumDecl>,
    fns: &FunctionIndex<'_>,
    seen: &mut Vec<String>,
) -> Vec<(String, Option<i64>)> {
    let Some(e) = enums.get(name) else {
        return Vec::new();
    };
    if seen.iter().any(|n| n == name) {
        return Vec::new(); // cycle guard
    }
    seen.push(name.to_string());
    let mut out = match enum_base_name(e, enums, fns) {
        Some(base) => effective_variants(&base, enums, fns, seen),
        None => Vec::new(),
    };
    for v in &e.variants {
        let disc = match &v.value {
            Some(ast::Expr::Int { text, .. }) => parse_int(text).map(|n| n as i64),
            _ => None,
        };
        out.push((v.name.text.clone(), disc));
    }
    seen.pop();
    out
}

/// Every enum's `variant -> discriminant` map, *including inherited variants*
/// from a derivation base (`enum Extended : Base` gets Base's variants too).
/// Consumers (runner, native emitter) share this so derived-enum variant
/// references resolve identically.
pub fn enum_discriminants(
    modules: &[Module],
    fns: &FunctionIndex<'_>,
) -> HashMap<String, HashMap<String, u64>> {
    let enums = enum_index(modules, fns);
    let mut out = HashMap::new();
    for name in enums.keys() {
        let mut vars = HashMap::new();
        let mut next = 0u64;
        for (v, disc) in effective_variants(name, &enums, fns, &mut Vec::new()) {
            let d = disc.map(|d| d as u64).unwrap_or(next);
            vars.insert(v, d);
            next = d + 1;
        }
        out.insert(name.clone(), vars);
    }
    out
}

/// Every enum's first-variant discriminant — the derived `new()` default
/// (`T'LEFT`). Mirrors `enum_discriminants`' running-counter numbering but keeps
/// only the first (declaration-order, base chain first) variant's value, so an
/// enum whose first variant carries a non-zero `= n` still defaults to a valid
/// member rather than a bare `0`.
pub fn enum_first_discriminants(
    modules: &[Module],
    fns: &FunctionIndex<'_>,
) -> HashMap<String, u64> {
    let enums = enum_index(modules, fns);
    let mut out = HashMap::new();
    for name in enums.keys() {
        if let Some((_, disc)) = effective_variants(name, &enums, fns, &mut Vec::new()).first() {
            out.insert(name.clone(), disc.map(|d| d as u64).unwrap_or(0));
        }
    }
    out
}
