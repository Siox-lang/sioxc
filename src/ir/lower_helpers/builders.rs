//! IR expression builders, widths, and constant evaluation.

use super::*;
use crate::types::consteval::{self, Binding, Environment, IntegerDomain};

/// Whether a pattern matches everything, including a `_` written inside an
/// alternation (`A | _`).
pub(in crate::ir) fn pattern_has_wildcard(p: &ast::Pattern) -> bool {
    match p {
        ast::Pattern::Wildcard => true,
        ast::Pattern::Or { alts, .. } => alts.iter().any(pattern_has_wildcard),
        _ => false,
    }
}

// --- helpers ----------------------------------------------------------------

/// Whether an expression depends on a `::event`-family system attribute, which
/// makes an enclosing `if` an event-controlled block (spec 3.11).
pub(in crate::ir) fn expr_is_event(e: &ast::Expr) -> bool {
    match e {
        ast::Expr::SysAttr { base, attr, .. } => {
            // `::event` is the primitive that makes an `if` sequential; the edge
            // helpers are the `ClockLike` methods (handled by the Call arm).
            attr.text == "event" || expr_is_event(base)
        }
        // A `ClockLike` edge method (`clk.rising()`, `clk.falling()`,
        // `clk.edge()`) depends on `::event`, so it makes an `if` sequential.
        ast::Expr::Call { callee, .. } => match callee.as_ref() {
            ast::Expr::Field { field, .. } => {
                matches!(field.text.as_str(), "rising" | "falling" | "edge")
            }
            _ => false,
        },
        ast::Expr::Unary { rhs, .. } => expr_is_event(rhs),
        ast::Expr::Binary { lhs, rhs, .. } => expr_is_event(lhs) || expr_is_event(rhs),
        ast::Expr::Field { base, .. } | ast::Expr::Index { base, .. } => expr_is_event(base),
        _ => false,
    }
}

/// Convert an AST infix operator into its IR form, or `None` when it is
/// library-defined and must be inlined instead.
pub(in crate::ir) fn lower_binop(op: AstBinOp) -> Option<BinOp> {
    Some(match op {
        AstBinOp::Add => BinOp::Add,
        AstBinOp::Sub => BinOp::Sub,
        AstBinOp::Mul => BinOp::Mul,
        AstBinOp::Div => BinOp::Div,
        AstBinOp::Rem => BinOp::Rem,
        AstBinOp::And => BinOp::And,
        AstBinOp::Or => BinOp::Or,
        AstBinOp::Custom { .. } => return None,
        AstBinOp::Shl => BinOp::Shl,
        AstBinOp::Shr => BinOp::Shr,
        AstBinOp::Eq => BinOp::Eq,
        AstBinOp::Ne => BinOp::Ne,
        AstBinOp::Lt => BinOp::Lt,
        AstBinOp::Le => BinOp::Le,
        AstBinOp::Gt => BinOp::Gt,
        AstBinOp::Ge => BinOp::Ge,
    })
}

/// Parse an integer literal that fits one word, honouring radix prefixes.
pub(in crate::ir) fn parse_int(text: &str) -> Option<u64> {
    let normalized = text.trim().replace('_', "");
    let t = normalized.as_str();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        u64::from_str_radix(b, 2).ok()
    } else {
        t.parse().ok()
    }
}

/// An integer literal as a constant expression, widening to a multi-word
/// constant when it does not fit one word.
pub(in crate::ir) fn integer_const(text: &str) -> Option<Expr> {
    let text = text.trim().replace('_', "");
    let (radix, digits) =
        if let Some(digits) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            (16, digits)
        } else if let Some(digits) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
            (2, digits)
        } else {
            (10, text.as_str())
        };
    let mut words = Vec::<u64>::new();
    for digit in digits.chars() {
        let digit = digit.to_digit(radix)? as u64;
        let mut carry = digit as u128;
        for word in &mut words {
            let next = (*word as u128) * radix as u128 + carry;
            *word = next as u64;
            carry = next >> 64;
        }
        if carry != 0 || words.is_empty() {
            words.push(carry as u64);
        }
    }
    while words.last() == Some(&0) && words.len() > 1 {
        words.pop();
    }
    Some(words_const(words))
}

