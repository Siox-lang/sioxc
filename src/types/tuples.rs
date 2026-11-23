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

use std::collections::{BTreeMap, BTreeSet};

use crate::diag::{FileId, SourceMap, Span};
use crate::resolve::{DefInfo, DefKind, ParameterKind, Resolved};
use crate::syntax::ast::*;

/// Rewrite every tuple type and tuple value in `modules`, and append the
/// module declaring the structs they now name. Nothing changes when the
/// source has no tuples. Generated declarations extend the existing resolver
/// facts; checked source definitions and references keep their identities.
pub fn lower_tuples(
    modules: &mut Vec<Module>,
    typed: &super::Typed,
    resolved: &mut Resolved,
    sources: &mut SourceMap,
) {
    let mut lower = Lower {
        typed,
        sources,
        arities: BTreeSet::new(),
        file: None,
        text: String::new(),
        references: Vec::new(),
    };
    for module in modules.iter_mut() {
        for item in &mut module.items {
            lower.item(item);
        }
    }
    if lower.arities.is_empty() {
        return;
    }
    let mut owners = BTreeMap::new();
    let mut items = Vec::new();
    for arity in std::mem::take(&mut lower.arities) {
        let name = lower.ident(tuple_struct_name(arity));
        let span = name.span;
        let owner = resolved.register_generated_definition(
            DefInfo {
                name: name.text.clone(),
                module: Some("tuple".to_string()),
                kind: DefKind::Struct,
                is_pub: true,
                span: Some(span),
                parent: None,
            },
            None,
        );
        owners.insert(arity, owner);
        let mut params = Vec::new();
        let mut fields = Vec::new();
        for index in 0..arity {
            let parameter = lower.ident(format!("T{index}"));
            let id = resolved.register_generated_definition(
                DefInfo {
                    name: parameter.text.clone(),
                    module: Some("tuple".to_string()),
                    kind: DefKind::Param,
                    is_pub: false,
                    span: Some(parameter.span),
                    parent: Some(owner),
                },
                Some(ParameterKind::Type),
            );
            let reference = lower.ident(parameter.text.clone());
            resolved.register_generated_use(reference.span, id);
            let field = lower.ident(index.to_string());
            fields.push(Field {
                is_pub: true,
                span: field.span,
                name: field,
                ty: Type::Path(Path {
                    span: reference.span,
                    segments: vec![reference],
                }),
            });
            params.push(Param {
                span: parameter.span,
                name: parameter,
                bound: None,
            });
        }
        items.push(Item::Struct(StructDecl {
            derives: Vec::new(),
            is_pub: true,
            name,
            params: Params { params },
            base: None,
            fields,
            span,
        }));
    }
    for (span, arity) in &lower.references {
        resolved.register_generated_use(*span, owners[arity]);
    }
    let name = lower.ident("tuple".to_string());
    let span = name.span;
    lower.sources.replace_text(
        lower
            .file
            .expect("a tuple declaration allocated its source"),
        std::mem::take(&mut lower.text),
    );
    modules.push(Module {
        path: Path {
            segments: vec![name],
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

struct Lower<'t, 's> {
    /// The checked types, which say which comparisons are of tuples.
    typed: &'t super::Typed,
    sources: &'s mut SourceMap,
    /// Every tuple arity met.
    arities: BTreeSet<usize>,
    file: Option<FileId>,
    text: String,
    references: Vec<(Span, usize)>,
}

impl Lower<'_, '_> {
    fn note(&mut self, arity: usize) {
        self.arities.insert(arity);
    }

    fn ident(&mut self, text: String) -> Ident {
        let file = *self
            .file
            .get_or_insert_with(|| self.sources.add("<tuple types>", ""));
        let start = self.text.len() as u32;
        self.text.push_str(&text);
        let end = self.text.len() as u32;
        self.text.push('\n');
        Ident {
            text,
            span: Span::new(file, start..end),
        }
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
            Expr::Tuple { elems, .. } => {
                self.note(elems.len());
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
                self.note(elems.len());
                let name = self.ident(tuple_struct_name(elems.len()));
                self.references.push((name.span, elems.len()));
                *ty = Type::Generic {
                    base: Box::new(Type::Path(Path {
                        span: name.span,
                        segments: vec![name],
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
                // An alias-expanded tuple callee names a type, not tuple values.
                if matches!(callee.as_ref(), Expr::Tuple { .. }) && args.is_empty() {
                    if let Some(mut target) = type_from_expr(callee) {
                        self.ty(&mut target);
                        if let Type::Generic { base, args, .. } = target {
                            if let Type::Path(path) = *base {
                                **callee = Expr::Path(path);
                                *type_args = args;
                            }
                        }
                    }
                } else {
                    self.expr(callee);
                }
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
                self.note(elems.len());
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

    #[test]
    fn generated_tuple_facts_preserve_source_ids_and_bind_each_parameter() {
        let mut input = [
            "module tuples;
             struct Record { pub value: integer }
             type Pair<T> = (T, T);
             entity Top {
                 pair: Pair<integer> in,
                 nested: ((integer,), other::Record) in,
                 triple: (Record, integer, real) in
             }
             impl Top {}",
            "module other; pub struct Record { pub value: real }",
        ];
        for _ in 0..2 {
            let mut sources = SourceMap::new();
            let mut sink = crate::diag::DiagnosticSink::new();
            let mut modules = input
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    let file = sources.add(format!("tuple-{index}.siox"), *text);
                    crate::syntax::parse_module(file, text, &mut sink)
                })
                .collect::<Vec<_>>();
            let mut resolved = crate::resolve::resolve(&modules, &mut sink);
            crate::resolve::normalize_aliases(&mut modules, &resolved, &mut sink);
            let typed = super::super::check(&modules, &resolved, &mut sink);
            assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());
            let original = resolved.defs().to_vec();
            let original_qualified = original
                .iter()
                .enumerate()
                .map(|(index, _)| resolved.qualified_name(crate::resolve::DefId(index as u32)))
                .collect::<Vec<_>>();
            lower_tuples(&mut modules, &typed, &mut resolved, &mut sources);
            for (index, before) in original.iter().enumerate() {
                let id = crate::resolve::DefId(index as u32);
                let after = resolved.def(id).unwrap();
                assert_eq!(after.name, before.name);
                assert_eq!(after.module, before.module);
                assert_eq!(after.kind, before.kind);
                assert_eq!(after.span, before.span);
                assert_eq!(after.parent, before.parent);
                assert_eq!(resolved.qualified_name(id), original_qualified[index]);
            }
            assert!(sources.get(FileId(2)).is_some());
            assert!(sources.get(FileId(3)).is_none());
            let index =
                crate::resolve::declarations::DeclarationIndex::collect(&resolved, &modules);
            let generated = modules.last().unwrap();
            let mut parameters = std::collections::HashSet::new();
            for item in &generated.items {
                let Item::Struct(structure) = item else {
                    panic!("generated tuple declaration: {item:?}");
                };
                let owner = resolved.declared(structure.name.span).unwrap();
                assert!(owner.0 as usize >= original.len());
                assert_eq!(resolved.kind_of(owner), Some(DefKind::Struct));
                for (parameter, field) in structure.params.params.iter().zip(&structure.fields) {
                    let id = resolved.declared(parameter.name.span).unwrap();
                    assert!(
                        parameters.insert(id),
                        "distinct generated binder identities"
                    );
                    assert_eq!(resolved.kind_of(id), Some(DefKind::Param));
                    let Type::Path(path) = &field.ty else {
                        panic!("{field:?}")
                    };
                    assert_eq!(resolved.resolved(path.span), Some(id));
                    assert_eq!(
                        sources.get(path.span.file).unwrap().text
                            [path.span.start as usize..path.span.end as usize],
                        parameter.name.text
                    );
                }
            }
            for port in modules
                .iter()
                .flat_map(|module| &module.items)
                .filter_map(|item| {
                    if let Item::Entity(entity) = item {
                        Some(&entity.ports)
                    } else {
                        None
                    }
                })
                .flatten()
            {
                let normalized = index
                    .normalize_type(&port.ty)
                    .expect("generated applications have identities");
                let Type::Generic { base, args, .. } = normalized.as_ref() else {
                    panic!("{normalized:?}")
                };
                let Type::Path(path) = base.as_ref() else {
                    panic!("{base:?}")
                };
                let owner = resolved.resolved(path.span).unwrap();
                let structure = index.structure(owner).unwrap();
                let bindings = index.arguments(&structure.params, args).unwrap();
                for (field, argument) in structure.fields.iter().zip(args) {
                    let actual = index.instantiate_type(&bindings, &field.ty).unwrap();
                    let expected = argument.as_type().unwrap();
                    assert_eq!(
                        crate::syntax::pretty::type_str(&actual),
                        crate::syntax::pretty::type_str(&expected)
                    );
                    assert_eq!(
                        index.scalar_type_id(&actual),
                        index.scalar_type_id(&expected)
                    );
                }
            }
            input.reverse();
        }
    }

    #[test]
    fn no_tuple_pass_adds_no_generated_source_or_definitions() {
        let mut sources = SourceMap::new();
        let text = "module plain; entity Top {} impl Top {}";
        let file = sources.add("plain.siox", text);
        let mut sink = crate::diag::DiagnosticSink::new();
        let mut modules = vec![crate::syntax::parse_module(file, text, &mut sink)];
        let mut resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = super::super::check(&modules, &resolved, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());
        let definitions = resolved.defs().len();
        lower_tuples(&mut modules, &typed, &mut resolved, &mut sources);
        assert_eq!(resolved.defs().len(), definitions);
        assert_eq!(modules.len(), 1);
        assert!(sources.get(FileId(1)).is_none());
    }
}
