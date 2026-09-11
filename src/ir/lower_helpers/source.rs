//! Source access paths, type classification, and file initializer helpers.

use super::*;

/// `p'old.valid` -> `p.valid'old`, `xs'old[0]` -> `xs[0]'old`. Only the two
/// value primitives move: `'length` and the range bounds describe the whole
/// aggregate, so pushing them at a leaf would change what is asked.
pub(in crate::ir) fn sunk_sysattr(e: &ast::Expr) -> Option<ast::Expr> {
    let (base, rebuild): (&ast::Expr, &dyn Fn(Box<ast::Expr>) -> ast::Expr) = match e {
        ast::Expr::Field { base, field, span } => (base, &|inner| ast::Expr::Field {
            base: inner,
            field: field.clone(),
            span: *span,
        }),
        ast::Expr::Index { base, index, span } => (base, &|inner| ast::Expr::Index {
            base: inner,
            index: index.clone(),
            span: *span,
        }),
        _ => return None,
    };
    let ast::Expr::SysAttr {
        base: inner,
        attr,
        span,
    } = base
    else {
        return None;
    };
    if attr.text != "old" && attr.text != "event" {
        return None;
    }
    Some(ast::Expr::SysAttr {
        base: Box::new(rebuild(inner.clone())),
        attr: attr.clone(),
        span: *span,
    })
}

