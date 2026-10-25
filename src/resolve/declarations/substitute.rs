//! Substitute only the parameter identities belonging to one alias application.

use super::*;
use crate::syntax::ast::*;

pub(super) enum Argument<'a> {
    Type(Type),
    Value(&'a Expr),
}

pub(super) fn bind<'a>(
    params: &Params,
    args: &'a [GenericArg],
    resolved: &Resolved,
) -> Option<HashMap<DefId, Argument<'a>>> {
    if params.params.len() != args.len() {
        return None;
    }
    let mut bindings = HashMap::new();
    for (position, arg) in args.iter().enumerate() {
        let param = match arg {
            GenericArg::Named { name, .. } | GenericArg::NamedType { name, .. } => params
                .params
                .iter()
                .find(|param| param.name.text == name.text)?,
            _ => params.params.get(position)?,
        };
        let type_parameter = param.bound.as_ref().is_none_or(|bound| {
            let Some(path) = super::super::type_head_path(bound) else {
                return false;
            };
            resolved.resolved(path.span).is_some_and(|id| {
                resolved.kind_of(id) == Some(DefKind::Trait)
                    || super::super::is_compiler_trait(resolved, id)
            })
        });
        let argument = if type_parameter {
            let ty = arg.as_type()?;
            let mut head = &ty;
            while let Type::Indexed { base, .. } | Type::Generic { base, .. } = head {
                head = base;
            }
            if let Type::Path(path) = head {
                if resolved.resolved(path.span).is_some_and(|id| {
                    resolved
                        .def(id)
                        .is_some_and(|definition| match definition.kind {
                            // `Self` is a compiler-introduced type binding, not a value local.
                            DefKind::Local => {
                                definition.name != "Self" || definition.span.is_some()
                            }
                            DefKind::Const | DefKind::Fn | DefKind::EnumVariant => true,
                            _ => false,
                        })
                }) {
                    return None;
                }
            }
            Argument::Type(ty)
        } else {
            let value = match arg {
                GenericArg::Positional(value) | GenericArg::Named { value, .. } => value,
                _ => return None,
            };
            if let Expr::Path(path) = value {
                if resolved.resolved(path.span).is_some_and(|id| {
                    matches!(
                        resolved.kind_of(id),
                        Some(
                            DefKind::Builtin
                                | DefKind::Struct
                                | DefKind::Enum
                                | DefKind::Entity
                                | DefKind::Trait
                                | DefKind::TypeAlias
                        )
                    )
                }) {
                    return None;
                }
            }
            Argument::Value(value)
        };
        if bindings
            .insert(resolved.declared(param.name.span)?, argument)
            .is_some()
        {
            return None;
        }
    }
    Some(bindings)
}

pub(super) fn ty(
    ty: &mut Type,
    bindings: &HashMap<DefId, Argument<'_>>,
    resolved: &Resolved,
) -> Option<()> {
    match ty {
        Type::Path(path) => {
            if let Some(argument) = resolved
                .resolved(path.span)
                .and_then(|id| bindings.get(&id))
            {
                let Argument::Type(actual) = argument else {
                    return None;
                };
                *ty = actual.clone();
            }
        }
        Type::Indexed { base, index, .. } => {
            self::ty(base, bindings, resolved)?;
            if let Some(index) = index {
                expr(index, bindings, resolved)?;
            }
        }
        Type::Generic { base, args, .. } => {
            self::ty(base, bindings, resolved)?;
            for arg in args {
                generic_arg(arg, bindings, resolved)?;
            }
        }
        Type::View { target, .. } => self::ty(target, bindings, resolved)?,
        Type::Tuple { elems, .. } => {
            for elem in elems {
                self::ty(elem, bindings, resolved)?;
            }
        }
    }
    Some(())
}

fn generic_arg(
    arg: &mut GenericArg,
    bindings: &HashMap<DefId, Argument<'_>>,
    resolved: &Resolved,
) -> Option<()> {
    let value = match arg {
        GenericArg::Positional(value) | GenericArg::Named { value, .. } => value,
        GenericArg::PositionalType(t) | GenericArg::NamedType { ty: t, .. } => {
            return ty(t, bindings, resolved)
        }
    };
    if let Expr::Path(path) = value {
        if let Some(Argument::Type(actual)) = resolved
            .resolved(path.span)
            .and_then(|id| bindings.get(&id))
        {
            *arg = match arg {
                GenericArg::Named { name, .. } => GenericArg::NamedType {
                    name: name.clone(),
                    ty: actual.clone(),
                },
                _ => GenericArg::PositionalType(actual.clone()),
            };
            return Some(());
        }
    }
    expr(value, bindings, resolved)
}

fn type_expression(ty: &Type) -> Option<Expr> {
    match ty {
        Type::Path(path) => Some(Expr::Path(path.clone())),
        Type::Indexed {
            base,
            index: Some(index),
            span,
        } => Some(Expr::Index {
            base: Box::new(type_expression(base)?),
            index: index.clone(),
            span: *span,
        }),
        Type::Tuple { elems, span } => Some(Expr::Tuple {
            elems: elems.iter().map(type_expression).collect::<Option<_>>()?,
            span: *span,
        }),
        _ => None,
    }
}

