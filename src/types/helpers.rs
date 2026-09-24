//! Free helper functions over AST patterns, types, and literals.

use super::*;

/// The base name of a type (`Counter<W>` -> `Counter`, `out S::Source` -> `S`).
/// Append the value ranges a numeric pattern covers. Returns false when the
/// pattern's coverage is not expressible as intervals — a bit pattern's
/// don't-care bits scatter across the domain — so the caller can step aside
/// instead of reporting a hole it cannot see.
pub(super) fn collect_pattern_ranges(p: &Pattern, out: &mut Vec<(i128, i128)>) -> bool {
    match p {
        Pattern::Wildcard => {
            out.push((i128::MIN, i128::MAX));
            true
        }
        Pattern::Range { lo, hi, .. } => {
            let (lo, hi) = (i128::from(*lo), i128::from(*hi));
            // `3..0` is written descending in some sources; a range covers the
            // same values either way.
            out.push((lo.min(hi), lo.max(hi)));
            true
        }
        Pattern::Or { alts, .. } => alts.iter().all(|a| collect_pattern_ranges(a, out)),
        // An enum path against a numeric scrutinee is a type error reported
        // elsewhere; a bit pattern is not an interval.
        Pattern::Path(_) | Pattern::BitPattern { .. } | Pattern::CharLit { .. } => false,
    }
}

/// Every character pattern in a pattern tree, flattening or-patterns.
pub(super) fn collect_char_patterns(p: &Pattern, out: &mut Vec<(char, Span)>) {
    match p {
        Pattern::CharLit { ch, span } => out.push((*ch, *span)),
        Pattern::Or { alts, .. } => {
            for a in alts {
                collect_char_patterns(a, out);
            }
        }
        _ => {}
    }
}

/// A pattern's covered enum-variant names and whether it contains a wildcard,
/// flattening or-patterns (`A | B` covers both; `A | _` is a wildcard).
pub(super) fn pattern_covers(p: &Pattern) -> (Vec<String>, bool) {
    match p {
        Pattern::Wildcard => (Vec::new(), true),
        Pattern::Path(pp) if pp.segments.len() >= 2 => (vec![pp.segments[1].text.clone()], false),
        // A char-valued enum declares its variants as character literals
        // (`enum Logic { '0', '1', … }`), so the pattern names one directly.
        Pattern::CharLit { ch, .. } => (vec![format!("'{ch}'")], false),
        Pattern::Or { alts, .. } => {
            let mut vars = Vec::new();
            let mut wild = false;
            for a in alts {
                let (v, w) = pattern_covers(a);
                vars.extend(v);
                wild |= w;
            }
            (vars, wild)
        }
        _ => (Vec::new(), false),
    }
}

/// The span of a type's head name segment (for resolving its definition).
pub(super) fn type_head_span(ty: &Type) -> Option<Span> {
    match ty {
        Type::Path(p) => p.segments.first().map(|s| s.span),
        Type::Generic { base, .. } | Type::Indexed { base, .. } => type_head_span(base),
        Type::View { view, .. } => Some(view.span),
    }
}

/// The leading name of a type expression.
pub(super) fn type_head_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Path(p) => p.segments.first().map(|s| s.text.as_str()),
        Type::Generic { base, .. } | Type::Indexed { base, .. } => type_head_name(base),
        Type::View { view, .. } => view.segments.last().map(|i| i.text.as_str()),
    }
}

/// Requested construction type of a `read<T>(path)` expression.
pub(super) fn read_call_type(expression: &Expr) -> Option<&Type> {
    let Expr::Call {
        callee, type_args, ..
    } = expression
    else {
        return None;
    };
    let Expr::Path(path) = callee.as_ref() else {
        return None;
    };
    (path.segments.len() == 1 && path.segments[0].text == "read" && type_args.len() == 1)
        .then(|| &type_args[0])
}

/// A dotted path string for a write target: `Expr::Path` or a `Field` chain
/// (`bus.ready` -> "bus.ready").
pub(super) fn path_string(e: &Expr) -> Option<String> {
    match e {
        Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        Expr::Field { base, field, .. } => Some(format!("{}.{}", path_string(base)?, field.text)),
        _ => None,
    }
}