/// The dotted signal path of a name, struct-field, or constant-index access:
/// `s` -> `"s"`, `s.data` -> `"s.data"`, `a[2]` -> `"a[2]"`. A dynamic index or
/// anything else (calls, slices) yields `None`.
pub(in crate::ir) fn expr_path(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        ast::Expr::Field { base, field, .. } => {
            Some(format!("{}.{}", expr_path(base)?, field.text))
        }
        ast::Expr::Index { base, index, .. } => match index.as_ref() {
            ast::Expr::Int { text, .. } => {
                Some(format!("{}[{}]", expr_path(base)?, parse_int(text)?))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Split a flattened aggregate access into its root name and ordered field /
/// index suffixes. Unlike `expr_path`, indices remain as expressions so a
/// runtime access can be expanded over the concrete leaf paths.
pub(in crate::ir) fn access_steps(e: &ast::Expr) -> Option<(String, Vec<AccessStep<'_>>)> {
    /// Walk a nested access into its ordered steps, returning the base path.
    fn walk<'e>(e: &'e ast::Expr, steps: &mut Vec<AccessStep<'e>>) -> Option<String> {
        match e {
            ast::Expr::Path(path) if path.segments.len() == 1 => {
                Some(path.segments[0].text.clone())
            }
            ast::Expr::Field { base, field, .. } => {
                let root = walk(base, steps)?;
                steps.push(AccessStep::Field(&field.text));
                Some(root)
            }
            ast::Expr::Index { base, index, .. } => {
                let root = walk(base, steps)?;
                steps.push(AccessStep::Index(index));
                Some(root)
            }
            _ => None,
        }
    }

    let mut steps = Vec::new();
    let root = walk(e, &mut steps)?;
    Some((root, steps))
}

/// The `(element type, length)` if `ty` is an ordinary indexed array rather
/// than the first constraint on a nominal array family (`unsigned[8]`).
/// The element type and **ordered element indices** of an array type.
/// A width-only index (`Bit[4]`) is ascending `0..=3`; a range keeps its
/// written direction (`Logic[7..0]` yields 7,6,...,0). A single-segment path
/// as the index may name a range constant.
pub(in crate::ir) fn array_of<'t>(
    ty: &'t ast::Type,
    env: &HashMap<String, i64>,
    const_ranges: &HashMap<String, (i64, i64)>,
    families: &std::collections::HashSet<String>,
    fns: &FunctionIndex<'_>,
) -> Option<(&'t ast::Type, Vec<i64>)> {
    let ast::Type::Indexed {
        base,
        index: Some(index),
        ..
    } = ty
    else {
        return None;
    };
    // A scalar nominal array family (unsigned/signed/user) `F[N]` is one packed
    // N-element value, but only when the base is directly the family. An
    // already constrained base (`unsigned[8][4]`) is an array of four words.
    let base_is_family = matches!(base.as_ref(), ast::Type::Path(_))
        && fns
            .type_head_key(base)
            .is_some_and(|head| families.contains(&head));
    if is_int_type(base) || base_is_family {
        return None;
    }
    let bounds = match index.as_ref() {
        ast::Expr::Range { lo, hi, .. } => Some((
            eval_const_fns(lo, env, fns, 0)?,
            eval_const_fns(hi, env, fns, 0)?,
        )),
        ast::Expr::Path(path) => fns
            .constant_path_key(path)
            .and_then(|key| const_ranges.get(&key).copied()),
        _ => None,
    };
    let indices = match bounds {
        Some((a, b)) if a <= b => (a..=b).collect(),
        Some((a, b)) => (b..=a).rev().collect(),
        None => (0..eval_const_fns(index, env, fns, 0).unwrap_or(0).max(0)).collect(),
    };
    Some((base, indices))
}

/// The kernel `integer` scalar (a bare word). Unsigned and signed are nominal
/// array families recognized structurally, not by name.
pub(in crate::ir) fn is_int_type(ty: &ast::Type) -> bool {
    matches!(ty, ast::Type::Path(p)
        if p.segments.last().map(|s| s.text.as_str()) == Some("integer"))
}

/// Build `enum name -> bit width`: the `repr` width if given (`enum S: unsigned[2]`),
/// else the bits needed for the variant count.
pub(in crate::ir) fn enum_reprs(
    modules: &[Module],
    fns: &FunctionIndex<'_>,
) -> HashMap<String, u32> {
    let empty = HashMap::new();
    let enums = enum_index(modules, fns);
    let mut out = HashMap::new();
    for (name, e) in &enums {
        // A numeric `: repr` sets the width explicitly; otherwise the width is
        // derived, and must hold every *value* the enum can take — not just
        // one code per variant. An explicit discriminant can sit far above the
        // ordinal range (`enum Code { Lo = 1, Hi = 9 }` is two variants but
        // needs four bits), so the larger of the two bounds wins.
        let w = if let Some(repr) = e
            .repr
            .as_ref()
            .filter(|_| enum_base_name(e, &enums, fns).is_none())
        {
            type_width(repr, &empty, fns, &HashMap::new(), &HashMap::new())
        } else {
            let variants = effective_variants(name, &enums, fns, &mut Vec::new());
            let n = variants.len().max(1) as u32;
            let count_bits = if n <= 1 {
                1
            } else {
                u32::BITS - (n - 1).leading_zeros()
            };
            let max_disc = variants.iter().filter_map(|(_, d)| *d).max().unwrap_or(0);
            let disc_bits = if max_disc <= 0 {
                1
            } else {
                u64::BITS - (max_disc as u64).leading_zeros()
            };
            count_bits.max(disc_bits)
        };
        out.insert(name.clone(), w);
    }
    out
}

/// Whether an entity carries the canonical `std::attrs::test` attribute.
pub(in crate::ir) fn is_test_entity(e: &ast::EntityDecl, resolved: &Resolved) -> bool {
    e.attrs
        .iter()
        .any(|attribute| crate::resolve::is_enabled_std_test_attribute(resolved, attribute))
}

/// The leading name of a type expression.
pub(in crate::ir) fn type_head_name(t: &ast::Type) -> Option<&str> {
    match t {
        ast::Type::Path(p) => p.segments.first().map(|s| s.text.as_str()),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => type_head_name(base),
        ast::Type::View { view, .. } => view.segments.last().map(|s| s.text.as_str()),
    }
}

/// The definition a type expression names.
pub(in crate::ir) fn type_def_id(ty: &ast::Type, resolved: &Resolved) -> Option<DefId> {
    match ty {
        ast::Type::Path(path) => resolved.resolved(path.span),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => {
            type_def_id(base, resolved)
        }
        ast::Type::View { view, .. } => resolved.resolved(view.span),
    }
}

/// Whether an impl is a blanket one over any array.
pub(in crate::ir) fn is_blanket_array_impl(im: &ast::ImplDecl) -> bool {
    let ast::Type::Indexed {
        base, index: None, ..
    } = &im.target
    else {
        return false;
    };
    let Some(head) = type_head_name(base) else {
        return false;
    };
    im.params.params.iter().any(|param| param.name.text == head)
}

/// The trait a blanket array impl requires of its element type.
pub(in crate::ir) fn blanket_requirement(
    im: &ast::ImplDecl,
    fns: &FunctionIndex<'_>,
) -> Option<String> {
    let ast::Type::Indexed { base, .. } = &im.target else {
        return None;
    };
    let parameter = type_head_name(base)?;
    let bound = im
        .params
        .params
        .iter()
        .find(|candidate| candidate.name.text == parameter)?
        .bound
        .as_ref()?;
    let trait_key = match bound {
        ast::Type::Path(path) => fns.trait_path_key(path),
        ast::Type::Generic { base, .. } => match base.as_ref() {
            ast::Type::Path(path) => fns.trait_path_key(path),
            _ => None,
        },
        _ => None,
    }?;
    match bound {
        ast::Type::Generic { args, .. } if trait_key == "Operator" => {
            args.first().and_then(|argument| match argument {
                ast::GenericArg::Positional(ast::Expr::StrLit { text, .. }) => Some(text.clone()),
                _ => None,
            })
        }
        _ => Some(trait_key),
    }
}

/// Pack one little-endian file integer into the compiler's ABI-word vector.
///
/// File integers use exactly `ceil(width / 8)` bytes. Missing bytes are zero,
/// and padding bits in the final byte never escape the declared type width.
pub(in crate::ir) fn file_integer_words(
    bytes: &[u8],
    offset: usize,
    byte_count: usize,
    width: u32,
) -> Vec<u64> {
    let word_count = width.max(1).div_ceil(64) as usize;
    let mut words = vec![0; word_count];
    for byte_index in 0..byte_count {
        let Some(index) = offset.checked_add(byte_index) else {
            break;
        };
        let Some(&byte) = bytes.get(index) else {
            break;
        };
        let bit = byte_index * 8;
        let word = bit / 64;
        if let Some(destination) = words.get_mut(word) {
            *destination |= u64::from(byte) << (bit % 64);
        }
    }
    if let Some(last) = words.last_mut() {
        let used = width % 64;
        if used != 0 {
            *last &= (1_u64 << used) - 1;
        }
    }
    words
}
