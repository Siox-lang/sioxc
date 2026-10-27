//! Matching postorder walks for inspecting and rewriting source types/expressions.
//!
//! Generate both reference modes from the same walk so a phase can collect only
//! changed owned nodes while borrowing source declarations, then apply them.

use super::ast::*;

macro_rules! walk {
    ($module:ident, $roots:expr $(, $mutable:tt)?) => {
        pub(crate) mod $module {
            use super::*;

            pub(crate) fn modules(modules: & $($mutable)? [Module], types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                for module in modules {
                    for item in & $($mutable)? module.items { self::item(item, types, expressions); }
                }
            }

            fn attrs(attrs: & $($mutable)? [Attr], types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                for attr in attrs { if let Some(value) = & $($mutable)? attr.value { expr(value, types, expressions); } }
            }

            fn params(params: & $($mutable)? Params, types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                for param in & $($mutable)? params.params { if let Some(bound) = & $($mutable)? param.bound { ty(bound, types, expressions); } }
            }

            fn constant(constant: & $($mutable)? ConstDecl, types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                ty(& $($mutable)? constant.ty, types, expressions);
                expr(& $($mutable)? constant.value, types, expressions);
            }

            fn item(item: & $($mutable)? Item, types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                match item {
                    Item::Using(using) => if let UsingKind::Alias { params: p, ty: t, .. } = & $($mutable)? using.kind { params(p, types, expressions); ty(t, types, expressions); },
                    Item::Const(c) => constant(c, types, expressions),
                    Item::Fn(f) => function(f, types, expressions),
                    Item::ExternBlock { fns, .. } => for f in fns { function(f, types, expressions); },
                    Item::Struct(s) => {
                        params(& $($mutable)? s.params, types, expressions);
                        if let Some(base) = & $($mutable)? s.base { ty(base, types, expressions); }
                        for field in & $($mutable)? s.fields { ty(& $($mutable)? field.ty, types, expressions); }
                    }
                    Item::View(v) => { params(& $($mutable)? v.params, types, expressions); ty(& $($mutable)? v.target, types, expressions); }
                    Item::Enum(e) => {
                        if let Some(repr) = & $($mutable)? e.repr { ty(repr, types, expressions); }
                        for variant in & $($mutable)? e.variants { if let Some(value) = & $($mutable)? variant.value { expr(value, types, expressions); } }
                    }
                    Item::Entity(e) => {
                        params(& $($mutable)? e.params, types, expressions);
                        attrs(& $($mutable)? e.attrs, types, expressions);
                        for port in & $($mutable)? e.ports { attrs(& $($mutable)? port.attrs, types, expressions); ty(& $($mutable)? port.ty, types, expressions); }
                    }
                    Item::Impl(i) => {
                        params(& $($mutable)? i.params, types, expressions);
                        for argument in & $($mutable)? i.trait_args { arg(argument, types, expressions); }
                        ty(& $($mutable)? i.target, types, expressions);
                        attrs(& $($mutable)? i.attrs, types, expressions);
                        for member in & $($mutable)? i.items {
                            match member {
                                ImplItem::Const(c) => constant(c, types, expressions),
                                ImplItem::Let(l) => declaration(l, types, expressions),
                                ImplItem::Fn(f) => function(f, types, expressions),
                                ImplItem::Process(p) => block(& $($mutable)? p.body, types, expressions),
                                ImplItem::Stmt(s) => statement(s, types, expressions),
                                ImplItem::AttrBinding(b) => expr(& $($mutable)? b.value, types, expressions),
                                ImplItem::Attr(a) => attribute(a, types, expressions),
                                ImplItem::ModeField { .. } => {}
                            }
                        }
                    }
                    Item::Trait(t) => { params(& $($mutable)? t.params, types, expressions); for f in & $($mutable)? t.items { function(f, types, expressions); } }
                    Item::AttrDecl(a) => attribute(a, types, expressions),
                    Item::AttrBinding(b) => expr(& $($mutable)? b.value, types, expressions),
                    Item::Macro(_) | Item::MacroCall { .. } => {}
                }
            }

            fn attribute(attr: & $($mutable)? AttrDecl, types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                ty(& $($mutable)? attr.ty, types, expressions);
                if let Some(value) = & $($mutable)? attr.default { expr(value, types, expressions); }
            }

            pub(crate) fn function(function: & $($mutable)? FnDecl, types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                params(& $($mutable)? function.generics, types, expressions);
                for param in & $($mutable)? function.params { if let Some(t) = & $($mutable)? param.ty { ty(t, types, expressions); } }
                if let Some(ret) = & $($mutable)? function.ret { ty(ret, types, expressions); }
                if let Some(body) = & $($mutable)? function.body { block(body, types, expressions); }
            }

            fn declaration(declaration: & $($mutable)? LetDecl, types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                attrs(& $($mutable)? declaration.attrs, types, expressions);
                if let Some(t) = & $($mutable)? declaration.ty { ty(t, types, expressions); }
                if let Some(value) = & $($mutable)? declaration.value { expr(value, types, expressions); }
            }

            fn block(block: & $($mutable)? Block, types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                for s in & $($mutable)? block.stmts { statement(s, types, expressions); }
            }

            fn statement(statement: & $($mutable)? Stmt, types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                match statement {
                    Stmt::Use(_) => {}
                    Stmt::Let(l) => declaration(l, types, expressions),
                    Stmt::Assign { target, value, after, .. } => { expr(target, types, expressions); expr(value, types, expressions); if let Some(after) = after { expr(after, types, expressions); } }
                    Stmt::Expr(e) => expr(e, types, expressions),
                    Stmt::Return { value, .. } => if let Some(value) = value { expr(value, types, expressions); },
                    Stmt::For { range, body, .. } => { expr(range, types, expressions); block(body, types, expressions); }
                    Stmt::If(i) => if_statement(i, types, expressions),
                    Stmt::Match(m) => { expr(& $($mutable)? m.scrutinee, types, expressions); for arm in & $($mutable)? m.arms { pattern(& $($mutable)? arm.pattern, types, expressions); block(& $($mutable)? arm.body, types, expressions); } }
                }
            }

            fn if_statement(statement: & $($mutable)? IfStmt, types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                expr(& $($mutable)? statement.cond, types, expressions);
                block(& $($mutable)? statement.then, types, expressions);
                if let Some(other) = & $($mutable)? statement.else_ {
                    match & $($mutable)? **other { ElseBranch::Block(b) => block(b, types, expressions), ElseBranch::If(i) => if_statement(i, types, expressions) }
                }
            }

            fn pattern(pattern: & $($mutable)? Pattern, types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                match pattern {
                    Pattern::Bounds { lo, hi, .. } => for bound in [lo, hi].into_iter().flatten() { expr(bound, types, expressions); },
                    Pattern::Or { alts, .. } | Pattern::Tuple { elems: alts, .. } => for alt in alts { self::pattern(alt, types, expressions); },
                    _ => {}
                }
            }

            fn arg(arg: & $($mutable)? GenericArg, types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                nested_arg(arg, true, types, expressions);
            }

            fn nested_arg(arg: & $($mutable)? GenericArg, root: bool, types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                match arg { GenericArg::Positional(e) | GenericArg::Named { value: e, .. } => expr(e, types, expressions), GenericArg::PositionalType(t) | GenericArg::NamedType { ty: t, .. } => nested_ty(t, root, types, expressions) }
            }

            fn ty(ty: & $($mutable)? Type, types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                nested_ty(ty, true, types, expressions);
            }

            fn nested_ty(ty: & $($mutable)? Type, root: bool, types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                match ty {
                    Type::Path(_) => {}
                    Type::Indexed { base, index, .. } => { nested_ty(base, false, types, expressions); if let Some(index) = index { expr(index, types, expressions); } }
                    Type::Generic { base, args, .. } => { nested_ty(base, false, types, expressions); for a in args { nested_arg(a, false, types, expressions); } }
                    Type::Tuple { elems, .. } => for elem in elems { nested_ty(elem, false, types, expressions); },
                    Type::View { target, .. } => nested_ty(target, false, types, expressions),
                }
                if !$roots || root { types(ty); }
            }

            fn expr(expression: & $($mutable)? Expr, types: &mut impl FnMut(& $($mutable)? Type), expressions: &mut impl FnMut(& $($mutable)? Expr)) {
                match expression {
                    Expr::Int { .. } | Expr::SuffixLit { .. } | Expr::BitStrLit { .. } | Expr::CharLit { .. } | Expr::StrLit { .. } | Expr::Path(_) => {}
                    Expr::Field { base, .. } | Expr::SysAttr { base, .. } => expr(base, types, expressions),
                    Expr::Index { base: lhs, index: rhs, .. } | Expr::Range { lo: lhs, hi: rhs, .. } | Expr::Binary { lhs, rhs, .. } => { expr(lhs, types, expressions); expr(rhs, types, expressions); }
                    Expr::PartialRange { lo, hi, .. } => for bound in [lo, hi].into_iter().flatten() { expr(bound, types, expressions); },
                    Expr::Unary { rhs, .. } => expr(rhs, types, expressions),
                    Expr::IfExpr { cond, then, els, .. } => { expr(cond, types, expressions); expr(then, types, expressions); expr(els, types, expressions); }
                    Expr::Call { callee, type_args, qualifier, args, .. } => { expr(callee, types, expressions); for a in type_args { arg(a, types, expressions); } if let Some(q) = qualifier { ty(q, types, expressions); } for a in args { expr(a, types, expressions); } }
                    Expr::Construct { ty: t, args, spread, .. } => { if let Some(t) = t { ty(t, types, expressions); } for a in args { if let Some(v) = & $($mutable)? a.value { expr(v, types, expressions); } } if let Some(s) = spread { expr(s, types, expressions); } }
                    Expr::Tuple { elems, .. } | Expr::Array { elems, .. } | Expr::Concat { parts: elems, .. } => for e in elems { expr(e, types, expressions); },
                    Expr::Match { scrutinee, arms, .. } => { expr(scrutinee, types, expressions); for arm in arms { pattern(& $($mutable)? arm.pattern, types, expressions); block(& $($mutable)? arm.body, types, expressions); } }
                }
                expressions(expression);
            }
        }
    };
}

