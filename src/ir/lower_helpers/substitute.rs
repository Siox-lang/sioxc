//! Type, path, and generate-value substitution.

use super::*;

/// Substitute a bound integer for a single-segment path variable throughout a
/// statement (used to unroll generate loops).
/// Read a generic argument expression as a type: `unsigned[8]` (parsed as an index
/// expression) becomes the type `unsigned[8]`, a bare name becomes a path type.
/// Used to substitute a struct's type parameters (`Pair<unsigned[8]>`).
pub(in crate::ir) fn expr_to_type(e: &ast::Expr) -> Option<ast::Type> {
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
pub(in crate::ir) fn subst_type_params(
    ty: &ast::Type,
    subst: &HashMap<String, ast::Type>,
) -> ast::Type {
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
pub(in crate::ir) fn subst_block_paths(
    b: &ast::Block,
    map: &HashMap<String, ast::Expr>,
) -> ast::Block {
    ast::Block {
        stmts: b.stmts.iter().map(|s| subst_stmt_paths(s, map)).collect(),
        span: b.span,
    }
}

/// Substitute path references throughout an `if` chain.
pub(in crate::ir) fn subst_if_paths(
    iff: &ast::IfStmt,
    map: &HashMap<String, ast::Expr>,
) -> ast::IfStmt {
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
pub(in crate::ir) fn subst_stmt(s: &ast::Stmt, var: &str, val: i64) -> ast::Stmt {
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
pub(in crate::ir) fn subst_if(iff: &ast::IfStmt, var: &str, val: i64) -> ast::IfStmt {
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
pub(in crate::ir) fn subst_expr(e: &ast::Expr, var: &str, val: i64) -> ast::Expr {
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
pub(in crate::ir) fn int_literal(val: i64, span: crate::diag::Span) -> ast::Expr {
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
pub(in crate::ir) fn fold_const(e: ast::Expr, span: crate::diag::Span) -> ast::Expr {
    match eval_const(&e, &HashMap::new()) {
        Some(v) => int_literal(v, span),
        None => e,
    }
}

/// Substitute the loop index into a type's index/generic-argument expressions.
pub(in crate::ir) fn subst_type(t: &ast::Type, var: &str, val: i64) -> ast::Type {
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
