//! IR expression builders, widths, and constant evaluation.

use super::*;

/// Logical negation of a 0/1 expression.
pub(in crate::ir) fn not(e: Expr) -> Expr {
    Expr::Unary {
        op: UnOp::Not,
        rhs: Box::new(e),
    }
}

/// Whether a pattern matches everything, including a `_` written inside an
/// alternation (`A | _`).
pub(in crate::ir) fn pattern_has_wildcard(p: &ast::Pattern) -> bool {
    match p {
        ast::Pattern::Wildcard => true,
        ast::Pattern::Or { alts, .. } => alts.iter().any(pattern_has_wildcard),
        _ => false,
    }
}

/// Equality between two expressions, yielding 0 or 1.
pub(in crate::ir) fn eq(lhs: Expr, rhs: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::Eq,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    }
}

/// The guard for one expanded write: the enclosing condition narrowed by the
/// index-match `hit`.
///
/// A constant index matches unconditionally, and saying so matters beyond tidy
/// IR. `Some(Const(1))` is still a *conditional* driver, so `word[1] = '1'` at
/// an entity's root drew an inferred-latch warning (W-P002) and never became
/// the unconditional driver that a following partial write merges over.
pub(in crate::ir) fn write_guard(cond: &Option<Expr>, hit: Expr) -> Option<Expr> {
    if matches!(hit, Expr::Const(1)) {
        return cond.clone();
    }
    Some(and(cond.clone(), hit))
}