walk!(read, false);
walk!(write, false, mut);
// Layout checking owns the nested shape, but still needs every source type use.
walk!(read_roots, true);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_walk_covers_expression_types_without_repeating_nested_shapes() {
        let mut sink = crate::diag::DiagnosticSink::new();
        let module = crate::syntax::parse_module(
            crate::diag::FileId(0),
            "module sample;
             struct Packet<T> { data: T }
             fn inspect<U>() {
                 let value: Packet<(integer[2], integer[3])>;
                 let other: integer[4] = Packet<integer[5]>::size();
                 Packet<integer[6]> {};
             }",
            &mut sink,
        );
        assert_eq!(sink.error_count(), 0, "{:?}", sink.diagnostics());
        let mut roots = Vec::new();
        read_roots::modules(
            std::slice::from_ref(&module),
            &mut |ty| {
                roots.push(crate::syntax::pretty::type_str(ty));
            },
            &mut |_| {},
        );
        assert_eq!(roots.len(), 5, "{roots:?}");
        assert_eq!(
            roots.iter().filter(|ty| ty.starts_with("Packet<")).count(),
            3
        );
        assert!(roots.iter().any(|ty| ty == "integer[4]"));
        assert!(roots.iter().any(|ty| ty == "T"));
        let mut all = 0;
        read::modules(&[module], &mut |_| all += 1, &mut |_| {});
        assert!(all > roots.len());
    }
}