fn expr(
    value: &mut Expr,
    bindings: &HashMap<DefId, Argument<'_>>,
    resolved: &Resolved,
) -> Option<()> {
    match value {
        Expr::Path(path) => {
            if let Some(argument) = resolved
                .resolved(path.span)
                .and_then(|id| bindings.get(&id))
            {
                *value = match argument {
                    Argument::Type(actual) => type_expression(actual)?,
                    Argument::Value(actual) => (*actual).clone(),
                };
            }
        }
        Expr::Field { base, .. } | Expr::SysAttr { base, .. } => expr(base, bindings, resolved)?,
        Expr::Index {
            base: lhs,
            index: rhs,
            ..
        }
        | Expr::Range {
            lo: lhs, hi: rhs, ..
        }
        | Expr::Binary { lhs, rhs, .. } => {
            expr(lhs, bindings, resolved)?;
            expr(rhs, bindings, resolved)?;
        }
        Expr::PartialRange { lo, hi, .. } => {
            for bound in [lo, hi].into_iter().flatten() {
                expr(bound, bindings, resolved)?;
            }
        }
        Expr::Unary { rhs, .. } => expr(rhs, bindings, resolved)?,
        Expr::IfExpr {
            cond, then, els, ..
        } => {
            expr(cond, bindings, resolved)?;
            expr(then, bindings, resolved)?;
            expr(els, bindings, resolved)?;
        }
        Expr::Call {
            callee,
            type_args,
            qualifier,
            args,
            ..
        } => {
            expr(callee, bindings, resolved)?;
            for arg in type_args {
                generic_arg(arg, bindings, resolved)?;
            }
            if let Some(qualifier) = qualifier {
                ty(qualifier, bindings, resolved)?;
            }
            for arg in args {
                expr(arg, bindings, resolved)?;
            }
        }
        Expr::Construct {
            ty: constructed,
            args,
            spread,
            ..
        } => {
            if let Some(constructed) = constructed {
                ty(constructed, bindings, resolved)?;
            }
            for arg in args {
                if let Some(value) = &mut arg.value {
                    expr(value, bindings, resolved)?;
                }
            }
            if let Some(spread) = spread {
                expr(spread, bindings, resolved)?;
            }
        }
        Expr::Tuple { elems, .. }
        | Expr::Array { elems, .. }
        | Expr::Concat { parts: elems, .. } => {
            for elem in elems {
                expr(elem, bindings, resolved)?;
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            expr(scrutinee, bindings, resolved)?;
            for arm in arms {
                pattern(&mut arm.pattern, bindings, resolved)?;
                block(&mut arm.body, bindings, resolved)?;
            }
        }
        Expr::Int { .. }
        | Expr::SuffixLit { .. }
        | Expr::BitStrLit { .. }
        | Expr::CharLit { .. }
        | Expr::StrLit { .. } => {}
    }
    Some(())
}

fn pattern(
    pattern: &mut Pattern,
    bindings: &HashMap<DefId, Argument<'_>>,
    resolved: &Resolved,
) -> Option<()> {
    match pattern {
        Pattern::Bounds { lo, hi, .. } => {
            for bound in [lo, hi].into_iter().flatten() {
                expr(bound, bindings, resolved)?;
            }
        }
        Pattern::Or { alts, .. } | Pattern::Tuple { elems: alts, .. } => {
            for alt in alts {
                self::pattern(alt, bindings, resolved)?;
            }
        }
        _ => {}
    }
    Some(())
}

fn block(
    block: &mut Block,
    bindings: &HashMap<DefId, Argument<'_>>,
    resolved: &Resolved,
) -> Option<()> {
    for stmt in &mut block.stmts {
        statement(stmt, bindings, resolved)?;
    }
    Some(())
}

fn if_statement(
    statement: &mut IfStmt,
    bindings: &HashMap<DefId, Argument<'_>>,
    resolved: &Resolved,
) -> Option<()> {
    expr(&mut statement.cond, bindings, resolved)?;
    block(&mut statement.then, bindings, resolved)?;
    match statement.else_.as_deref_mut() {
        Some(ElseBranch::Block(b)) => block(b, bindings, resolved)?,
        Some(ElseBranch::If(i)) => if_statement(i, bindings, resolved)?,
        None => {}
    }
    Some(())
}

fn statement(
    statement: &mut Stmt,
    bindings: &HashMap<DefId, Argument<'_>>,
    resolved: &Resolved,
) -> Option<()> {
    match statement {
        Stmt::Use(_) => {}
        Stmt::Let(declaration) => {
            if let Some(t) = &mut declaration.ty {
                ty(t, bindings, resolved)?;
            }
            if let Some(value) = &mut declaration.value {
                expr(value, bindings, resolved)?;
            }
        }
        Stmt::Assign {
            target,
            value,
            after,
            ..
        } => {
            expr(target, bindings, resolved)?;
            expr(value, bindings, resolved)?;
            if let Some(after) = after {
                expr(after, bindings, resolved)?;
            }
        }
        Stmt::If(i) => if_statement(i, bindings, resolved)?,
        Stmt::Match(m) => {
            expr(&mut m.scrutinee, bindings, resolved)?;
            for arm in &mut m.arms {
                pattern(&mut arm.pattern, bindings, resolved)?;
                block(&mut arm.body, bindings, resolved)?;
            }
        }
        Stmt::For { range, body, .. } => {
            expr(range, bindings, resolved)?;
            block(body, bindings, resolved)?;
        }
        Stmt::Expr(value) => expr(value, bindings, resolved)?,
        Stmt::Return { value, .. } => {
            if let Some(value) = value {
                expr(value, bindings, resolved)?;
            }
        }
    }
    Some(())
}