/// A constant from low-word-first words, narrowing to [`Expr::Const`] when
/// one word suffices.
pub(in crate::ir) fn words_const(mut words: Vec<u64>) -> Expr {
    while words.last() == Some(&0) && words.len() > 1 {
        words.pop();
    }
    match words.as_slice() {
        [] => Expr::Const(0),
        [word] => Expr::Const(*word),
        _ => Expr::WideConst(words),
    }
}

/// Bit width from a type annotation, substituting parameters from `env` (so
/// `unsigned[W]` with `W=8` is width 8). `0` means parametric / not yet known.
pub(in crate::ir) fn source_type_span(ty: &ast::Type) -> crate::diag::Span {
    match ty {
        ast::Type::Path(path) => path.span,
        ast::Type::Indexed { span, .. }
        | ast::Type::Generic { span, .. }
        | ast::Type::View { span, .. }
        | ast::Type::Tuple { span, .. } => *span,
    }
}

/// The bit width of a declared type in `env`.
pub(in crate::ir) fn type_width(
    t: &ast::Type,
    env: &HashMap<String, i64>,
    fns: &FunctionIndex<'_>,
    structs: &HashMap<String, &ast::StructDecl>,
    ranges: &HashMap<String, (i64, i64)>,
) -> u32 {
    type_width_at(t, env, fns, structs, ranges, &mut HashSet::new())
}

/// `type_width` with cycle detection. A cyclic derivation
/// (`struct A : B` / `struct B : A`) is reported by resolve, but lowering runs
/// anyway best-effort.
pub(in crate::ir) fn type_width_at(
    t: &ast::Type,
    env: &HashMap<String, i64>,
    fns: &FunctionIndex<'_>,
    structs: &HashMap<String, &ast::StructDecl>,
    ranges: &HashMap<String, (i64, i64)>,
    seen: &mut HashSet<String>,
) -> u32 {
    match t {
        ast::Type::Tuple { elems, .. } => elems.iter().fold(0u32, |total, elem| {
            total.saturating_add(type_width_at(elem, env, fns, structs, ranges, seen))
        }),
        ast::Type::Path(_) => match fns.type_head_key(t).as_deref() {
            Some("integer") | Some("natural") | Some("real") => 64, // native kernel word / f64 bits
            Some("Char") => 32, // symbol storage (implementation detail)
            // A derived type inherits its base array's size/range: `struct Byte
            // : Logic[8]` is 8 bits, `struct Word : unsigned[16]` is 16 (spec:
            // nominal derivation reuses the base representation).
            Some(name) => {
                if !seen.insert(name.to_string()) {
                    return 0;
                }
                let width = structs
                    .get(name)
                    .and_then(|s| s.base.as_ref())
                    .map(|b| type_width_at(b, env, fns, structs, ranges, seen))
                    .unwrap_or(0);
                seen.remove(name);
                width
            }
            None => 0,
        },
        // For `unsigned[8]` the index is the width; for `Logic[31..0]` it is the
        // span; unconstrained `T[]` stays width 0 ("set at use").
        ast::Type::Indexed { index: None, .. } => 0,
        ast::Type::Indexed {
            index: Some(index), ..
        } => match index.as_ref() {
            ast::Expr::Range { lo, hi, .. } => {
                match (
                    eval_const_fns(lo, env, fns, 0),
                    eval_const_fns(hi, env, fns, 0),
                ) {
                    (Some(a), Some(b)) => {
                        u32::try_from((i128::from(a) - i128::from(b)).unsigned_abs())
                            .ok()
                            .and_then(|width| width.checked_add(1))
                            .unwrap_or(0)
                    }
                    _ => 0,
                }
            }
            // A *range* constant used as the index (`unsigned[SPAN]` where
            // `const SPAN: range = 7..0`) states a span, not a width. Falling
            // through to the integer path found no integer and produced a
            // zero-width signal in silence — the literal `unsigned[7..0]`
            // spelling of the same thing was eight bits.
            ast::Expr::Path(path)
                if fns
                    .constant_path_key(path)
                    .is_some_and(|key| ranges.contains_key(&key)) =>
            {
                let key = fns.constant_path_key(path).expect("guarded range key");
                let (a, b) = ranges[&key];
                u32::try_from((i128::from(a) - i128::from(b)).unsigned_abs())
                    .ok()
                    .and_then(|width| width.checked_add(1))
                    .unwrap_or(0)
            }
            e => eval_const_fns(e, env, fns, 0)
                .map(|v| v.max(0) as u32)
                .unwrap_or(0),
        },
        // `uf<8>` for `struct uf<N: integer>(Logic[N])`: the struct's base is
        // sized by its own parameters, so bind them to the arguments first.
        ast::Type::Generic { base, args, .. } => {
            let name = fns.type_head_key(base);
            let declaration = name.as_deref().and_then(|name| structs.get(name));
            match (name, declaration) {
                (Some(name), Some(declaration))
                    if !declaration.params.params.is_empty() && declaration.base.is_some() =>
                {
                    if !seen.insert(name.clone()) {
                        return 0;
                    }
                    let inner = bind_generic_values(&declaration.params, args, env, fns);
                    let width = declaration
                        .base
                        .as_ref()
                        .map(|b| type_width_at(b, &inner, fns, structs, ranges, seen))
                        .unwrap_or(0);
                    seen.remove(&name);
                    width
                }
                _ => type_width(base, env, fns, structs, ranges),
            }
        }
        ast::Type::View { target: base, .. } => type_width(base, env, fns, structs, ranges),
    }
}

