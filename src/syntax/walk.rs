//! Mutable traversal of the syntax tree: visit every expression of an item,
//! block or statement, width expressions in types included. A pass that
//! rewrites expressions in place (attribute folding, read expansion, implicit
//! field names) walks with these rather than re-listing every node kind.

use crate::syntax::ast::*;

/// Visit every expression of an item, types' width expressions included.
pub fn walk_item(item: &mut Item, f: &mut impl FnMut(&mut Expr) -> bool) {
    match item {
        Item::Const(constant) => {
            walk_type(&mut constant.ty, f);
            walk_expr(&mut constant.value, f);
        }
        Item::Fn(function) => walk_fn(function, f),
        Item::Entity(entity) => {
            for port in &mut entity.ports {
                walk_type(&mut port.ty, f);
            }
        }
        Item::Impl(implementation) => {
            for member in &mut implementation.items {
                walk_impl_item(member, f);
            }
        }
        Item::Trait(declaration) => {
            for function in &mut declaration.items {
                walk_fn(function, f);
            }
        }
        _ => {}
    }
}

/// Visit an implementation member's expressions, a scoped block's members
/// included.
pub fn walk_impl_item(member: &mut ImplItem, f: &mut impl FnMut(&mut Expr) -> bool) {
    match member {
        ImplItem::Const(constant) => {
            walk_type(&mut constant.ty, f);
            walk_expr(&mut constant.value, f);
        }
        ImplItem::Let(declaration) => walk_let(declaration, f),
        ImplItem::Fn(function) => walk_fn(function, f),
        ImplItem::Process(process) => walk_block(&mut process.body, f),
        ImplItem::Stmt(statement) => walk_stmt(statement, f),
        ImplItem::AttrBinding(binding) => walk_expr(&mut binding.value, f),
        ImplItem::Block(block) => {
            for item in &mut block.items {
                walk_impl_item(item, f);
            }
        }
        ImplItem::ModeField { .. } | ImplItem::Attr(_) => {}
    }
}

/// Visit a function's parameter and return types and its body.
pub fn walk_fn(function: &mut FnDecl, f: &mut impl FnMut(&mut Expr) -> bool) {
    for parameter in &mut function.params {
        if let Some(ty) = &mut parameter.ty {
            walk_type(ty, f);
        }
    }
    if let Some(ty) = &mut function.ret {
        walk_type(ty, f);
    }
    if let Some(body) = &mut function.body {
        walk_block(body, f);
    }
}

/// Visit a `let`'s type and initializer.
pub fn walk_let(declaration: &mut LetDecl, f: &mut impl FnMut(&mut Expr) -> bool) {
    if let Some(ty) = &mut declaration.ty {
        walk_type(ty, f);
    }
    if let Some(value) = &mut declaration.value {
        walk_expr(value, f);
    }
}

/// Visit every statement of a block.
pub fn walk_block(block: &mut Block, f: &mut impl FnMut(&mut Expr) -> bool) {
    for statement in &mut block.stmts {
        walk_stmt(statement, f);
    }
}

/// Visit an `if` chain's conditions and branches.
pub fn walk_if(statement: &mut IfStmt, f: &mut impl FnMut(&mut Expr) -> bool) {
    walk_expr(&mut statement.cond, f);
    walk_block(&mut statement.then, f);
    match statement.else_.as_deref_mut() {
        Some(ElseBranch::Block(block)) => walk_block(block, f),
        Some(ElseBranch::If(nested)) => walk_if(nested, f),
        None => {}
    }
}

