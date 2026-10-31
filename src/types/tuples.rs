//! Tuples as the structs they are (after checking): `(A, B)` is the generic
//! struct `(,)<A, B>`, declared once per arity as `struct (,)<T0, T1> { 0: T0,
//! 1: T1 }`, and a tuple value `(a, b)` is the literal `{ .0 = a, .1 = b }`
//! typed by its consumer. Later stages lay tuples out, connect and flatten
//! them through the struct machinery they already have.
//!
//! A tuple stays a tuple in the two places that are not values: a `match`
//! scrutinee, whose positions are matched one by one and never built, and an
//! assignment target `(q, r) = e;`, which writes each position. A tuple
//! comparison `l == r` becomes `l.0 == r.0 and l.1 == r.1`.

use std::collections::BTreeSet;

use crate::diag::Span;
use crate::syntax::ast::*;

/// Rewrite every tuple type and tuple value in `modules`, and append the
/// module declaring the structs they now name. Nothing changes when the
/// source has no tuples.
pub fn lower_tuples(modules: &mut Vec<Module>, typed: &super::Typed) {
    let mut lower = Lower {
        typed,
        arities: BTreeSet::new(),
        span: None,
    };
    for module in modules.iter_mut() {
        for item in &mut module.items {
            lower.item(item);
        }
    }
    let Some(span) = lower.span else {
        return;
    };
    let ident = |text: String| Ident { text, span };
    let items = lower
        .arities
        .iter()
        .map(|&arity| {
            let param = |index: usize| format!("T{index}");
            Item::Struct(StructDecl {
                derives: Vec::new(),
                is_pub: true,
                name: ident(tuple_struct_name(arity)),
                params: Params {
                    params: (0..arity)
                        .map(|index| Param {
                            name: ident(param(index)),
                            bound: None,
                            span,
                        })
                        .collect(),
                },
                base: None,
                fields: (0..arity)
                    .map(|index| Field {
                        is_pub: true,
                        name: ident(index.to_string()),
                        ty: Type::Path(Path {
                            segments: vec![ident(param(index))],
                            span,
                        }),
                        span,
                    })
                    .collect(),
                span,
            })
        })
        .collect();
    modules.push(Module {
        path: Path {
            segments: vec![ident("tuple".to_string())],
            span,
        },
        items,
        lints: Vec::new(),
        span,
        macro_args: Default::default(),
        derived_files: Vec::new(),
    });
}

/// The name of the struct a tuple of `arity` elements is: `(,)` for a pair,
/// `(,,)` for a triple. No source name can be spelled this way.
pub fn tuple_struct_name(arity: usize) -> String {
    format!("({})", ",".repeat(arity.saturating_sub(1)))
}

/// The element count of a tuple struct named by [`tuple_struct_name`]. A
/// 1-tuple (`(integer,)`) is `()`; it used to be excluded, so its values had
/// no layout and a process using one did not lower (#155).
pub fn tuple_struct_arity(name: &str) -> Option<usize> {
    let commas = name.strip_prefix('(')?.strip_suffix(')')?;
    commas.chars().all(|c| c == ',').then(|| commas.len() + 1)
}

struct Lower<'t> {
    /// The checked types, which say which comparisons are of tuples.
    typed: &'t super::Typed,
    /// Every tuple arity met.
    arities: BTreeSet<usize>,
    /// A span to give the generated declarations: the first tuple's.
    span: Option<Span>,
}

