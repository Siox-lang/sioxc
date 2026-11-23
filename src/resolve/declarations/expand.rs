//! Materialize alias applications/constructors after definition-site resolution.

use std::collections::VecDeque;

use super::*;
use crate::diag::{codes, Diagnostic, DiagnosticSink, Span};
use crate::syntax::{ast::*, visit};

pub(crate) fn normalize_aliases(
    modules: &mut [Module],
    resolved: &Resolved,
    sink: &mut DiagnosticSink,
) {
    let mut types: HashMap<Span, VecDeque<Type>> = HashMap::new();
    let mut calls: HashMap<Span, VecDeque<Type>> = HashMap::new();
    {
        let index = DeclarationIndex::collect(resolved, modules);
        let problems = std::cell::RefCell::new(Vec::new());
        visit::read::modules(
            modules,
            &mut |ty| {
                let projection = projection(ty, resolved);
                if !projection && !alias_application(ty, &index, resolved) {
                    return;
                }
                if !projection && !index.valid_alias_arguments(ty) {
                    let Type::Generic { base, args, .. } = ty else {
                        unreachable!()
                    };
                    let Type::Path(path) = base.as_ref() else {
                        unreachable!()
                    };
                    if resolved
                        .resolved(path.span)
                        .and_then(|id| index.alias_arity(id))
                        == Some(args.len())
                    {
                        problems.borrow_mut().push(invalid_arguments(type_span(ty)));
                    } // Wrong counts already have a resolver diagnostic.
                } else if let Some(normalized) = index.normalize_type(ty) {
                    types
                        .entry(type_span(ty))
                        .or_default()
                        .push_back(normalized.into_owned());
                } else if projection {
                    let Type::Generic { args, .. } = ty else {
                        unreachable!()
                    };
                    let symbolic = args.first().and_then(GenericArg::as_type).is_some_and(|t| matches!(t, Type::Path(path) if resolved.resolved(path.span).is_some_and(|id| resolved.kind_of(id) == Some(DefKind::Param))));
                    if !symbolic {
                        problems.borrow_mut().push(
                            Diagnostic::error("this type has no such tuple position")
                                .with_code(codes::TYPE_MISMATCH)
                                .at(type_span(ty)),
                        );
                    }
                }
            },
            &mut |expression| {
                let Expr::Call {
                    callee,
                    type_args,
                    span,
                    ..
                } = expression
                else {
                    return;
                };
                let Expr::Path(path) = callee.as_ref() else {
                    return;
                };
                if !resolved
                    .resolved(path.span)
                    .is_some_and(|id| resolved.kind_of(id) == Some(DefKind::TypeAlias))
                {
                    return;
                }
                let ty = Type::Generic {
                    base: Box::new(Type::Path(path.clone())),
                    args: type_args.clone(),
                    span: *span,
                };
                if !index.valid_alias_arguments(&ty) {
                    problems.borrow_mut().push(invalid_arguments(*span));
                } else if let Some(normalized) = index.normalize_type(&ty) {
                    calls
                        .entry(*span)
                        .or_default()
                        .push_back(normalized.into_owned());
                }
            },
        );
        for problem in problems.into_inner() {
            sink.emit(problem);
        }
    }
    // FIFO per span also handles multiple macro copies carrying the same span.
    visit::write::modules(
        modules,
        &mut |ty| {
            if !matches!(ty, Type::Generic { .. }) {
                return;
            }
            if let Some(normalized) = types.get_mut(&type_span(ty)).and_then(VecDeque::pop_front) {
                *ty = normalized;
            }
        },
        &mut |expression| {
            let Expr::Call {
                callee,
                type_args,
                span,
                ..
            } = expression
            else {
                return;
            };
            let span = *span;
            let Some(normalized) = calls.get_mut(&span).and_then(VecDeque::pop_front) else {
                return;
            };
            let (base, args) = match normalized {
                Type::Generic { base, args, .. } => (*base, args),
                other => (other, Vec::new()),
            };
            if let Some(constructor) = constructor(base) {
                **callee = constructor;
                *type_args = args;
            } else {
                sink.emit(
                    Diagnostic::error("this alias does not name a callable value type")
                        .with_code(codes::TYPE_MISMATCH)
                        .at(span),
                );
            }
        },
    );
    debug_assert!(types.values().all(VecDeque::is_empty));
    debug_assert!(calls.values().all(VecDeque::is_empty));
}

fn alias_application(ty: &Type, index: &DeclarationIndex<'_>, resolved: &Resolved) -> bool {
    matches!(ty, Type::Generic { base, .. } if matches!(base.as_ref(), Type::Path(path) if resolved.resolved(path.span).is_some_and(|id| index.generic_alias(id))))
}

fn projection(ty: &Type, resolved: &Resolved) -> bool {
    matches!(ty, Type::Generic { base, .. } if matches!(base.as_ref(), Type::Path(path) if resolved.resolved(path.span).and_then(|id| resolved.def(id)).is_some_and(|d| d.kind == DefKind::Builtin && d.name == TUPLE_POSITION)))
}

fn invalid_arguments(span: Span) -> Diagnostic {
    Diagnostic::error("invalid arguments for type alias").with_code(codes::TYPE_MISMATCH).at(span)
        .help("supply each declared parameter once, using a type for a type parameter and a value for a value parameter")
}

fn constructor(ty: Type) -> Option<Expr> {
    match ty {
        Type::Path(path) => Some(Expr::Path(path)),
        Type::Indexed {
            base,
            index: Some(index),
            span,
        } => Some(Expr::Index {
            base: Box::new(constructor(*base)?),
            index,
            span,
        }),
        Type::Tuple { elems, span } => Some(Expr::Tuple {
            elems: elems.into_iter().map(constructor).collect::<Option<_>>()?,
            span,
        }),
        _ => None,
    }
}