/// The leftmost identifier of a field/index access chain (`bus.ready` -> `bus`,
/// `a[3]` -> `a`, `p.f.g` -> `p`), for the plain-input-port write check.
pub(super) fn target_root_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        Expr::Field { base, .. } | Expr::Index { base, .. } => target_root_name(base),
        _ => None,
    }
}

/// Port-direction facts for the write-to-input check within one impl.
#[derive(Clone, Default)]
pub(super) struct PortDirs {
    /// Names whose write is illegal exactly: a bare `in` port, or an `in`
    /// bus-mode leaf (`bus.ready`).
    pub(super) illegal: HashSet<String>,
    /// Plain (non-bus-mode) `in` ports — writing *any* field/index of one is
    /// illegal too (it has no writable parts).
    pub(super) plain_in_roots: HashSet<String>,
    /// `const`s declared in this impl. A write to one is not storage at all,
    /// and reached the emitter as "unknown signal `K`" — a message naming
    /// something the author had in fact declared.
    pub(super) consts: HashSet<String>,
}

/// The type `self` has inside an impl: the backing struct for a view-applied
/// target (`impl Bus BusOut` -> `Bus`), the target itself otherwise.
pub(super) fn self_ty(im: &ImplDecl) -> &Type {
    match &im.target {
        Type::View { target, .. } => target,
        other => other,
    }
}

/// Whether an expression is exactly the `self` receiver.
pub(super) fn is_self_value(expression: &Expr) -> bool {
    matches!(
        expression,
        Expr::Path(path)
            if path.segments.len() == 1 && path.segments[0].text == "self"
    )
}