/// `env` extended with a generic declaration's value parameters bound to the
/// constant arguments of one application (`<8>`, `<N = 8>`).
fn bind_generic_values(
    params: &ast::Params,
    args: &[ast::GenericArg],
    env: &HashMap<String, i64>,
    fns: &FunctionIndex<'_>,
) -> HashMap<String, i64> {
    let mut inner = env.clone();
    for (index, arg) in args.iter().enumerate() {
        let (name, value) = match arg {
            ast::GenericArg::Positional(value) => match params.params.get(index) {
                Some(param) => (param.name.text.as_str(), value),
                None => continue,
            },
            ast::GenericArg::Named { name, value } => (name.text.as_str(), value),
            _ => continue,
        };
        if let Some(value) = eval_const_fns(value, env, fns, 0) {
            inner.insert(name.to_string(), value);
        }
    }
    inner
}

/// Whether a field-less nominal struct ultimately derives from `kernel`.
/// Representation follows the declared base chain; names such as `time` and
/// `frequency` are not special to the compiler.
pub(in crate::ir) fn struct_derives_kernel(
    name: &str,
    kernel: &str,
    structs: &HashMap<String, &ast::StructDecl>,
    fns: &FunctionIndex<'_>,
) -> bool {
    if name == kernel {
        return true;
    }
    let mut current = name.to_string();
    let mut seen = HashSet::new();
    while seen.insert(current.clone()) {
        let Some(st) = structs.get(&current) else {
            return false;
        };
        if !st.fields.is_empty() {
            return false;
        }
        let Some(base) = st.base.as_ref().and_then(|ty| fns.type_head_key(ty)) else {
            return false;
        };
        if base == kernel {
            return true;
        }
        current = base;
    }
    false
}

/// Const-evaluate a width expression against a parameter environment.
pub(in crate::ir) fn eval_const(e: &ast::Expr, env: &HashMap<String, i64>) -> Option<i64> {
    let resolved = Resolved::default();
    let fns = FunctionIndex::new(&resolved);
    eval_const_fns(e, env, &fns, 0)
}

/// Widths, constants and const-fn calls use the shared interpreter.
pub fn eval_const_fns(
    e: &ast::Expr,
    env: &HashMap<String, i64>,
    fns: &FunctionIndex<'_>,
    depth: u32,
) -> Option<i64> {
    consteval::evaluate(e, &WordConstants { env, fns }, depth)
        .ok()
        .and_then(|value| i64::try_from(value).ok())
}