/// Visit a statement's expressions, nested blocks included.
pub fn walk_stmt(statement: &mut Stmt, f: &mut impl FnMut(&mut Expr) -> bool) {
    match statement {
        Stmt::Let(declaration) => walk_let(declaration, f),
        Stmt::Use(_) => {}
        Stmt::Process(process) => walk_block(&mut process.body, f),
        Stmt::Assign {
            target,
            value,
            after,
            ..
        } => {
            walk_expr(target, f);
            walk_expr(value, f);
            if let Some(after) = after {
                walk_expr(after, f);
            }
        }
        Stmt::If(nested) => walk_if(nested, f),
        Stmt::Match(nested) => {
            walk_expr(&mut nested.scrutinee, f);
            for arm in &mut nested.arms {
                walk_block(&mut arm.body, f);
            }
        }
        Stmt::For {
            range: cond, body, ..
        }
        | Stmt::While { cond, body, .. } => {
            walk_expr(cond, f);
            walk_block(body, f);
        }
        Stmt::Expr(e) => walk_expr(e, f),
        Stmt::Return { value, .. } => {
            if let Some(value) = value {
                walk_expr(value, f);
            }
        }
    }
}

/// Visit the expressions inside a type (widths, generic values).
pub fn walk_type(ty: &mut Type, f: &mut impl FnMut(&mut Expr) -> bool) {
    match ty {
        Type::Path(_) => {}
        Type::Indexed { base, index, .. } => {
            walk_type(base, f);
            if let Some(index) = index {
                walk_expr(index, f);
            }
        }
        Type::Generic { base, args, .. } => {
            walk_type(base, f);
            for argument in args {
                walk_generic_arg(argument, f);
            }
        }
        Type::View { target, .. } => walk_type(target, f),
        Type::Tuple { elems, .. } => {
            for elem in elems {
                walk_type(elem, f);
            }
        }
    }
}

/// Visit a generic argument's expression or type.
pub fn walk_generic_arg(argument: &mut GenericArg, f: &mut impl FnMut(&mut Expr) -> bool) {
    match argument {
        GenericArg::Positional(e) | GenericArg::Named { value: e, .. } => walk_expr(e, f),
        GenericArg::PositionalType(ty) | GenericArg::NamedType { ty, .. } => walk_type(ty, f),
    }
}

/// Visit `e` and, while `f` says so, its subexpressions, before-order.
pub fn walk_expr(e: &mut Expr, f: &mut impl FnMut(&mut Expr) -> bool) {
    if !f(e) {
        return;
    }
    match e {
        Expr::Int { .. }
        | Expr::SuffixLit { .. }
        | Expr::BitStrLit { .. }
        | Expr::CharLit { .. }
        | Expr::StrLit { .. }
        | Expr::Path(_) => {}
        Expr::Field { base, .. } | Expr::SysAttr { base, .. } => walk_expr(base, f),
        Expr::Index { base, index, .. } => {
            walk_expr(base, f);
            walk_expr(index, f);
        }
        Expr::Range { lo, hi, .. } => {
            walk_expr(lo, f);
            walk_expr(hi, f);
        }
        Expr::PartialRange { lo, hi, .. } => {
            for bound in [lo, hi].into_iter().flatten() {
                walk_expr(bound, f);
            }
        }
        Expr::Unary { rhs, .. } => walk_expr(rhs, f),
        Expr::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        Expr::IfExpr {
            cond, then, els, ..
        } => {
            walk_expr(cond, f);
            walk_expr(then, f);
            walk_expr(els, f);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            walk_expr(scrutinee, f);
            for arm in arms {
                walk_block(&mut arm.body, f);
            }
        }
        Expr::Call {
            callee,
            type_args,
            qualifier,
            args,
            ..
        } => {
            walk_expr(callee, f);
            for argument in type_args {
                walk_generic_arg(argument, f);
            }
            if let Some(qualifier) = qualifier {
                walk_type(qualifier, f);
            }
            for argument in args {
                walk_expr(argument, f);
            }
        }
        Expr::Construct {
            ty, args, spread, ..
        } => {
            if let Some(ty) = ty {
                walk_type(ty, f);
            }
            for argument in args {
                if let Some(value) = &mut argument.value {
                    walk_expr(value, f);
                }
            }
            if let Some(spread) = spread {
                walk_expr(spread, f);
            }
        }
        Expr::Concat { parts: items, .. }
        | Expr::Array { elems: items, .. }
        | Expr::Tuple { elems: items, .. } => {
            for item in items {
                walk_expr(item, f);
            }
        }
    }
}
