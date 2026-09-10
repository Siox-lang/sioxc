//! Shared helpers used by the Siox-to-IR lowering stage.
//!
//! These are kept separate from the lowering state so representation-neutral
//! constant evaluation, substitution, and source-shape utilities do not make
//! the orchestration module harder to navigate.

use super::*;

// --- expression builders ----------------------------------------------------

/// Logical negation of a 0/1 expression.
pub(super) fn not(e: Expr) -> Expr {
    Expr::Unary {
        op: UnOp::Not,
        rhs: Box::new(e),
    }
}

/// Whether a pattern matches everything, including a `_` written inside an
/// alternation (`A | _`).
pub(super) fn pattern_has_wildcard(p: &ast::Pattern) -> bool {
    match p {
        ast::Pattern::Wildcard => true,
        ast::Pattern::Or { alts, .. } => alts.iter().any(pattern_has_wildcard),
        _ => false,
    }
}

/// Equality between two expressions, yielding 0 or 1.
pub(super) fn eq(lhs: Expr, rhs: Expr) -> Expr {
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
pub(super) fn write_guard(cond: &Option<Expr>, hit: Expr) -> Option<Expr> {
    if matches!(hit, Expr::Const(1)) {
        return cond.clone();
    }
    Some(and(cond.clone(), hit))
}

/// `and` of an optional accumulated condition with a new one.
pub(super) fn and(acc: Option<Expr>, c: Expr) -> Expr {
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
pub(super) fn expr_is_event(e: &ast::Expr) -> bool {
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
pub(super) fn lower_unop(op: AstUnOp) -> UnOp {
    match op {
        AstUnOp::Not => UnOp::Not,
        AstUnOp::Neg => UnOp::Neg,
    }
}

/// Convert an AST infix operator into its IR form, or `None` when it is
/// library-defined and must be inlined instead.
pub(super) fn lower_binop(op: AstBinOp) -> Option<BinOp> {
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
pub(super) fn parse_int(text: &str) -> Option<u64> {
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
pub(super) fn integer_const(text: &str) -> Option<Expr> {
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
pub(super) fn words_const(mut words: Vec<u64>) -> Expr {
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
pub(super) fn lower_const_value(
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
pub(super) fn source_type_span(ty: &ast::Type) -> crate::diag::Span {
    match ty {
        ast::Type::Path(path) => path.span,
        ast::Type::Indexed { span, .. }
        | ast::Type::Generic { span, .. }
        | ast::Type::View { span, .. } => *span,
    }
}

/// The bit width of a declared type in `env`.
pub(super) fn type_width(
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
pub(super) fn type_width_at(
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
pub(super) fn struct_derives_kernel(
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
pub(super) fn eval_const(e: &ast::Expr, env: &HashMap<String, i64>) -> Option<i64> {
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
pub(super) fn eval_logic_function(
    function: &ast::FnDecl,
    env: &HashMap<String, u64>,
    variants: &HashMap<String, u64>,
) -> Option<u64> {
    eval_logic_stmts(&function.body.as_ref()?.stmts, env, variants)
}

/// Evaluate a std function body over logic values, so the value tables come
/// from std source rather than being duplicated in the backend.
pub(super) fn eval_logic_stmts(
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
pub(super) fn eval_logic_expr(
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
pub(super) fn is_array_family_struct(
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
pub(super) fn enum_index<'a>(
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
pub(super) fn enum_base_name(
    e: &ast::EnumDecl,
    enums: &HashMap<String, &ast::EnumDecl>,
    fns: &FunctionIndex<'_>,
) -> Option<String> {
    let name = fns.type_head_key(e.repr.as_ref()?)?;
    enums.contains_key(&name).then_some(name)
}

/// An enum's effective variants, base chain first then its own declared ones
/// (spec: nominal derivation). `(name, explicit discriminant)`.
pub(super) fn effective_variants(
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

/// Flatten a struct literal into `suffix -> value` (".valid", ".inner.x"),
/// named the way a composite port's leaves are.
pub(super) fn literal_leaves<'a>(
    args: &'a [ast::ConnectArg],
    prefix: &str,
    out: &mut HashMap<String, &'a ast::Expr>,
) {
    for a in args {
        let (Some(name), Some(value)) = (a.field.as_ref(), a.value.as_ref()) else {
            continue;
        };
        let key = format!("{prefix}.{}", name.text);
        match value {
            ast::Expr::Construct { args: inner, .. } => literal_leaves(inner, &key, out),
            _ => {
                out.insert(key, value);
            }
        }
    }
}

/// The instance type + connections a `let` declares, in either form:
/// - `let x: Entity = { .. }` (type on the construct),
/// - `let x: Entity = { .. }` (type from the annotation, name-less construct),
/// - `let x: Entity;` (type from the annotation, no connections).
///
/// `entities` decides whether an annotation names an entity.
pub(super) fn instance_let_parts(
    l: &ast::LetDecl,
    entities: &HashMap<DefId, &ast::EntityDecl>,
    resolved: &Resolved,
) -> Option<(ast::Type, Vec<ast::ConnectArg>)> {
    // A *named* construction is a sub-instance only when the name is an
    // entity's. Every other branch below checks that; this one did not, so
    // `let p: Pair = Pair { .a = 1 }` — naming the struct, the way one
    // ordinarily writes a struct literal — was filed as an instance. No field
    // signals were ever created, and reading `p.a` came back as E-P017 "has no
    // hardware form", which describes a runtime-index problem the source does
    // not have. The same literal written `{ .a = 1 }` worked.
    if let Some(ast::Expr::Construct {
        ty: Some(cty),
        args,
        ..
    }) = &l.value
    {
        if type_def_id(cty, resolved).is_some_and(|id| entities.contains_key(&id)) {
            return Some((cty.clone(), args.clone()));
        }
    }
    let ann = l.ty.as_ref()?;
    // An entity *array* (`let stage: Inc[N]`) is built element-wise, not a
    // single instance.
    if matches!(ann, ast::Type::Indexed { .. }) {
        return None;
    }
    if !type_def_id(ann, resolved).is_some_and(|id| entities.contains_key(&id)) {
        return None;
    }
    match &l.value {
        // Dotted name-less construct `{ .a = a }`.
        Some(ast::Expr::Construct { ty: None, args, .. }) => Some((ann.clone(), args.clone())),
        // Positional/empty `{ a, b }` / `{}` lexes as a concat; its parts are
        // positional connections.
        Some(ast::Expr::Concat { parts, span }) => {
            let args = parts
                .iter()
                .map(|p| ast::ConnectArg {
                    field: None,
                    value: Some(p.clone()),
                    span: *span,
                })
                .collect();
            Some((ann.clone(), args))
        }
        None => Some((ann.clone(), Vec::new())),
        _ => None,
    }
}

/// Unroll a generate `for i in a..b { let s: Sub = {..} }` into concrete
/// sub-instances, substituting the loop index into each instance's name, type
/// arguments, and connection expressions. Plain `let` instances inside the
/// loop body are handled too; nested loops recurse. Non-instance statements
/// are left for the behavioural pass.
pub(super) fn gather_generate(
    s: &ast::Stmt,
    env: &HashMap<String, i64>,
    loop_idx: &[i64],
    entities: &HashMap<DefId, &ast::EntityDecl>,
    resolved: &Resolved,
    fns: &FunctionIndex<'_>,
    out: &mut Vec<(String, ast::Type, Vec<ast::ConnectArg>)>,
) {
    match s {
        ast::Stmt::Let(l) => {
            if let Some((cty, args)) = instance_let_parts(l, entities, resolved) {
                // A generated instance (inside a loop) gets the enclosing loop
                // indices appended for a unique name, matching the elaborator's
                // `<name>_<i>` convention.
                let name = if loop_idx.is_empty() {
                    l.name.text.clone()
                } else {
                    let idx: Vec<String> = loop_idx.iter().map(|v| v.to_string()).collect();
                    format!("{}_{}", l.name.text, idx.join("_"))
                };
                out.push((name, cty, args));
            }
        }
        // Instance-array element: `stage[i] = Sub { .. }` (index already
        // substituted). The rendered target (`stage[1]`) is the instance name,
        // matching the elaborator so `stage[i].port` reads line up.
        ast::Stmt::Assign {
            target,
            value:
                ast::Expr::Construct {
                    ty: Some(cty),
                    args,
                    ..
                },
            ..
        } => {
            if let Some(name) = expr_path(target) {
                out.push((name, cty.clone(), args.clone()));
            }
        }
        ast::Stmt::For {
            var,
            range: ast::Expr::Range { lo, hi, .. },
            body,
            ..
        } => {
            if let (Some(a), Some(b)) = (
                eval_const_fns(lo, env, fns, 0),
                eval_const_fns(hi, env, fns, 0),
            ) {
                for i in loop_range(a, b) {
                    let mut e = env.clone();
                    e.insert(var.text.clone(), i);
                    let mut idx = loop_idx.to_vec();
                    idx.push(i);
                    for st in &body.stmts {
                        // Substitute the loop index throughout the statement so
                        // `Sub<W=i>` and `wires[i]` become concrete before the
                        // instance is recorded.
                        let st = subst_stmt(st, &var.text, i);
                        gather_generate(&st, &e, &idx, entities, resolved, fns, out);
                    }
                }
            }
        }
        // `if <const> { .. } else { .. }`: a generate-if — the condition is
        // constant-folded and only the taken branch's instances are gathered.
        // A non-constant condition is behavioral, not a generate-if.
        ast::Stmt::If(iff) => {
            if let Some(c) = eval_const_fns(&iff.cond, env, fns, 0) {
                if c != 0 {
                    for st in &iff.then.stmts {
                        gather_generate(st, env, loop_idx, entities, resolved, fns, out);
                    }
                } else {
                    match iff.else_.as_deref() {
                        Some(ast::ElseBranch::Block(b)) => {
                            for st in &b.stmts {
                                gather_generate(st, env, loop_idx, entities, resolved, fns, out);
                            }
                        }
                        Some(ast::ElseBranch::If(inner)) => {
                            gather_generate(
                                &ast::Stmt::If(inner.clone()),
                                env,
                                loop_idx,
                                entities,
                                resolved,
                                fns,
                                out,
                            );
                        }
                        None => {}
                    }
                }
            }
        }
        _ => {}
    }
}

/// Substitute a bound integer for a single-segment path variable throughout a
/// statement (used to unroll generate loops).
/// Read a generic argument expression as a type: `unsigned[8]` (parsed as an index
/// expression) becomes the type `unsigned[8]`, a bare name becomes a path type.
/// Used to substitute a struct's type parameters (`Pair<unsigned[8]>`).
pub(super) fn expr_to_type(e: &ast::Expr) -> Option<ast::Type> {
    match e {
        ast::Expr::Path(p) => Some(ast::Type::Path(p.clone())),
        ast::Expr::Index { base, index, span } => Some(ast::Type::Indexed {
            base: Box::new(expr_to_type(base)?),
            index: Some(index.clone()),
            span: *span,
        }),
        _ => None,
    }
}

/// Substitute type parameters (`T -> unsigned[8]`) in a type, recursing through
/// array/generic/mode wrappers.
pub(super) fn subst_type_params(ty: &ast::Type, subst: &HashMap<String, ast::Type>) -> ast::Type {
    match ty {
        ast::Type::Path(p) if p.segments.len() == 1 => subst
            .get(&p.segments[0].text)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        ast::Type::Indexed { base, index, span } => ast::Type::Indexed {
            base: Box::new(subst_type_params(base, subst)),
            index: index.clone(),
            span: *span,
        },
        ast::Type::Generic { base, args, span } => ast::Type::Generic {
            base: Box::new(subst_type_params(base, subst)),
            args: args
                .iter()
                .map(|arg| match arg {
                    ast::GenericArg::Positional(ast::Expr::Path(path))
                        if path.segments.len() == 1
                            && subst.contains_key(&path.segments[0].text) =>
                    {
                        ast::GenericArg::PositionalType(subst[&path.segments[0].text].clone())
                    }
                    ast::GenericArg::Named {
                        name,
                        value: ast::Expr::Path(path),
                    } if path.segments.len() == 1 && subst.contains_key(&path.segments[0].text) => {
                        ast::GenericArg::NamedType {
                            name: name.clone(),
                            ty: subst[&path.segments[0].text].clone(),
                        }
                    }
                    ast::GenericArg::PositionalType(ty) => {
                        ast::GenericArg::PositionalType(subst_type_params(ty, subst))
                    }
                    ast::GenericArg::NamedType { name, ty } => ast::GenericArg::NamedType {
                        name: name.clone(),
                        ty: subst_type_params(ty, subst),
                    },
                    _ => arg.clone(),
                })
                .collect(),
            span: *span,
        },
        ast::Type::View { view, target, span } => ast::Type::View {
            view: view.clone(),
            target: Box::new(subst_type_params(target, subst)),
            span: *span,
        },
        _ => ty.clone(),
    }
}

/// Deep-clone a statement, replacing every bare single-segment path named in
/// `map` with its expression. Used to inline a method body: `self` maps to the
/// receiver and each parameter to its argument, so `self.valid = '1'` in a
/// method becomes `<recv>.valid = '1'` at the call site (spec 3.20). Public so
/// the testbench evaluators (siox-run, the native emitter) inline method calls
/// the same way hardware lowering does.
pub fn subst_stmt_paths(s: &ast::Stmt, map: &HashMap<String, ast::Expr>) -> ast::Stmt {
    use ast::Stmt;
    match s {
        Stmt::Assign {
            target,
            value,
            after,
            span,
        } => Stmt::Assign {
            target: subst_expr_paths(target, map),
            value: subst_expr_paths(value, map),
            after: after.as_ref().map(|a| subst_expr_paths(a, map)),
            span: *span,
        },
        Stmt::If(iff) => Stmt::If(subst_if_paths(iff, map)),
        Stmt::Match(m) => Stmt::Match(ast::MatchStmt {
            scrutinee: subst_expr_paths(&m.scrutinee, map),
            arms: m
                .arms
                .iter()
                .map(|a| ast::MatchArm {
                    pattern: a.pattern.clone(),
                    body: subst_block_paths(&a.body, map),
                    span: a.span,
                })
                .collect(),
            span: m.span,
        }),
        Stmt::For {
            var,
            range,
            body,
            span,
        } => Stmt::For {
            var: var.clone(),
            range: subst_expr_paths(range, map),
            body: subst_block_paths(body, map),
            span: *span,
        },
        Stmt::Let(l) => {
            let mut l = l.clone();
            l.value = l.value.as_ref().map(|v| subst_expr_paths(v, map));
            Stmt::Let(l)
        }
        Stmt::Expr(e) => Stmt::Expr(subst_expr_paths(e, map)),
        Stmt::Return { value, span } => Stmt::Return {
            value: value.as_ref().map(|v| subst_expr_paths(v, map)),
            span: *span,
        },
    }
}

/// Substitute path references throughout a block, for inlining.
pub(super) fn subst_block_paths(b: &ast::Block, map: &HashMap<String, ast::Expr>) -> ast::Block {
    ast::Block {
        stmts: b.stmts.iter().map(|s| subst_stmt_paths(s, map)).collect(),
        span: b.span,
    }
}

/// Substitute path references throughout an `if` chain.
pub(super) fn subst_if_paths(iff: &ast::IfStmt, map: &HashMap<String, ast::Expr>) -> ast::IfStmt {
    ast::IfStmt {
        cond: subst_expr_paths(&iff.cond, map),
        then: subst_block_paths(&iff.then, map),
        else_: iff.else_.as_ref().map(|e| {
            Box::new(match e.as_ref() {
                ast::ElseBranch::Block(b) => ast::ElseBranch::Block(subst_block_paths(b, map)),
                ast::ElseBranch::If(i) => ast::ElseBranch::If(subst_if_paths(i, map)),
            })
        }),
        span: iff.span,
    }
}

/// Deep-clone an expression, replacing every bare single-segment path named in
/// `map` with its mapped expression (the value-side counterpart of
/// [`subst_stmt_paths`]).
pub fn subst_expr_paths(e: &ast::Expr, map: &HashMap<String, ast::Expr>) -> ast::Expr {
    use ast::Expr;
    let sub = |x: &Expr| Box::new(subst_expr_paths(x, map));
    match e {
        Expr::Path(p) if p.segments.len() == 1 => map
            .get(&p.segments[0].text)
            .cloned()
            .unwrap_or_else(|| e.clone()),
        Expr::Field { base, field, span } => Expr::Field {
            base: sub(base),
            field: field.clone(),
            span: *span,
        },
        Expr::SysAttr { base, attr, span } => Expr::SysAttr {
            base: sub(base),
            attr: attr.clone(),
            span: *span,
        },
        Expr::Index { base, index, span } => Expr::Index {
            base: sub(base),
            index: sub(index),
            span: *span,
        },
        Expr::Range { lo, hi, span } => Expr::Range {
            lo: sub(lo),
            hi: sub(hi),
            span: *span,
        },
        Expr::PartialRange { lo, hi, span } => Expr::PartialRange {
            lo: lo.as_deref().map(sub),
            hi: hi.as_deref().map(sub),
            span: *span,
        },
        Expr::Unary { op, rhs, span } => Expr::Unary {
            op: *op,
            rhs: sub(rhs),
            span: *span,
        },
        Expr::Binary { op, lhs, rhs, span } => Expr::Binary {
            op: op.clone(),
            lhs: sub(lhs),
            rhs: sub(rhs),
            span: *span,
        },
        Expr::IfExpr {
            cond,
            then,
            els,
            span,
        } => Expr::IfExpr {
            cond: sub(cond),
            then: sub(then),
            els: sub(els),
            span: *span,
        },
        Expr::Call {
            callee,
            type_args,
            args,
            bang,
            span,
        } => Expr::Call {
            callee: sub(callee),
            type_args: type_args.clone(),
            args: args.iter().map(|a| subst_expr_paths(a, map)).collect(),
            bang: *bang,
            span: *span,
        },
        Expr::Concat { parts, span } => Expr::Concat {
            parts: parts.iter().map(|p| subst_expr_paths(p, map)).collect(),
            span: *span,
        },
        Expr::Array { elems, span } => Expr::Array {
            elems: elems.iter().map(|e| subst_expr_paths(e, map)).collect(),
            span: *span,
        },
        Expr::Construct {
            ty,
            args,
            spread,
            span,
        } => Expr::Construct {
            ty: ty.clone(),
            args: args
                .iter()
                .map(|a| ast::ConnectArg {
                    field: a.field.clone(),
                    value: a.value.as_ref().map(|v| subst_expr_paths(v, map)),
                    span: a.span,
                })
                .collect(),
            spread: spread.as_ref().map(|b| Box::new(subst_expr_paths(b, map))),
            span: *span,
        },
        other => other.clone(),
    }
}

/// Substitute a loop variable's value throughout a statement, for generate
/// unrolling.
pub(super) fn subst_stmt(s: &ast::Stmt, var: &str, val: i64) -> ast::Stmt {
    match s {
        ast::Stmt::Let(l) => {
            let mut l = l.clone();
            l.value = l.value.as_ref().map(|v| subst_expr(v, var, val));
            ast::Stmt::Let(l)
        }
        ast::Stmt::For {
            var: v,
            range,
            body,
            span,
        } => ast::Stmt::For {
            var: v.clone(),
            range: subst_expr(range, var, val),
            body: {
                let mut b = body.clone();
                b.stmts = b.stmts.iter().map(|st| subst_stmt(st, var, val)).collect();
                b
            },
            span: *span,
        },
        // `stage[i] = Sub { .x = w[i] }`: substitute in both the indexed target
        // and the construct, so instance-array elements unroll concretely.
        ast::Stmt::Assign {
            target,
            value,
            after,
            span,
        } => ast::Stmt::Assign {
            target: subst_expr(target, var, val),
            value: subst_expr(value, var, val),
            after: after.as_ref().map(|a| subst_expr(a, var, val)),
            span: *span,
        },
        // Recurse into `if`/`match` so a generate loop's index is substituted
        // inside their branches too (`for i { if i<N { .. w[i] .. } }`).
        ast::Stmt::If(iff) => ast::Stmt::If(subst_if(iff, var, val)),
        ast::Stmt::Match(m) => {
            let mut m = m.clone();
            m.scrutinee = subst_expr(&m.scrutinee, var, val);
            for arm in &mut m.arms {
                arm.body.stmts = arm
                    .body
                    .stmts
                    .iter()
                    .map(|s| subst_stmt(s, var, val))
                    .collect();
            }
            ast::Stmt::Match(m)
        }
        other => other.clone(),
    }
}

/// Substitute a loop variable's value throughout an `if` chain.
pub(super) fn subst_if(iff: &ast::IfStmt, var: &str, val: i64) -> ast::IfStmt {
    let mut n = iff.clone();
    n.cond = subst_expr(&iff.cond, var, val);
    n.then.stmts = iff
        .then
        .stmts
        .iter()
        .map(|s| subst_stmt(s, var, val))
        .collect();
    n.else_ = iff.else_.as_ref().map(|eb| {
        Box::new(match eb.as_ref() {
            ast::ElseBranch::Block(b) => {
                let mut b = b.clone();
                b.stmts = b.stmts.iter().map(|s| subst_stmt(s, var, val)).collect();
                ast::ElseBranch::Block(b)
            }
            ast::ElseBranch::If(inner) => ast::ElseBranch::If(subst_if(inner, var, val)),
        })
    });
    n
}

/// Deep-clone an expression, replacing every bare `var` reference with the
/// integer literal `val`. Also rewrites index/type-argument expressions.
pub(super) fn subst_expr(e: &ast::Expr, var: &str, val: i64) -> ast::Expr {
    use ast::Expr;
    let sub = |x: &Expr| Box::new(subst_expr(x, var, val));
    match e {
        // A negative iteration has to stay a well-formed AST: the lexer never
        // produces an `Int` whose text carries a sign, so `Int { text: "-1" }`
        // failed `parse_int` and every const-folding path treated the index as
        // non-constant. `for i in 0..(N - 1)` with `N = 0` counts down through
        // -1 (ranges are directional), and that iteration silently folded to
        // nothing instead of to element -1.
        Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == var => {
            int_literal(val, p.span)
        }
        Expr::Field { base, field, span } => Expr::Field {
            base: sub(base),
            field: field.clone(),
            span: *span,
        },
        Expr::SysAttr { base, attr, span } => Expr::SysAttr {
            base: sub(base),
            attr: attr.clone(),
            span: *span,
        },
        Expr::Index { base, index, span } => Expr::Index {
            base: sub(base),
            index: sub(index),
            span: *span,
        },
        Expr::Range { lo, hi, span } => Expr::Range {
            lo: sub(lo),
            hi: sub(hi),
            span: *span,
        },
        Expr::PartialRange { lo, hi, span } => Expr::PartialRange {
            lo: lo.as_deref().map(sub),
            hi: hi.as_deref().map(sub),
            span: *span,
        },
        // Fold constant arithmetic so a substituted index like `wires[i+1]`
        // becomes the literal `wires[2]` that `expr_path` can resolve.
        Expr::Unary { op, rhs, span } => {
            let n = Expr::Unary {
                op: *op,
                rhs: sub(rhs),
                span: *span,
            };
            fold_const(n, *span)
        }
        Expr::Binary { op, lhs, rhs, span } => {
            let n = Expr::Binary {
                op: op.clone(),
                lhs: sub(lhs),
                rhs: sub(rhs),
                span: *span,
            };
            fold_const(n, *span)
        }
        Expr::IfExpr {
            cond,
            then,
            els,
            span,
        } => Expr::IfExpr {
            cond: sub(cond),
            then: sub(then),
            els: sub(els),
            span: *span,
        },
        Expr::Call {
            callee,
            type_args,
            args,
            bang,
            span,
        } => Expr::Call {
            callee: sub(callee),
            type_args: type_args.clone(),
            args: args.iter().map(|a| subst_expr(a, var, val)).collect(),
            bang: *bang,
            span: *span,
        },
        Expr::Concat { parts, span } => Expr::Concat {
            parts: parts.iter().map(|p| subst_expr(p, var, val)).collect(),
            span: *span,
        },
        Expr::Array { elems, span } => Expr::Array {
            elems: elems.iter().map(|e| subst_expr(e, var, val)).collect(),
            span: *span,
        },
        Expr::Construct {
            ty,
            args,
            spread,
            span,
        } => Expr::Construct {
            ty: ty.as_ref().map(|t| subst_type(t, var, val)),
            args: args
                .iter()
                .map(|a| ast::ConnectArg {
                    field: a.field.clone(),
                    value: a.value.as_ref().map(|v| subst_expr(v, var, val)),
                    span: a.span,
                })
                .collect(),
            spread: spread.as_ref().map(|b| Box::new(subst_expr(b, var, val))),
            span: *span,
        },
        other => other.clone(),
    }
}

/// The values a `for i in left..right` loop visits. Range endpoints are **inclusive
/// and directional**, matching bit slices and array ranges elsewhere in the
/// language: `0..2` yields 0,1,2 and `2..0` yields 2,1,0.
pub fn loop_range(a: i64, b: i64) -> Vec<i64> {
    if a <= b {
        (a..=b).collect()
    } else {
        (b..=a).rev().collect()
    }
}

/// Collapse a now-constant arithmetic node to an integer literal, so unrolled
/// index expressions resolve as plain `Int`s. Non-constant nodes pass through.
/// Build the AST for an integer value.
///
/// The lexer never produces an `Int` whose text carries a sign, so a negative
/// value has to be a negation over an unsigned literal — `Int { text: "-5" }`
/// is a node no other stage can read. `parse_int` rejects it, which silently
/// demotes the whole expression to non-constant, and the value it was carrying
/// reaches hardware as 0. Both places that turn a folded `i64` back into AST
/// go through here so a third one cannot drift.
pub(super) fn int_literal(val: i64, span: crate::diag::Span) -> ast::Expr {
    let lit = ast::Expr::Int {
        text: val.unsigned_abs().to_string(),
        span,
    };
    if val < 0 {
        ast::Expr::Unary {
            op: ast::UnOp::Neg,
            rhs: Box::new(lit),
            span,
        }
    } else {
        lit
    }
}

/// Fold constant arithmetic in an expression, so unrolled generate bodies
/// carry concrete indices.
pub(super) fn fold_const(e: ast::Expr, span: crate::diag::Span) -> ast::Expr {
    match eval_const(&e, &HashMap::new()) {
        Some(v) => int_literal(v, span),
        None => e,
    }
}

/// Substitute the loop index into a type's index/generic-argument expressions.
pub(super) fn subst_type(t: &ast::Type, var: &str, val: i64) -> ast::Type {
    match t {
        ast::Type::Indexed { base, index, span } => ast::Type::Indexed {
            base: Box::new(subst_type(base, var, val)),
            index: index.as_ref().map(|i| Box::new(subst_expr(i, var, val))),
            span: *span,
        },
        ast::Type::Generic { base, args, span } => ast::Type::Generic {
            base: Box::new(subst_type(base, var, val)),
            args: args
                .iter()
                .map(|a| match a {
                    ast::GenericArg::Positional(e) => {
                        ast::GenericArg::Positional(subst_expr(e, var, val))
                    }
                    ast::GenericArg::PositionalType(ty) => {
                        ast::GenericArg::PositionalType(subst_type(ty, var, val))
                    }
                    ast::GenericArg::Named { name, value } => ast::GenericArg::Named {
                        name: name.clone(),
                        value: subst_expr(value, var, val),
                    },
                    ast::GenericArg::NamedType { name, ty } => ast::GenericArg::NamedType {
                        name: name.clone(),
                        ty: subst_type(ty, var, val),
                    },
                })
                .collect(),
            span: *span,
        },
        ast::Type::View { view, target, span } => ast::Type::View {
            view: view.clone(),
            target: Box::new(subst_type(target, var, val)),
            span: *span,
        },
        ast::Type::Path(_) => t.clone(),
    }
}

/// `p'old.valid` -> `p.valid'old`, `xs'old[0]` -> `xs[0]'old`. Only the two
/// value primitives move: `'length` and the range bounds describe the whole
/// aggregate, so pushing them at a leaf would change what is asked.
pub(super) fn sunk_sysattr(e: &ast::Expr) -> Option<ast::Expr> {
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
pub(super) fn expr_path(e: &ast::Expr) -> Option<String> {
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
pub(super) fn access_steps(e: &ast::Expr) -> Option<(String, Vec<AccessStep<'_>>)> {
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
pub(super) fn array_of<'t>(
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
pub(super) fn is_int_type(ty: &ast::Type) -> bool {
    matches!(ty, ast::Type::Path(p)
        if p.segments.last().map(|s| s.text.as_str()) == Some("integer"))
}

/// Build `enum name -> bit width`: the `repr` width if given (`enum S: unsigned[2]`),
/// else the bits needed for the variant count.
pub(super) fn enum_reprs(modules: &[Module], fns: &FunctionIndex<'_>) -> HashMap<String, u32> {
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
pub(super) fn is_test_entity(e: &ast::EntityDecl, resolved: &Resolved) -> bool {
    e.attrs
        .iter()
        .any(|attribute| crate::resolve::is_enabled_std_test_attribute(resolved, attribute))
}

/// The leading name of a type expression.
pub(super) fn type_head_name(t: &ast::Type) -> Option<&str> {
    match t {
        ast::Type::Path(p) => p.segments.first().map(|s| s.text.as_str()),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => type_head_name(base),
        ast::Type::View { view, .. } => view.segments.last().map(|s| s.text.as_str()),
    }
}

/// The definition a type expression names.
pub(super) fn type_def_id(ty: &ast::Type, resolved: &Resolved) -> Option<DefId> {
    match ty {
        ast::Type::Path(path) => resolved.resolved(path.span),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => {
            type_def_id(base, resolved)
        }
        ast::Type::View { view, .. } => resolved.resolved(view.span),
    }
}

/// Whether an impl is a blanket one over any array.
pub(super) fn is_blanket_array_impl(im: &ast::ImplDecl) -> bool {
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
pub(super) fn blanket_requirement(im: &ast::ImplDecl, fns: &FunctionIndex<'_>) -> Option<String> {
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
pub(super) fn file_integer_words(
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