/// Evaluate a pure function body through the shared constant interpreter.
pub fn eval_const_stmts(
    stmts: &[ast::Stmt],
    env: &HashMap<String, i64>,
    fns: &FunctionIndex<'_>,
    depth: u32,
) -> Option<i64> {
    consteval::evaluate_statements(stmts, &WordConstants { env, fns }, depth)
        .ok()
        .flatten()
        .and_then(|value| i64::try_from(value).ok())
}

struct WordConstants<'a, 'r> {
    env: &'a HashMap<String, i64>,
    fns: &'a FunctionIndex<'r>,
}

impl Environment for WordConstants<'_, '_> {
    fn domain(&self) -> IntegerDomain {
        IntegerDomain::NativeWord
    }

    fn literal(&self, text: &str) -> Option<i128> {
        parse_int(text).map(|value| i128::from(value as i64))
    }

    fn binding(&self, path: &ast::Path) -> Option<Binding<'_>> {
        self.fns
            .constant_path_key(path)
            .and_then(|key| self.env.get(&key))
            .map(|value| {
                Binding::Value(crate::types::consteval::Value::Integer(i128::from(*value)))
            })
            .or_else(|| {
                self.fns
                    .declarations()
                    .integer_constant_path(path)
                    .map(|declaration| Binding::Expression(&declaration.value))
            })
    }

    fn local_name<'p>(&self, path: &'p ast::Path) -> Option<&'p str> {
        self.fns.declarations().local_name(path)
    }

    fn attribute(&self, base: &ast::Expr, attr: &ast::Ident) -> Option<i128> {
        let ast::Expr::Path(path) = base else {
            return None;
        };
        let [name] = path.segments.as_slice() else {
            return None;
        };
        self.env
            .get(&format!("{}::{}", name.text, attr.text))
            .copied()
            .map(i128::from)
    }

    fn function(&self, callee: &ast::Expr) -> Option<&ast::FnDecl> {
        self.fns.get(callee)
    }

    fn transparent_conversion(&self, callee: &ast::Expr) -> bool {
        matches!(call_fn_key(callee).as_deref(), Some("integer" | "Char"))
    }

    fn local_key<'p>(&self, path: &'p ast::Path) -> Option<consteval::LocalKey<'p>> {
        self.fns
            .declarations()
            .local_id(path)
            .map(consteval::LocalKey::Definition)
    }

    fn parameter_key<'p>(
        &self,
        parameter: &'p crate::syntax::ast::FnParam,
    ) -> Option<consteval::LocalKey<'p>> {
        self.fns
            .declarations()
            .parameter_id(parameter)
            .map(consteval::LocalKey::Definition)
    }

    // shortcut: retain legacy scalar predicates until typed source-owned
    // operator/condition evaluation replaces raw integer binding environments.
    fn unary(&self, expression: &ast::Expr, value: i128) -> Option<i128> {
        match expression {
            ast::Expr::Unary {
                op: ast::UnOp::Not, ..
            } => Some((value == 0) as i128),
            _ => None,
        }
    }

    fn binary(&self, expression: &ast::Expr, a: i128, b: i128) -> Option<i128> {
        let ast::Expr::Binary { op, .. } = expression else {
            return None;
        };
        Some(match op {
            ast::BinOp::Eq => (a == b) as i128,
            ast::BinOp::Ne => (a != b) as i128,
            ast::BinOp::Lt => (a < b) as i128,
            ast::BinOp::Le => (a <= b) as i128,
            ast::BinOp::Gt => (a > b) as i128,
            ast::BinOp::Ge => (a >= b) as i128,
            ast::BinOp::And => (a != 0 && b != 0) as i128,
            ast::BinOp::Or => (a != 0 || b != 0) as i128,
            _ => return None,
        })
    }

    fn condition(&self, value: i128) -> Option<bool> {
        Some(value != 0)
    }
}
