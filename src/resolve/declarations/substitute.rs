//! Bind and substitute only the parameter identities of one template application.

use super::super::ParameterKind;
use super::*;
use crate::diag::Span;
use crate::syntax::ast::*;

#[derive(Debug)]
pub(crate) enum ArgumentError {
    Count { expected: usize, actual: usize },
    UnknownName { name: String, span: Span },
    Duplicate { parameter: usize, span: Span },
    ExpectedType { parameter: usize, span: Span },
    ExpectedValue { parameter: usize, span: Span },
    Unresolved,
}

pub(in crate::resolve) fn parameter_kind(
    param: &Param,
    resolved: &Resolved,
) -> Option<ParameterKind> {
    let Some(bound) = &param.bound else {
        return Some(ParameterKind::Type);
    };
    let Some(path) = super::super::type_head_path(bound) else {
        return Some(ParameterKind::Value);
    };
    let id = resolved.resolved(path.span)?;
    if resolved.kind_of(id) == Some(DefKind::Trait) || super::super::is_compiler_trait(resolved, id)
    {
        Some(ParameterKind::Type)
    } else {
        match resolved.kind_of(id)? {
            DefKind::Builtin
            | DefKind::Struct
            | DefKind::Enum
            | DefKind::Entity
            | DefKind::TypeAlias => Some(ParameterKind::Value),
            DefKind::Param if resolved.parameter_kinds.get(&id) == Some(&ParameterKind::Type) => {
                Some(ParameterKind::Value)
            }
            _ => None,
        }
    }
}

fn type_has_value_head(ty: &Type, resolved: &Resolved) -> bool {
    match ty {
        Type::Path(path) => resolved.resolved(path.span).is_some_and(|id| {
            resolved
                .def(id)
                .is_some_and(|definition| match definition.kind {
                    // `Self` is a compiler-introduced type binding, not a value local.
                    DefKind::Local => definition.name != "Self" || definition.span.is_some(),
                    DefKind::Const | DefKind::Fn | DefKind::EnumVariant => true,
                    DefKind::Param => {
                        resolved.parameter_kinds.get(&id) == Some(&ParameterKind::Value)
                    }
                    _ => false,
                })
        }),
        Type::Indexed { base, .. } | Type::Generic { base, .. } => {
            type_has_value_head(base, resolved)
        }
        Type::View { target, .. } => type_has_value_head(target, resolved),
        Type::Tuple { elems, .. } => elems.iter().any(|elem| type_has_value_head(elem, resolved)),
    }
}

pub(super) enum Argument<'a> {
    Type(Type),
    Value(&'a Expr),
}

pub(super) fn bind<'a>(
    params: &Params,
    args: &'a [GenericArg],
    resolved: &Resolved,
) -> Result<HashMap<DefId, Argument<'a>>, ArgumentError> {
    if params.params.len() != args.len() {
        return Err(ArgumentError::Count {
            expected: params.params.len(),
            actual: args.len(),
        });
    }
    let mut bindings = HashMap::new();
    for (position, arg) in args.iter().enumerate() {
        let parameter = match arg {
            GenericArg::Named { name, .. } | GenericArg::NamedType { name, .. } => params
                .params
                .iter()
                .position(|param| param.name.text == name.text)
                .ok_or_else(|| ArgumentError::UnknownName {
                    name: name.text.clone(),
                    span: name.span,
                })?,
            _ => position,
        };
        let param = &params.params[parameter];
        let span = match arg {
            GenericArg::Positional(value) => expr_span(value),
            GenericArg::PositionalType(ty) => type_span(ty),
            GenericArg::Named { name, .. } | GenericArg::NamedType { name, .. } => name.span,
        };
        let id = resolved
            .declared(param.name.span)
            .ok_or(ArgumentError::Unresolved)?;
        if bindings.contains_key(&id) {
            return Err(ArgumentError::Duplicate { parameter, span });
        }
        let argument = if parameter_kind(param, resolved).ok_or(ArgumentError::Unresolved)?
            == ParameterKind::Type
        {
            let ty = arg
                .as_type()
                .ok_or(ArgumentError::ExpectedType { parameter, span })?;
            if type_has_value_head(&ty, resolved) {
                return Err(ArgumentError::ExpectedType { parameter, span });
            }
            Argument::Type(ty)
        } else {
            let value = match arg {
                GenericArg::Positional(value) | GenericArg::Named { value, .. } => value,
                _ => return Err(ArgumentError::ExpectedValue { parameter, span }),
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
                    ) || resolved.parameter_kinds.get(&id) == Some(&ParameterKind::Type)
                        || resolved.def(id).is_some_and(|definition| {
                            definition.kind == DefKind::Local
                                && definition.name == "Self"
                                && definition.span.is_none()
                        })
                }) {
                    return Err(ArgumentError::ExpectedValue { parameter, span });
                }
            }
            Argument::Value(value)
        };
        bindings.insert(id, argument);
    }
    Ok(bindings)
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
        Stmt::Process(process) => block(&mut process.body, bindings, resolved)?,
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
        Stmt::For {
            range: cond, body, ..
        }
        | Stmt::While { cond, body, .. } => {
            expr(cond, bindings, resolved)?;
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