/// Whether an impl is a blanket one over any array, as in
/// `impl<T: Op> Op for T[]`.
pub(super) fn is_blanket_array_impl(im: &ImplDecl) -> bool {
    let Type::Indexed {
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

/// Keys whose element-wise array forwarding lowering can perform. std may
/// declare a blanket `for T[]` impl only for these; the list had `and`/`or`/
/// `not` and not the rest of the logic family, so `Logic[3] xor Logic[3]` had
/// no implementation at all while `and` on the same operands worked.
pub(super) fn is_liftable_array_key(key: &str) -> bool {
    matches!(
        key,
        "Resolve" | "and" | "or" | "not" | "xor" | "nand" | "nor" | "xnor"
    )
}

/// Width of a bracketed type index when it is a literal (`unsigned[8]` -> 8);
/// otherwise `0`, meaning "parametric / not yet known".
pub(super) fn width_of(index: &Expr) -> u32 {
    // A declared range states a length too: `Bit[3..0]` is four elements, in
    // either direction. Without this it measured 0, so a range-declared array
    // rejected an array-literal initializer as `Bit[0]` — while the same type
    // took a *string* literal, which is sized from the literal instead.
    if let Expr::Range { lo, hi, .. } = index {
        if let (Some(lo), Some(hi)) = (signed_lit(lo), signed_lit(hi)) {
            // A range wide enough to overflow is unrepresentable anyway; 0
            // keeps it "not yet known", which is what rejects it later.
            return hi
                .checked_sub(lo)
                .and_then(i64::checked_abs)
                .and_then(|len| len.checked_add(1))
                .and_then(|len| u32::try_from(len).ok())
                .unwrap_or(0);
        }
    }
    signed_lit(index)
        .and_then(|width| u32::try_from(width).ok())
        .unwrap_or(0)
}

/// Whether an operator is one of the six comparisons, which yield `Bool`
/// rather than their operands' family.
pub(super) fn is_comparison(op: &BinOp) -> bool {
    matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
    )
}

/// Whether a value of type `rhs` may be assigned to `lhs` with no conversion.
/// A width of `0` is "not yet known" (parametric) and assumed compatible — the
/// concrete width check happens after elaboration.
pub(super) fn compatible(lhs: &Ty, rhs: &Ty) -> bool {
    use Ty::*;
    if matches!(lhs, Error) || matches!(rhs, Error) {
        return true;
    }
    match (lhs, rhs) {
        (Real, Real) | (Char, Char) => true,
        // `integer` is the number kernel; it coerces to/from any bit vector
        // (a unsigned[8] accepts `42`, and a vector's value is an integer).
        (Integer, Integer) => true,
        (
            Integer,
            Array {
                family: Some(_), ..
            },
        )
        | (
            Array {
                family: Some(_), ..
            },
            Integer,
        ) => true,
        (Named(a), Named(b)) => a == b,
        // All indexed collections use the same shape. Nominal families do not
        // gate assignment; their behavior lives in trait implementations.
        (
            Array {
                elem: ea, len: la, ..
            },
            Array {
                elem: eb, len: lb, ..
            },
        ) => compatible(ea, eb) && (*la == 0 || *lb == 0 || la == lb),
        _ => false,
    }
}

/// When a string literal (`"c"`) is used where a character, logic scalar, or
/// bit vector is expected, explain that `"..."` is a *string* (a `Char` array)
/// and point at the right form: `'c'` for a single value, `b"..."` for a bit
/// vector. Assigning a string to a `Char` array is fine, so no hint there.
pub(super) fn strlit_help(lhs: &Ty, value: &Expr) -> Option<String> {
    let Expr::StrLit { text, .. } = value else {
        return None;
    };
    match lhs {
        Ty::Char | Ty::Named(_) => Some(if text.chars().count() == 1 {
            format!("`\"{text}\"` is a string; for a single {} value use a character literal `'{text}'`", ty_name(lhs))
        } else {
            format!("`\"{text}\"` is a string (a `Char` array); a {} is one character, written `'c'`", ty_name(lhs))
        }),
        Ty::Array {
            family: Some(_),
            ..
        } => Some(format!(
            "`\"{text}\"` is a string; for a bit vector use a bit-string literal `b\"{text}\"` (binary) or `x\"...\"` (hex)"
        )),
        Ty::Array { .. } => None,
        _ => None,
    }
}

/// Render a checked type the way the source spells it, for diagnostics.
pub(super) fn ty_name(t: &Ty) -> String {
    match t {
        Ty::Real => "real".to_string(),
        Ty::Integer => "integer".to_string(),
        Ty::Char => "Char".to_string(),
        Ty::Named(_) => "a named type".to_string(),
        Ty::Array {
            family: Some(name),
            len: 0,
            ..
        } => name.clone(),
        Ty::Array {
            family: Some(name),
            len,
            ..
        } => format!("{name}[{len}]"),
        Ty::Array { .. } => "an array".to_string(),
        Ty::Void => "no value".to_string(),
        Ty::Error => "<unknown>".to_string(),
    }
}

/// The value of an integer literal, allowing a leading unary minus.
/// Only a plain local name can be looked up; a field or nested index has no
/// entry, and is skipped rather than guessed at.
pub(super) fn declared_bounds_of(
    base: &Expr,
    bounds: &std::cell::RefCell<HashMap<String, (i64, i64)>>,
) -> Option<(i64, i64)> {
    let Expr::Path(p) = base else { return None };
    let [seg] = p.segments.as_slice() else {
        return None;
    };
    bounds.borrow().get(&seg.text).copied()
}

/// A signed integer literal's value, including a negative one.
pub(super) fn signed_lit(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int { text, .. } => i64::try_from(unsigned_lit_text(text)?).ok(),
        Expr::Unary {
            op: UnOp::Neg, rhs, ..
        } => match rhs.as_ref() {
            // Permit the full signed domain, including `-0x8000_0000_0000_0000`,
            // whose unsigned magnitude cannot first pass through i64.
            Expr::Int { text, .. } => i64::try_from(-i128::from(unsigned_lit_text(text)?)).ok(),
            _ => signed_lit(rhs)?.checked_neg(),
        },
        _ => None,
    }
}

/// An unsigned integer literal's value, honouring radix prefixes and `_`
/// separators.
pub(super) fn unsigned_lit_text(text: &str) -> Option<u64> {
    let text = text.replace('_', "");
    if let Some(digits) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(digits, 16).ok()
    } else if let Some(digits) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
        u64::from_str_radix(digits, 2).ok()
    } else {
        text.parse().ok()
    }
}

/// The element count an explicit range covers, when both bounds are
/// constant.
pub(super) fn explicit_range_len(e: &Expr) -> Option<u32> {
    let Expr::Range { lo, hi, .. } = e else {
        return None;
    };
    let lo = signed_lit(lo)?;
    let hi = signed_lit(hi)?;
    u32::try_from((i128::from(lo) - i128::from(hi)).unsigned_abs())
        .ok()?
        .checked_add(1)
}
