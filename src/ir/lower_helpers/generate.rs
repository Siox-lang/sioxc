//! Aggregate literal inspection and generate expansion.

use super::*;

/// Flatten a struct literal into `suffix -> value` (".valid", ".inner.x"),
/// named the way a composite port's leaves are.
pub(in crate::ir) fn literal_leaves<'a>(
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
pub(in crate::ir) fn instance_let_parts(
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
pub(in crate::ir) fn gather_generate(
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
