//! Type attributes: `pub attr integers: integer = self'high + 1;` inside a
//! type's implementation makes `x'integers` answer for every value `x` of
//! that type, and `Self'integers` inside its implementation.
//!
//! A type attribute is a shape fact: its value reads only `self`'s
//! attributes (system ones such as `'high`, or other type attributes),
//! constants and arithmetic over them, so a read is a constant, as `x'high`
//! is. The checker validates each declaration ([`codes::INVALID_TYPE_ATTR`])
//! and records each read; after checking, [`expand_type_attrs`] replaces
//! every read with the declared value, `self` standing for the read's base,
//! so later stages only ever see system attributes (`x'high + 1`).

use super::*;

impl Checker<'_> {
    /// The type attribute `name` that `base`'s type declares, with its owner
    /// key. `Self` is the implementation being checked.
    pub(super) fn type_attr(
        &self,
        base: &Expr,
        name: &str,
        sym: &HashMap<String, Ty>,
    ) -> Option<(String, &AttrDecl)> {
        let owner = match base {
            Expr::Path(path) if matches!(path.segments.as_slice(), [s] if s.text == "Self") => {
                self.current_impl_owner.borrow().clone()?
            }
            _ => self.ty_head(&self.type_of(base, sym))?,
        };
        let declaration = self.type_attrs.get(&(owner.clone(), name.to_string()))?;
        Some((owner, declaration))
    }

    /// Record a read of a type attribute for expansion, after checking that
    /// a private one is read inside its own type's implementation.
    pub(super) fn check_type_attr_read(&mut self, owner: &str, declaration: &AttrDecl, span: Span) {
        if !declaration.is_pub && self.current_impl_owner.borrow().as_deref() != Some(owner) {
            self.error_with_help(
                codes::PRIVATE_MEMBER,
                span,
                format!(
                    "type attribute `'{}` is private to `{}`",
                    declaration.name.text,
                    self.key_leaf(owner)
                ),
                "declare it `pub attr` to read it outside the type's implementation".to_string(),
            );
        }
        if let Some(value) = &declaration.default {
            self.type_attr_reads.insert(span, value.clone());
        }
    }

    /// Check one type attribute declaration: a free name, a value that is a
    /// shape fact, and a value of the declared type.
    pub(super) fn check_type_attr_decl(&mut self, declaration: &AttrDecl, in_trait_impl: bool) {
        let name = declaration.name.text.as_str();
        let taken = if SYS_ATTRS.contains(&name) || PHASE2_ATTRS.contains(&name) {
            Some("a system attribute")
        } else if self.attr_value_kinds.contains_key(name) {
            Some("a declared metadata attribute")
        } else {
            None
        };
        if let Some(taken) = taken {
            self.error(
                codes::INVALID_TYPE_ATTR,
                declaration.name.span,
                format!("`'{name}` is already {taken}, so a type attribute cannot use it"),
            );
        }
        if in_trait_impl {
            self.error(
                codes::INVALID_TYPE_ATTR,
                declaration.span,
                "a type attribute belongs to the type's own implementation, not a trait's"
                    .to_string(),
            );
        }
        let Some(value) = &declaration.default else {
            self.error(
                codes::INVALID_TYPE_ATTR,
                declaration.span,
                format!("type attribute `'{name}` needs a value: `= self'high + 1`"),
            );
            return;
        };
        if let Some(span) = self.not_a_shape_fact(value) {
            self.error_with_help(
                codes::INVALID_TYPE_ATTR,
                span,
                format!("type attribute `'{name}` must be a shape fact"),
                "it may read `self`'s attributes (`self'high`), constants and arithmetic over \
                 them, never the value itself; a computation on the value is a method"
                    .to_string(),
            );
            return;
        }
        let mut sym = HashMap::new();
        if let Some(ty) = self.current_self_ty.borrow().clone() {
            sym.insert("self".to_string(), ty);
        }
        self.check_expr(value, &sym);
        let expected = self.ast_ty(&declaration.ty);
        if !self.assignable(&expected, value, &sym) {
            self.error(
                codes::TYPE_MISMATCH,
                crate::syntax::ast::expr_span(value),
                format!(
                    "type attribute `'{name}` is declared {} but its value is {}",
                    ty_name(&expected),
                    ty_name(&self.type_of(value, &sym))
                ),
            );
        }
    }

    /// The first part of a type attribute's value that is not a shape fact.
    fn not_a_shape_fact(&self, e: &Expr) -> Option<Span> {
        match e {
            Expr::Int { .. }
            | Expr::SuffixLit { .. }
            | Expr::CharLit { .. }
            | Expr::StrLit { .. }
            | Expr::BitStrLit { .. } => None,
            Expr::Path(path) => (self
                .resolved
                .resolved(path.span)
                .and_then(|id| self.resolved.kind_of(id))
                != Some(DefKind::Const))
            .then_some(path.span),
            Expr::SysAttr { base, attr, span } => {
                let on_self = matches!(base.as_ref(), Expr::Path(path)
                    if matches!(path.segments.as_slice(), [s] if s.text == "self" || s.text == "Self"));
                (!on_self || matches!(attr.text.as_str(), "event" | "old")).then_some(*span)
            }
            Expr::Unary { rhs, .. } => self.not_a_shape_fact(rhs),
            Expr::Binary { lhs, rhs, .. } => self
                .not_a_shape_fact(lhs)
                .or_else(|| self.not_a_shape_fact(rhs)),
            Expr::IfExpr {
                cond, then, els, ..
            } => self
                .not_a_shape_fact(cond)
                .or_else(|| self.not_a_shape_fact(then))
                .or_else(|| self.not_a_shape_fact(els)),
            // A free function of shape facts (`clog2(self'length)`).
            Expr::Call {
                callee,
                qualifier: None,
                args,
                bang: false,
                ..
            } if matches!(callee.as_ref(), Expr::Path(_)) => args
                .iter()
                .find_map(|argument| self.not_a_shape_fact(argument)),
            other => Some(crate::syntax::ast::expr_span(other)),
        }
    }
}