impl Lower<'_> {
    fn note(&mut self, arity: usize, span: Span) {
        self.arities.insert(arity);
        self.span.get_or_insert(span);
    }

    fn item(&mut self, item: &mut Item) {
        match item {
            Item::Using(using) => {
                if let UsingKind::Alias { ty, .. } = &mut using.kind {
                    self.ty(ty);
                }
            }
            Item::Const(constant) => {
                self.ty(&mut constant.ty);
                self.expr(&mut constant.value);
            }
            Item::Fn(function) => self.function(function),
            Item::ExternBlock { fns, .. } => {
                for function in fns {
                    self.function(function);
                }
            }
            Item::Struct(declaration) => {
                if let Some(base) = &mut declaration.base {
                    self.ty(base);
                }
                for field in &mut declaration.fields {
                    self.ty(&mut field.ty);
                }
            }
            Item::View(view) => self.ty(&mut view.target),
            Item::Entity(entity) => {
                for port in &mut entity.ports {
                    self.ty(&mut port.ty);
                }
            }
            Item::Impl(implementation) => {
                self.ty(&mut implementation.target);
                for member in &mut implementation.items {
                    match member {
                        ImplItem::Const(constant) => {
                            self.ty(&mut constant.ty);
                            self.expr(&mut constant.value);
                        }
                        ImplItem::Let(declaration) => self.let_decl(declaration),
                        ImplItem::Fn(function) => self.function(function),
                        ImplItem::Process(process) => self.block(&mut process.body),
                        ImplItem::Stmt(statement) => self.stmt(statement),
                        ImplItem::AttrBinding(binding) => self.expr(&mut binding.value),
                        ImplItem::ModeField { .. } | ImplItem::Attr(_) => {}
                    }
                }
            }
            Item::Trait(declaration) => {
                for function in &mut declaration.items {
                    self.function(function);
                }
            }
            Item::AttrBinding(binding) => self.expr(&mut binding.value),
            Item::Enum(_) | Item::AttrDecl(_) | Item::Macro(_) | Item::MacroCall { .. } => {}
        }
    }

    fn function(&mut self, function: &mut FnDecl) {
        for parameter in &mut function.params {
            if let Some(ty) = &mut parameter.ty {
                self.ty(ty);
            }
        }
        if let Some(ty) = &mut function.ret {
            self.ty(ty);
        }
        if let Some(body) = &mut function.body {
            self.block(body);
        }
    }

    fn let_decl(&mut self, declaration: &mut LetDecl) {
        if let Some(ty) = &mut declaration.ty {
            self.ty(ty);
        }
        if let Some(value) = &mut declaration.value {
            self.expr(value);
        }
    }

    fn block(&mut self, block: &mut Block) {
        for statement in &mut block.stmts {
            self.stmt(statement);
        }
    }

    fn if_stmt(&mut self, statement: &mut IfStmt) {
        self.expr(&mut statement.cond);
        self.block(&mut statement.then);
        match statement.else_.as_deref_mut() {
            Some(ElseBranch::Block(block)) => self.block(block),
            Some(ElseBranch::If(nested)) => self.if_stmt(nested),
            None => {}
        }
    }

    fn stmt(&mut self, statement: &mut Stmt) {
        match statement {
            Stmt::Let(declaration) => self.let_decl(declaration),
            Stmt::Use(_) => {}
            Stmt::Process(process) => self.block(&mut process.body),
            Stmt::Assign {
                target,
                value,
                after,
                ..
            } => {
                self.kept(target);
                self.expr(value);
                if let Some(after) = after {
                    self.expr(after);
                }
            }
            Stmt::If(nested) => self.if_stmt(nested),
            Stmt::Match(nested) => {
                self.kept(&mut nested.scrutinee);
                for arm in &mut nested.arms {
                    self.pattern(&mut arm.pattern);
                    self.block(&mut arm.body);
                }
            }
            Stmt::For {
                range: cond, body, ..
            }
            | Stmt::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            }
            Stmt::Expr(e) => self.expr(e),
            Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    self.expr(value);
                }
            }
        }
    }

    fn pattern(&mut self, pattern: &mut Pattern) {
        match pattern {
            Pattern::Bounds { lo, hi, .. } => {
                for bound in [lo, hi].into_iter().flatten() {
                    self.expr(bound);
                }
            }
            Pattern::Or { alts: elems, .. } | Pattern::Tuple { elems, .. } => {
                for elem in elems {
                    self.pattern(elem);
                }
            }
            Pattern::Wildcard
            | Pattern::Path(_)
            | Pattern::BitPattern { .. }
            | Pattern::Range { .. }
            | Pattern::CharLit { .. } => {}
        }
    }

    /// A tuple that stays one — a scrutinee or an assignment target — with
    /// its elements, nested tuples included, kept the same way.
    fn kept(&mut self, e: &mut Expr) {
        match e {
            Expr::Tuple { elems, span } => {
                self.note(elems.len(), *span);
                for elem in elems {
                    self.kept(elem);
                }
            }
            other => self.expr(other),
        }
    }

    fn ty(&mut self, ty: &mut Type) {
        match ty {
            Type::Path(_) => {}
            Type::Indexed { base, index, .. } => {
                self.ty(base);
                if let Some(index) = index {
                    self.expr(index);
                }
            }
            Type::Generic { base, args, .. } => {
                self.ty(base);
                for arg in args {
                    self.generic_arg(arg);
                }
            }
            Type::View { target, .. } => self.ty(target),
            Type::Tuple { elems, span } => {
                let span = *span;
                let mut elems = std::mem::take(elems);
                for elem in &mut elems {
                    self.ty(elem);
                }
                self.note(elems.len(), span);
                *ty = Type::Generic {
                    base: Box::new(Type::Path(Path {
                        segments: vec![Ident {
                            text: tuple_struct_name(elems.len()),
                            span,
                        }],
                        span,
                    })),
                    args: elems.into_iter().map(GenericArg::PositionalType).collect(),
                    span,
                };
            }
        }
    }

    fn generic_arg(&mut self, arg: &mut GenericArg) {
        // `Box<(integer, integer)>` parses its argument as a tuple value; it
        // names a tuple type, and as a value it lowered to a construct the
        // struct's layout could not use (#227).
        fn as_type(e: &Expr) -> Option<Type> {
            match e {
                Expr::Path(path) => Some(Type::Path(path.clone())),
                Expr::Index { base, index, span } => Some(Type::Indexed {
                    base: Box::new(as_type(base)?),
                    index: Some(index.clone()),
                    span: *span,
                }),
                Expr::Tuple { elems, span } => Some(Type::Tuple {
                    elems: elems.iter().map(as_type).collect::<Option<_>>()?,
                    span: *span,
                }),
                _ => None,
            }
        }
        if let GenericArg::Positional(e @ Expr::Tuple { .. }) = arg {
            if let Some(ty) = as_type(e) {
                *arg = GenericArg::PositionalType(ty);
            }
        }
        // `Container<T = (Logic, Logic)>`: a named tuple argument too (#260).
        if let GenericArg::Named {
            name,
            value: e @ Expr::Tuple { .. },
        } = arg
        {
            if let Some(ty) = as_type(e) {
                *arg = GenericArg::NamedType {
                    name: name.clone(),
                    ty,
                };
            }
        }
        match arg {
            GenericArg::Positional(e) | GenericArg::Named { value: e, .. } => self.expr(e),
            GenericArg::PositionalType(ty) | GenericArg::NamedType { ty, .. } => self.ty(ty),
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match e {
            Expr::Int { .. }
            | Expr::SuffixLit { .. }
            | Expr::BitStrLit { .. }
            | Expr::CharLit { .. }
            | Expr::StrLit { .. }
            | Expr::Path(_) => {}
            Expr::Field { base, .. } | Expr::SysAttr { base, .. } => self.expr(base),
            Expr::Index { base, index, .. } => {
                self.expr(base);
                self.expr(index);
            }
            Expr::Range { lo, hi, .. } => {
                self.expr(lo);
                self.expr(hi);
            }
            Expr::PartialRange { lo, hi, .. } => {
                for bound in [lo, hi].into_iter().flatten() {
                    self.expr(bound);
                }
            }
            Expr::Unary { rhs, .. } => self.expr(rhs),
            Expr::Binary {
                op: op @ (BinOp::Eq | BinOp::Ne),
                lhs,
                rhs,
                span,
            } if matches!(
                self.typed.expr_type(expr_span(lhs)),
                Some(super::Ty::Tuple(_))
            ) =>
            {
                let Some(ty) = self.typed.expr_type(expr_span(lhs)).cloned() else {
                    return;
                };
                let mut offset = 0;
                let equal = tuple_equality(lhs, rhs, &ty, *span, &mut offset);
                *e = match op {
                    BinOp::Eq => equal,
                    _ => Expr::Unary {
                        op: UnOp::Not,
                        rhs: Box::new(equal),
                        span: *span,
                    },
                };
                self.expr(e);
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::IfExpr {
                cond, then, els, ..
            } => {
                self.expr(cond);
                self.expr(then);
                self.expr(els);
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.kept(scrutinee);
                for arm in arms {
                    self.pattern(&mut arm.pattern);
                    self.block(&mut arm.body);
                }
            }
            Expr::Call {
                callee,
                type_args,
                qualifier,
                args,
                ..
            } => {
                self.expr(callee);
                for arg in type_args {
                    self.generic_arg(arg);
                }
                if let Some(qualifier) = qualifier {
                    self.ty(qualifier);
                }
                for arg in args {
                    self.expr(arg);
                }
            }
            Expr::Construct {
                ty, args, spread, ..
            } => {
                if let Some(ty) = ty {
                    self.ty(ty);
                }
                for arg in args {
                    if let Some(value) = &mut arg.value {
                        self.expr(value);
                    }
                }
                if let Some(spread) = spread {
                    self.expr(spread);
                }
            }
            Expr::Concat { parts: elems, .. } | Expr::Array { elems, .. } => {
                for elem in elems {
                    self.expr(elem);
                }
            }
            // `(a, b)` is `{ .0 = a, .1 = b }`, the struct literal its
            // consumer types.
            Expr::Tuple { elems, span } => {
                let span = *span;
                let mut elems = std::mem::take(elems);
                for elem in &mut elems {
                    self.expr(elem);
                }
                self.note(elems.len(), span);
                *e = Expr::Construct {
                    ty: None,
                    args: elems
                        .into_iter()
                        .enumerate()
                        .map(|(index, value)| {
                            let at = expr_span(&value);
                            ConnectArg {
                                field: Some(Ident {
                                    text: index.to_string(),
                                    span: at,
                                }),
                                value: Some(value),
                                span: at,
                            }
                        })
                        .collect(),
                    spread: None,
                    span,
                };
            }
        }
    }
}

/// `l == r` over a tuple type, position by position: each position of a
/// tuple literal is its element, any other operand's is `.i`. Each piece
/// gets a distinct empty span inside `span`, so nothing keyed by span takes
/// two of them for one.
fn tuple_equality(lhs: &Expr, rhs: &Expr, ty: &super::Ty, span: Span, offset: &mut u32) -> Expr {
    let super::Ty::Tuple(elems) = ty else {
        return Expr::Binary {
            op: BinOp::Eq,
            lhs: Box::new(lhs.clone()),
            rhs: Box::new(rhs.clone()),
            span: fresh(span, offset),
        };
    };
    let position = |operand: &Expr, index: usize, offset: &mut u32| match operand {
        Expr::Tuple { elems, .. } => elems[index].clone(),
        whole => {
            let at = fresh(span, offset);
            Expr::Field {
                base: Box::new(whole.clone()),
                field: Ident {
                    text: index.to_string(),
                    span: at,
                },
                span: at,
            }
        }
    };
    let mut all: Option<Expr> = None;
    for (index, elem) in elems.iter().enumerate() {
        let left = position(lhs, index, offset);
        let right = position(rhs, index, offset);
        let equal = tuple_equality(&left, &right, elem, span, offset);
        all = Some(match all {
            Some(all) => Expr::Binary {
                op: BinOp::And,
                lhs: Box::new(all),
                rhs: Box::new(equal),
                span: fresh(span, offset),
            },
            None => equal,
        });
    }
    all.expect("a tuple has two or more positions")
}

/// A new empty span inside `span`.
fn fresh(span: Span, offset: &mut u32) -> Span {
    *offset += 1;
    let at = span.start.saturating_add(*offset).min(span.end);
    Span::new(span.file, at..at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tuple_struct_names_round_trip() {
        assert_eq!(tuple_struct_name(2), "(,)");
        assert_eq!(tuple_struct_name(3), "(,,)");
        assert_eq!(tuple_struct_arity("(,,)"), Some(3));
        assert_eq!(tuple_struct_arity("(,)"), Some(2));
        assert_eq!(tuple_struct_arity("()"), Some(1));
        assert_eq!(tuple_struct_arity("Pair"), None);
    }
}