/// `and` of an optional accumulated condition with a new one.
pub(in crate::ir) fn and(acc: Option<Expr>, c: Expr) -> Expr {
    match acc {
        Some(a) => Expr::Binary {
            op: BinOp::And,
            lhs: Box::new(a),
            rhs: Box::new(c),
        },
        None => c,
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

/// Convert an AST prefix operator into its IR form.
pub(in crate::ir) fn lower_unop(op: AstUnOp) -> UnOp {
    match op {
        AstUnOp::Not => UnOp::Not,
        AstUnOp::Neg => UnOp::Neg,
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

/// Fold a constant expression using the already-folded constants in scope.
pub(in crate::ir) fn lower_const_value(
    expression: &ast::Expr,
    exact: &HashMap<String, Expr>,
    narrow: &HashMap<String, i64>,
    fns: &FunctionIndex<'_>,
) -> Option<Expr> {
    match expression {
        ast::Expr::Int { text, .. } if text.contains('.') => {
            text.replace('_', "").parse().ok().map(Expr::Real)
        }
        ast::Expr::Int { text, .. } => integer_const(text),
        ast::Expr::Path(path) => fns.constant_path_key(path).and_then(|key| {
            exact
                .get(&key)
                .cloned()
                .or_else(|| narrow.get(&key).map(|value| Expr::Const(*value as u64)))
        }),
        ast::Expr::Unary { op, rhs, .. } => Some(Expr::Unary {
            op: lower_unop(*op),
            rhs: Box::new(lower_const_value(rhs, exact, narrow, fns)?),
        }),
        ast::Expr::Binary { op, lhs, rhs, .. } => Some(Expr::Binary {
            op: lower_binop(op.clone())?,
            lhs: Box::new(lower_const_value(lhs, exact, narrow, fns)?),
            rhs: Box::new(lower_const_value(rhs, exact, narrow, fns)?),
        }),
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => Some(Expr::Select {
            cond: Box::new(lower_const_value(cond, exact, narrow, fns)?),
            then: Box::new(lower_const_value(then, exact, narrow, fns)?),
            els: Box::new(lower_const_value(els, exact, narrow, fns)?),
        }),
        ast::Expr::SuffixLit { text, suffix, .. } => Some(Expr::Binary {
            op: BinOp::Mul,
            lhs: Box::new(integer_const(text)?),
            rhs: Box::new(Expr::Const(
                ast::suffix_scale(&suffix.text).unwrap_or(1) as u64
            )),
        }),
        _ => None,
    }
}

/// Bit width from a type annotation, substituting parameters from `env` (so
/// `unsigned[W]` with `W=8` is width 8). `0` means parametric / not yet known.
pub(in crate::ir) fn source_type_span(ty: &ast::Type) -> crate::diag::Span {
    match ty {
        ast::Type::Path(path) => path.span,
        ast::Type::Indexed { span, .. }
        | ast::Type::Generic { span, .. }
        | ast::Type::View { span, .. } => *span,
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
        ast::Type::Path(_) => match fns.type_head_key(t).as_deref() {
            Some("integer") | Some("real") => 64, // native kernel word / f64 bits
            Some("Char") => 32,                   // symbol storage (implementation detail)
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
        ast::Type::Generic { base, .. } | ast::Type::View { target: base, .. } => {
            type_width(base, env, fns, structs, ranges)
        }
    }
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

/// `eval_const` with module functions in scope: a call whose arguments
/// const-evaluate runs the function body statically (recursion allowed to a
/// bounded depth) — `clog2(DEPTH)` works in width positions.
pub fn eval_const_fns(
    e: &ast::Expr,
    env: &HashMap<String, i64>,
    fns: &FunctionIndex<'_>,
    depth: u32,
) -> Option<i64> {
    if depth > 64 {
        return None;
    }
    match e {
        ast::Expr::Int { text, .. } => parse_int(text).map(|v| v as i64),
        ast::Expr::Path(path) => fns
            .constant_path_key(path)
            .and_then(|key| env.get(&key).copied()),
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => {
            if eval_const_fns(cond, env, fns, depth + 1)? != 0 {
                eval_const_fns(then, env, fns, depth + 1)
            } else {
                eval_const_fns(els, env, fns, depth + 1)
            }
        }
        ast::Expr::Call { callee, args, .. } => {
            let name = call_fn_key(callee)?;
            // Kernel conversions are value-transparent in const context.
            if name == "integer" || name == "Char" {
                return eval_const_fns(args.first()?, env, fns, depth + 1);
            }
            let f = fns.get(callee)?;
            let body = f.body.as_ref()?;
            // Parameters shadow local leaves, while resolver-qualified module
            // constants remain available inside the called function body.
            // Starting from an empty map made `fn f() { return VALUE; }`
            // cease to be constant even though `VALUE` was in the caller's
            // compile-time environment.
            let mut fenv = env.clone();
            for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
                let n = p.name.as_ref()?;
                fenv.insert(n.text.clone(), eval_const_fns(a, env, fns, depth + 1)?);
            }
            eval_const_stmts(&body.stmts, &fenv, fns, depth + 1)
        }
        ast::Expr::Unary { op, rhs, .. } => {
            let v = eval_const_fns(rhs, env, fns, depth + 1)?;
            match op {
                ast::UnOp::Neg => v.checked_neg(),
                ast::UnOp::Not => Some((v == 0) as i64),
            }
        }
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            let (a, b) = (
                eval_const_fns(lhs, env, fns, depth + 1)?,
                eval_const_fns(rhs, env, fns, depth + 1)?,
            );
            match op {
                ast::BinOp::Add => a.checked_add(b),
                ast::BinOp::Sub => a.checked_sub(b),
                ast::BinOp::Mul => a.checked_mul(b),
                ast::BinOp::Div => a.checked_div(b),
                ast::BinOp::Shl => u32::try_from(b).ok().and_then(|shift| a.checked_shl(shift)),
                ast::BinOp::Shr => u32::try_from(b).ok().and_then(|shift| a.checked_shr(shift)),
                ast::BinOp::Eq => Some((a == b) as i64),
                ast::BinOp::Ne => Some((a != b) as i64),
                ast::BinOp::Lt => Some((a < b) as i64),
                ast::BinOp::Le => Some((a <= b) as i64),
                ast::BinOp::Gt => Some((a > b) as i64),
                ast::BinOp::Ge => Some((a >= b) as i64),
                ast::BinOp::And => Some((a != 0 && b != 0) as i64),
                ast::BinOp::Or => Some((a != 0 || b != 0) as i64),
                ast::BinOp::Custom { .. } => None,
            }
        }
        _ => None,
    }
}

/// Statically execute a const-fn body: `return`s and `if`/`else` chains.
pub fn eval_const_stmts(
    stmts: &[ast::Stmt],
    env: &HashMap<String, i64>,
    fns: &FunctionIndex<'_>,
    depth: u32,
) -> Option<i64> {
    for st in stmts {
        match st {
            ast::Stmt::Return { value, .. } => {
                return eval_const_fns(value.as_ref()?, env, fns, depth);
            }
            ast::Stmt::If(iff) => {
                if eval_const_fns(&iff.cond, env, fns, depth)? != 0 {
                    if let Some(v) = eval_const_stmts(&iff.then.stmts, env, fns, depth) {
                        return Some(v);
                    }
                } else {
                    match iff.else_.as_deref() {
                        Some(ast::ElseBranch::Block(b)) => {
                            if let Some(v) = eval_const_stmts(&b.stmts, env, fns, depth) {
                                return Some(v);
                            }
                        }
                        Some(ast::ElseBranch::If(inner)) => {
                            if let Some(v) = eval_const_stmts(
                                std::slice::from_ref(&ast::Stmt::If(inner.clone())),
                                env,
                                fns,
                                depth,
                            ) {
                                return Some(v);
                            }
                        }
                        None => {}
                    }
                }
            }
            _ => return None,
        }
    }
    None
}

/// Evaluate the deliberately small pure subset used by `LogicEncoding` and
/// scalar logic operator tables. Unlike integer const evaluation, character
/// literals here are resolved against the implementing enum's source-owned
/// variant map.
pub(in crate::ir) fn eval_logic_function(
    function: &ast::FnDecl,
    env: &HashMap<String, u64>,
    variants: &HashMap<String, u64>,
) -> Option<u64> {
    eval_logic_stmts(&function.body.as_ref()?.stmts, env, variants)
}

/// Evaluate a std function body over logic values, so the value tables come
/// from std source rather than being duplicated in the backend.
pub(in crate::ir) fn eval_logic_stmts(
    statements: &[ast::Stmt],
    env: &HashMap<String, u64>,
    variants: &HashMap<String, u64>,
) -> Option<u64> {
    for statement in statements {
        match statement {
            ast::Stmt::Return {
                value: Some(value), ..
            } => {
                return eval_logic_expr(value, env, variants);
            }
            ast::Stmt::If(branch) => {
                if eval_logic_expr(&branch.cond, env, variants)? != 0 {
                    if let Some(value) = eval_logic_stmts(&branch.then.stmts, env, variants) {
                        return Some(value);
                    }
                } else if let Some(otherwise) = branch.else_.as_deref() {
                    let value = match otherwise {
                        ast::ElseBranch::Block(block) => {
                            eval_logic_stmts(&block.stmts, env, variants)
                        }
                        ast::ElseBranch::If(nested) => eval_logic_stmts(
                            std::slice::from_ref(&ast::Stmt::If(nested.clone())),
                            env,
                            variants,
                        ),
                    };
                    if value.is_some() {
                        return value;
                    }
                }
            }
            _ => return None,
        }
    }
    None
}

/// Evaluate one std expression over logic values.
pub(in crate::ir) fn eval_logic_expr(
    expression: &ast::Expr,
    env: &HashMap<String, u64>,
    variants: &HashMap<String, u64>,
) -> Option<u64> {
    match expression {
        ast::Expr::Path(path) => {
            let name = &path.segments.last()?.text;
            env.get(name).copied().or_else(|| match name.as_str() {
                "false" => Some(0),
                "true" => Some(1),
                _ => variants.get(name).copied(),
            })
        }
        ast::Expr::CharLit { ch, .. } => variants.get(&format!("'{ch}'")).copied(),
        ast::Expr::Int { text, .. } => parse_int(text),
        ast::Expr::Unary { op, rhs, .. } => {
            let value = eval_logic_expr(rhs, env, variants)?;
            match op {
                ast::UnOp::Not => Some(u64::from(value == 0)),
                ast::UnOp::Neg => value.checked_neg(),
            }
        }
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            let left = eval_logic_expr(lhs, env, variants)?;
            let right = eval_logic_expr(rhs, env, variants)?;
            Some(match op {
                ast::BinOp::Eq => u64::from(left == right),
                ast::BinOp::Ne => u64::from(left != right),
                ast::BinOp::Lt => u64::from(left < right),
                ast::BinOp::Le => u64::from(left <= right),
                ast::BinOp::Gt => u64::from(left > right),
                ast::BinOp::Ge => u64::from(left >= right),
                ast::BinOp::And => u64::from(left != 0 && right != 0),
                ast::BinOp::Or => u64::from(left != 0 || right != 0),
                ast::BinOp::Add => left.checked_add(right)?,
                ast::BinOp::Sub => left.checked_sub(right)?,
                ast::BinOp::Mul => left.checked_mul(right)?,
                ast::BinOp::Div => left.checked_div(right)?,
                ast::BinOp::Shl => left.checked_shl(u32::try_from(right).ok()?)?,
                ast::BinOp::Shr => left.checked_shr(u32::try_from(right).ok()?)?,
                ast::BinOp::Custom { .. } => return None,
            })
        }
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => {
            if eval_logic_expr(cond, env, variants)? != 0 {
                eval_logic_expr(then, env, variants)
            } else {
                eval_logic_expr(els, env, variants)
            }
        }
        _ => None,
    }
}