/// Replace every type attribute read the checker recorded with the declared
/// value, `self` (and `Self`) standing for the read's base: `x'integers`
/// becomes `x'high + 1`. A value that reads another type attribute expands
/// in turn.
pub fn expand_type_attrs(modules: &mut [Module], typed: &Typed) {
    if typed.type_attr_reads.is_empty() {
        return;
    }
    let mut expand = |e: &mut Expr| expand_read(e, &typed.type_attr_reads, 0);
    for module in modules {
        for item in &mut module.items {
            walk_item(item, &mut expand);
        }
    }
}

/// Expand `e` when it is a recorded read; true when the walk should still
/// descend into it.
fn expand_read(e: &mut Expr, reads: &HashMap<Span, Expr>, depth: usize) -> bool {
    let Expr::SysAttr { base, span, .. } = e else {
        return true;
    };
    // A value defined through itself was reported; stop rather than loop.
    let Some(value) = reads.get(span).filter(|_| depth < 32) else {
        return true;
    };
    let mut replacement = value.clone();
    let base = base.as_ref().clone();
    walk_expr(&mut replacement, &mut |inner: &mut Expr| match inner {
        Expr::Path(path) if matches!(path.segments.as_slice(), [s] if s.text == "self" || s.text == "Self") =>
        {
            *inner = base.clone();
            false
        }
        _ => true,
    });
    walk_expr(&mut replacement, &mut |inner: &mut Expr| {
        expand_read(inner, reads, depth + 1)
    });
    *e = replacement;
    false
}

/// Visit every expression of an item, types' width expressions included.
fn walk_item(item: &mut Item, f: &mut impl FnMut(&mut Expr) -> bool) {
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
                    ImplItem::ModeField { .. } | ImplItem::Attr(_) => {}
                }
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

fn walk_fn(function: &mut FnDecl, f: &mut impl FnMut(&mut Expr) -> bool) {
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

fn walk_let(declaration: &mut LetDecl, f: &mut impl FnMut(&mut Expr) -> bool) {
    if let Some(ty) = &mut declaration.ty {
        walk_type(ty, f);
    }
    if let Some(value) = &mut declaration.value {
        walk_expr(value, f);
    }
}

fn walk_block(block: &mut Block, f: &mut impl FnMut(&mut Expr) -> bool) {
    for statement in &mut block.stmts {
        walk_stmt(statement, f);
    }
}

fn walk_if(statement: &mut IfStmt, f: &mut impl FnMut(&mut Expr) -> bool) {
    walk_expr(&mut statement.cond, f);
    walk_block(&mut statement.then, f);
    match statement.else_.as_deref_mut() {
        Some(ElseBranch::Block(block)) => walk_block(block, f),
        Some(ElseBranch::If(nested)) => walk_if(nested, f),
        None => {}
    }
}

fn walk_stmt(statement: &mut Stmt, f: &mut impl FnMut(&mut Expr) -> bool) {
    match statement {
        Stmt::Let(declaration) => walk_let(declaration, f),
        Stmt::Use(_) => {}
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
        Stmt::For { range, body, .. } => {
            walk_expr(range, f);
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

fn walk_type(ty: &mut Type, f: &mut impl FnMut(&mut Expr) -> bool) {
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
    }
}

fn walk_generic_arg(argument: &mut GenericArg, f: &mut impl FnMut(&mut Expr) -> bool) {
    match argument {
        GenericArg::Positional(e) | GenericArg::Named { value: e, .. } => walk_expr(e, f),
        GenericArg::PositionalType(ty) | GenericArg::NamedType { ty, .. } => walk_type(ty, f),
    }
}

/// Visit `e` and, while `f` says so, its subexpressions, before-order.
fn walk_expr(e: &mut Expr, f: &mut impl FnMut(&mut Expr) -> bool) {
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
        Expr::Concat { parts: items, .. } | Expr::Array { elems: items, .. } => {
            for item in items {
                walk_expr(item, f);
            }
        }
    }
}
