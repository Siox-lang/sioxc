//! Entity methods (owner-approved design, 2026-11-03): a `pub fn` with a
//! receiver on an entity is called on a direct child instance,
//! `fifo.push(x)` or `fifo.is_full()`. Before resolution, each one becomes
//! ports and a process on the child, and wiring in the parent, so every later
//! stage sees only ordinary hardware:
//!
//! - the child gains `m#called: Bit in`, one `m#p: T in` per parameter and,
//!   for a result, `m#ret: R out`. The body becomes a process `m#call` that
//!   gives every signal it writes a default and then, while `m#called` is
//!   `'1'`, runs the body with `return e;` as `m#ret = e;`. A method's writes
//!   are therefore combinational strobes, active only while it is called (a
//!   SystemVerilog interface function, as `push(v) { wr_en = '1'; din = v; }`
//!   drives an `xpm_fifo_sync`); the state lives in the child.
//! - the parent gains hidden signals `x#m#called`, `x#m#p`, `x#m#ret`, wired
//!   into the instance's connections. A call site sets `called` and the
//!   arguments just before its statement and reads `ret` in its place; the
//!   enclosing process defaults them, so no call means no strobe.
//!
//! One call site per method per instance (more would need arbitration), a
//! direct child only, and no testbench callers yet: each is reported. Names
//! the pass generates take spans in a source file of their own, so no two
//! generated names can share a span with each other or with written code.

use std::collections::{HashMap, HashSet};

use crate::diag::{Diagnostic, DiagnosticSink, FileId, SourceMap, Span};
use crate::syntax::ast::*;

/// One public method of an entity: its signature, as the ports carry it.
#[derive(Clone)]
struct Method {
    name: String,
    params: Vec<(String, Type)>,
    ret: Option<Type>,
}

/// Allocates spans in the generated file, each covering its own name.
struct Spans {
    file: FileId,
    text: String,
}

impl Spans {
    fn at(&mut self, label: &str) -> Span {
        let start = self.text.len() as u32;
        self.text.push_str(label);
        let end = self.text.len() as u32;
        self.text.push('\n');
        Span {
            file: self.file,
            start,
            end,
        }
    }

    fn ident(&mut self, text: &str) -> Ident {
        Ident {
            text: text.to_string(),
            span: self.at(text),
        }
    }

    fn path(&mut self, text: &str) -> Expr {
        let ident = self.ident(text);
        Expr::Path(Path {
            span: ident.span,
            segments: vec![ident],
        })
    }

    fn assign(&mut self, target: &str, value: Expr) -> Stmt {
        let target = self.path(target);
        let span = crate::syntax::ast::expr_span(&target);
        Stmt::Assign {
            label: None,
            target,
            value,
            after: None,
            span,
        }
    }

    fn bit(&mut self, value: char) -> Expr {
        Expr::CharLit {
            ch: value,
            span: self.at(&format!("'{value}'")),
        }
    }

    fn let_decl(&mut self, name: &str, ty: &Type) -> LetDecl {
        let name = self.ident(name);
        LetDecl {
            attrs: Vec::new(),
            span: name.span,
            name,
            ty: Some(ty.clone()),
            value: None,
            is_static: false,
            is_pub: false,
        }
    }
}

/// `impl Doubler for Pt` where `type Pt = Point;` implements `Point`: an
/// alias is transparent, but the impl was filed under the alias's name, so
/// `p.double()`, `Pt::double(p)` and a `T: Doubler` bound on a `Point` all
/// failed (#490). A non-generic alias of a named type is replaced by that
/// type, through chains of aliases: declared in the impl's module,
/// imported (`use types_mod::Pt;`), or named by path (`types_mod::Pt`),
/// which were left as written (#616).
// ponytail: a generic alias is left as written.
pub fn impl_aliases(modules: &mut [Module]) {
    fn joined(segments: &[Ident]) -> String {
        segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("::")
    }
    // Every alias by its qualified name, its target qualified by its module
    // when it names a type declared there, so it reads the same elsewhere.
    let mut qualified: HashMap<String, Type> = HashMap::new();
    for module in modules.iter() {
        let declared: HashSet<&str> = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Struct(declaration) => Some(declaration.name.text.as_str()),
                Item::Enum(declaration) => Some(declaration.name.text.as_str()),
                Item::Entity(declaration) => Some(declaration.name.text.as_str()),
                _ => None,
            })
            .collect();
        for item in &module.items {
            let Item::Using(Using {
                kind:
                    UsingKind::Alias {
                        name,
                        params,
                        ty: Type::Path(path),
                    },
                ..
            }) = item
            else {
                continue;
            };
            if !params.params.is_empty() {
                continue;
            }
            let mut target = path.clone();
            if let [only] = path.segments.as_slice() {
                if declared.contains(only.text.as_str()) {
                    target.segments = module
                        .path
                        .segments
                        .iter()
                        .cloned()
                        .chain(std::iter::once(only.clone()))
                        .collect();
                }
            }
            qualified.insert(
                format!("{}::{}", joined(&module.path.segments), name.text),
                Type::Path(target),
            );
        }
    }
    for module in modules {
        let mut aliases: HashMap<String, Type> = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Using(Using {
                    kind: UsingKind::Alias { name, params, ty },
                    ..
                }) if params.params.is_empty() && matches!(ty, Type::Path(_)) => {
                    Some((name.text.clone(), ty.clone()))
                }
                _ => None,
            })
            .collect();
        for item in &module.items {
            let Item::Using(Using {
                kind: UsingKind::Import { base, names },
                ..
            }) = item
            else {
                continue;
            };
            for imported in names.iter().filter(|imported| !imported.glob) {
                let full: Vec<Ident> = base
                    .segments
                    .iter()
                    .chain(&imported.via)
                    .chain(std::iter::once(&imported.name))
                    .cloned()
                    .collect();
                if let Some(target) = qualified.get(&joined(&full)) {
                    let local = imported.local.as_ref().unwrap_or(&imported.name);
                    aliases.insert(local.text.clone(), target.clone());
                }
            }
        }
        if aliases.is_empty() && qualified.is_empty() {
            continue;
        }
        for item in &mut module.items {
            let Item::Impl(implementation) = item else {
                continue;
            };
            for _ in 0..=aliases.len() + qualified.len() {
                let Type::Path(path) = &implementation.target else {
                    break;
                };
                let target = match path.segments.as_slice() {
                    [segment] => aliases.get(&segment.text),
                    segments => qualified.get(&joined(segments)),
                };
                let Some(target) = target else {
                    break;
                };
                implementation.target = target.clone();
            }
        }
    }
}

/// A struct's own `impl Default` is its default everywhere (spec 3.29):
/// `let p: Pt;` and `Pt()` are written `Pt::default()`, which both lowering
/// paths call. They built the structural all-zero default instead (#569).
/// Matched by name: `T::default()` on a same-named struct without the impl
/// is its structural default, so a collision changes nothing.
pub fn struct_defaults(modules: &mut [Module]) {
    let with_fields: HashSet<String> = modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            Item::Struct(declaration) if !declaration.fields.is_empty() => {
                Some(declaration.name.text.clone())
            }
            _ => None,
        })
        .collect();
    let defaulted: HashSet<String> = modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            Item::Impl(implementation)
                if implementation
                    .trait_
                    .as_ref()
                    .and_then(|path| path.segments.last())
                    .is_some_and(|name| name.text == "Default") =>
            {
                let Type::Path(target) = &implementation.target else {
                    return None;
                };
                let name = &target.segments.last()?.text;
                with_fields.contains(name).then(|| name.clone())
            }
            _ => None,
        })
        .collect();
    if defaulted.is_empty() {
        return;
    }
    let named = |path: &Path| {
        path.segments
            .last()
            .is_some_and(|name| defaulted.contains(&name.text))
    };
    let default_of = |path: &Path| {
        let mut path = path.clone();
        path.segments.push(Ident {
            text: "default".to_string(),
            span: path.span,
        });
        path
    };
    fn lets(statements: &mut [Stmt], f: &mut impl FnMut(&mut LetDecl)) {
        for statement in statements {
            match statement {
                Stmt::Let(declaration) => f(declaration),
                Stmt::Process(process) => lets(&mut process.body.stmts, f),
                Stmt::If(chain) => {
                    let mut chain = Some(chain);
                    while let Some(current) = chain.take() {
                        lets(&mut current.then.stmts, f);
                        match current.else_.as_deref_mut() {
                            Some(ElseBranch::Block(block)) => lets(&mut block.stmts, f),
                            Some(ElseBranch::If(nested)) => chain = Some(nested),
                            None => {}
                        }
                    }
                }
                Stmt::Match(nested) => {
                    for arm in &mut nested.arms {
                        lets(&mut arm.body.stmts, f);
                    }
                }
                Stmt::For { body, .. } | Stmt::While { body, .. } => lets(&mut body.stmts, f),
                _ => {}
            }
        }
    }
    fn member_lets(member: &mut ImplItem, f: &mut impl FnMut(&mut LetDecl)) {
        match member {
            ImplItem::Let(declaration) => f(declaration),
            ImplItem::Fn(function) => {
                if let Some(body) = &mut function.body {
                    lets(&mut body.stmts, f);
                }
            }
            ImplItem::Process(process) => lets(&mut process.body.stmts, f),
            ImplItem::Stmt(statement) => lets(std::slice::from_mut(statement), f),
            ImplItem::Block(block) => {
                for item in &mut block.items {
                    member_lets(item, f);
                }
            }
            _ => {}
        }
    }
    let mut give_default = |declaration: &mut LetDecl| {
        if declaration.value.is_some() {
            return;
        }
        let Some(Type::Path(path)) = &declaration.ty else {
            return;
        };
        if named(path) {
            declaration.value = Some(Expr::Call {
                callee: Box::new(Expr::Path(default_of(path))),
                type_args: Vec::new(),
                qualifier: None,
                args: Vec::new(),
                bang: false,
                span: declaration.span,
            });
        }
    };
    for module in modules.iter_mut() {
        for item in &mut module.items {
            match item {
                Item::Fn(function) => {
                    if let Some(body) = &mut function.body {
                        lets(&mut body.stmts, &mut give_default);
                    }
                }
                // The default itself builds its value; `let p: Pt;` there
                // would call itself.
                Item::Impl(implementation)
                    if implementation
                        .trait_
                        .as_ref()
                        .and_then(|path| path.segments.last())
                        .is_some_and(|name| name.text == "Default") => {}
                Item::Impl(implementation) => {
                    for member in &mut implementation.items {
                        member_lets(member, &mut give_default);
                    }
                }
                _ => {}
            }
            crate::syntax::walk::walk_item(item, &mut |e| {
                if let Expr::Call {
                    callee,
                    type_args,
                    args,
                    bang: false,
                    ..
                } = e
                {
                    if let Expr::Path(path) = callee.as_mut() {
                        if args.is_empty() && type_args.is_empty() && named(path) {
                            *path = default_of(path);
                        }
                    }
                }
                true
            });
        }
    }
}

/// `Self::make(42)` in `impl Foo` is `Foo::make(42)`: `Self` resolved as a
/// local, so a static call through it found no function and did not lower
/// (#659). Written out once, before resolution, for an impl whose target is
/// a plain path; a generic impl keeps `Self`.
pub fn self_paths(modules: &mut [Module]) {
    for module in modules {
        for item in &mut module.items {
            let Item::Impl(implementation) = item else {
                continue;
            };
            let Type::Path(target) = &implementation.target else {
                continue;
            };
            let target = target.segments.clone();
            let mut rewrite = |e: &mut Expr| {
                if let Expr::Path(path) = e {
                    if path.segments.len() >= 2 && path.segments[0].text == "Self" {
                        let rest = path.segments.split_off(1);
                        path.segments = target.iter().cloned().chain(rest).collect();
                    }
                }
                true
            };
            for member in &mut implementation.items {
                crate::syntax::walk::walk_impl_item(member, &mut rewrite);
            }
        }
    }
}

/// `std::bits::resize`, `bits::resize`, `std::primitive::integer`,
/// `rand::randint`: a builtin named through the std module that provides it
/// is the builtin itself, but lowering recognized only the bare one-segment
/// name, so a qualified call passed the checker and crashed hardware or
/// testbench lowering (#546, also #524, #499). Each is written bare here,
/// once, before resolution. A module of the program's own named `bits`,
/// `primitive`, `rand` or `fs` keeps its two-segment calls (`fs::read`): they
/// name its function, not the builtin (#620).
pub fn builtin_paths(modules: &mut [Module]) {
    let own_modules: HashSet<String> = modules
        .iter()
        .filter(|module| {
            module
                .path
                .segments
                .first()
                .is_some_and(|root| !matches!(root.text.as_str(), "std" | "core"))
        })
        .filter_map(|module| module.path.segments.last().map(|s| s.text.clone()))
        .collect();
    fn builtin_module(name: &str) -> Option<&'static str> {
        match name {
            "resize" => Some("bits"),
            "integer" | "natural" | "real" | "Char" => Some("primitive"),
            "rand" | "randint" | "uniform" | "seed" => Some("rand"),
            "read" | "exists" => Some("fs"),
            _ => None,
        }
    }
    let mut rewrite = |e: &mut Expr| {
        if let Expr::Path(path) = e {
            let qualified = match path.segments.as_slice() {
                [module, name] => {
                    builtin_module(&name.text) == Some(module.text.as_str())
                        && !own_modules.contains(&module.text)
                }
                [root, module, name] => {
                    root.text == "std" && builtin_module(&name.text) == Some(module.text.as_str())
                }
                _ => false,
            };
            if qualified {
                let name = path
                    .segments
                    .pop()
                    .expect("a qualified path has a last segment");
                path.segments = vec![name];
            }
        }
        true
    };
    for module in modules {
        for item in &mut module.items {
            crate::syntax::walk::walk_item(item, &mut rewrite);
        }
    }
}

/// Rewrite every entity method and every call to one.
pub fn entity_methods(modules: &mut [Module], sources: &mut SourceMap, sink: &mut DiagnosticSink) {
    let methods = collect(modules);
    if methods.is_empty() {
        return;
    }
    let entity_params = entity_params(modules);
    let tests = test_entities(modules);
    let signals = entity_signals(modules);
    for module in modules.iter_mut() {
        // Each module's generated names live in a file of its own, counted
        // as that module's, so privacy treats them as written there.
        let file = sources.add("<entity methods>", "");
        module.derived_files.push(file);
        let mut spans = Spans {
            file,
            text: String::new(),
        };
        for item in &mut module.items {
            match item {
                Item::Entity(entity) => {
                    if let Some(methods) = methods.get(&entity.name.text) {
                        for method in methods {
                            add_ports(entity, method, &mut spans);
                        }
                    }
                }
                Item::Impl(implementation) => {
                    let Some(target) = head_name(&implementation.target) else {
                        continue;
                    };
                    if let Some(own) = methods.get(&target) {
                        let declared = signals.get(&target).cloned().unwrap_or_default();
                        rewrite_child(implementation, own, &declared, &mut spans, sink);
                    }
                    rewrite_callers(
                        implementation,
                        &methods,
                        &entity_params,
                        tests.contains(&target),
                        &mut spans,
                        sink,
                    );
                }
                _ => {}
            }
        }
        sources.replace_text(file, spans.text);
    }
}

/// The last segment of a type's head path.
fn head_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(path) => path.segments.last().map(|s| s.text.clone()),
        Type::Generic { base, .. } => head_name(base),
        _ => None,
    }
}

/// Entities marked `#[test]`.
fn test_entities(modules: &[Module]) -> HashSet<String> {
    modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            Item::Entity(entity)
                if entity.attrs.iter().any(|attr| {
                    attr.name.segments.len() == 1 && attr.name.segments[0].text == "test"
                }) =>
            {
                Some(entity.name.text.clone())
            }
            _ => None,
        })
        .collect()
}

/// Each entity's generic parameter names, in order.
fn entity_params(modules: &[Module]) -> HashMap<String, Vec<String>> {
    modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            Item::Entity(entity) => Some((
                entity.name.text.clone(),
                entity
                    .params
                    .params
                    .iter()
                    .map(|p| p.name.text.clone())
                    .collect(),
            )),
            _ => None,
        })
        .collect()
}

/// Every entity's public members with a receiver.
fn collect(modules: &[Module]) -> HashMap<String, Vec<Method>> {
    let entities: HashSet<String> = modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            Item::Entity(entity) => Some(entity.name.text.clone()),
            _ => None,
        })
        .collect();
    let mut methods: HashMap<String, Vec<Method>> = HashMap::new();
    for module in modules {
        for item in &module.items {
            let Item::Impl(implementation) = item else {
                continue;
            };
            if implementation.trait_.is_some() {
                continue;
            }
            let Some(target) = head_name(&implementation.target).filter(|t| entities.contains(t))
            else {
                continue;
            };
            for member in &implementation.items {
                let ImplItem::Fn(function) = member else {
                    continue;
                };
                if !is_method(function) {
                    continue;
                }
                methods.entry(target.clone()).or_default().push(Method {
                    name: function.name.text.clone(),
                    params: function
                        .params
                        .iter()
                        .filter(|p| !p.is_self)
                        .filter_map(|p| Some((p.name.as_ref()?.text.clone(), p.ty.clone()?)))
                        .collect(),
                    ret: function.ret.clone(),
                });
            }
        }
    }
    methods
}

/// A public member with a receiver.
fn is_method(function: &FnDecl) -> bool {
    function.is_pub && !function.is_static && function.params.iter().any(|p| p.is_self)
}

/// The ports a method's calls cross on.
fn add_ports(entity: &mut EntityDecl, method: &Method, spans: &mut Spans) {
    let port = |spans: &mut Spans, name: String, ty: Type, dir: Direction| {
        let name = spans.ident(&name);
        Port {
            dir: Some(dir),
            span: name.span,
            name,
            ty,
            attrs: Vec::new(),
        }
    };
    let bit = bit_type(spans);
    entity.ports.push(port(
        spans,
        format!("{}#called", method.name),
        bit,
        Direction::In,
    ));
    for (name, ty) in &method.params {
        entity.ports.push(port(
            spans,
            format!("{}#{name}", method.name),
            ty.clone(),
            Direction::In,
        ));
    }
    if let Some(ret) = &method.ret {
        entity.ports.push(port(
            spans,
            format!("{}#ret", method.name),
            ret.clone(),
            Direction::Out,
        ));
    }
}

fn bit_type(spans: &mut Spans) -> Type {
    let ident = spans.ident("Bit");
    Type::Path(Path {
        span: ident.span,
        segments: vec![ident],
    })
}

/// Replace each method of the child with its process.
/// Every signal an entity's methods may write, with its declared type, for
/// the defaults: its ports and the `let`s of all its implementations.
fn entity_signals(modules: &[Module]) -> HashMap<String, HashMap<String, Type>> {
    let mut signals: HashMap<String, HashMap<String, Type>> = HashMap::new();
    for item in modules.iter().flat_map(|module| &module.items) {
        match item {
            Item::Entity(entity) => {
                let own = signals.entry(entity.name.text.clone()).or_default();
                for port in &entity.ports {
                    own.insert(port.name.text.clone(), port.ty.clone());
                }
            }
            Item::Impl(implementation) => {
                let Some(target) = head_name(&implementation.target) else {
                    continue;
                };
                let own = signals.entry(target).or_default();
                for member in &implementation.items {
                    if let ImplItem::Let(declaration) = member {
                        if let Some(ty) = &declaration.ty {
                            own.insert(declaration.name.text.clone(), ty.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    signals
}

fn rewrite_child(
    implementation: &mut ImplDecl,
    methods: &[Method],
    declared: &HashMap<String, Type>,
    spans: &mut Spans,
    sink: &mut DiagnosticSink,
) {
    let names: HashSet<&str> = methods.iter().map(|m| m.name.as_str()).collect();
    let mut generated = Vec::new();
    implementation.items.retain_mut(|item| {
        let ImplItem::Fn(function) = item else {
            return true;
        };
        if !is_method(function) || !names.contains(function.name.text.as_str()) {
            return true;
        }
        if let Some(body) = function.body.take() {
            generated.extend(method_process(function, body, declared, spans, sink));
        }
        false
    });
    implementation.items.extend(generated);
}

/// `m#call: process { defaults; if m#called == '1' { body } }` and the
/// never-written signals the defaults read.
fn method_process(
    function: &FnDecl,
    body: Block,
    declared: &HashMap<String, Type>,
    spans: &mut Spans,
    sink: &mut DiagnosticSink,
) -> Vec<ImplItem> {
    let name = function.name.text.clone();
    let params: HashSet<String> = function
        .params
        .iter()
        .filter_map(|p| p.name.as_ref().map(|n| n.text.clone()))
        .collect();
    let mut stmts = body.stmts;
    // `return e;` is the result; it ends the body.
    let ret = match stmts.last() {
        Some(Stmt::Return { value: Some(_), .. }) => match stmts.pop() {
            Some(Stmt::Return { value, .. }) => value,
            _ => None,
        },
        _ => None,
    };
    if function.ret.is_some() && ret.is_none() {
        sink.emit(
            Diagnostic::error(format!(
                "entity method `{name}` must end with `return` of its result"
            ))
            .at(function.name.span),
        );
    }
    let mut early = false;
    for statement in &stmts {
        early |= contains_return(statement);
    }
    if early {
        sink.emit(
            Diagnostic::error(format!(
                "entity method `{name}` returns only at the end of its body"
            ))
            .at(function.name.span)
            .help("compute the result into a local and return it last"),
        );
    }
    // Parameters are the call's ports; `self.x` is the entity's own `x`.
    let mut rename = |e: &mut Expr| {
        match e {
            Expr::Path(path)
                if path.segments.len() == 1 && params.contains(&path.segments[0].text) =>
            {
                path.segments[0].text = format!("{name}#{}", path.segments[0].text);
            }
            Expr::Field { base, field, span } if matches!(base.as_ref(), Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == "self") =>
            {
                *e = Expr::Path(Path {
                    segments: vec![field.clone()],
                    span: *span,
                });
            }
            _ => {}
        }
        true
    };
    for statement in &mut stmts {
        crate::syntax::walk::walk_stmt(statement, &mut rename);
    }
    let ret = ret.map(|mut value| {
        crate::syntax::walk::walk_expr(&mut value, &mut rename);
        value
    });
    // Every signal the body writes falls back to its default when the
    // method is not called; locals the body declares are its own.
    let mut locals = HashSet::new();
    let mut written = Vec::new();
    for statement in &stmts {
        written_roots(statement, &mut locals, &mut written);
    }
    let mut items = Vec::new();
    let mut process_body = Vec::new();
    for target in written.iter().filter(|w| !locals.contains(*w)) {
        let Some(ty) = declared.get(target) else {
            continue;
        };
        let none = format!("{name}#none#{target}");
        items.push(ImplItem::Let(spans.let_decl(&none, ty)));
        let value = spans.path(&none);
        process_body.push(spans.assign(target, value));
    }
    if let (Some(ty), true) = (&function.ret, ret.is_some()) {
        let none = format!("{name}#none#ret");
        items.push(ImplItem::Let(spans.let_decl(&none, ty)));
        let value = spans.path(&none);
        process_body.push(spans.assign(&format!("{name}#ret"), value));
    }
    if let Some(value) = ret {
        stmts.push(spans.assign(&format!("{name}#ret"), value));
    }
    let called = spans.path(&format!("{name}#called"));
    let one = spans.bit('1');
    let cond_span = crate::syntax::ast::expr_span(&called);
    process_body.push(Stmt::If(IfStmt {
        label: None,
        cond: Expr::Binary {
            op: BinOp::Eq,
            lhs: Box::new(called),
            rhs: Box::new(one),
            span: cond_span,
        },
        then: Block {
            stmts,
            span: body.span,
        },
        else_: None,
        span: body.span,
    }));
    items.push(ImplItem::Process(ProcessDecl {
        label: Some(spans.ident(&format!("{name}#call"))),
        span: function.span,
        body: Block {
            stmts: process_body,
            span: body.span,
        },
    }));
    items
}

fn contains_return(statement: &Stmt) -> bool {
    match statement {
        Stmt::Return { .. } => true,
        Stmt::If(chain) => {
            chain.then.stmts.iter().any(contains_return)
                || match chain.else_.as_deref() {
                    Some(ElseBranch::Block(block)) => block.stmts.iter().any(contains_return),
                    Some(ElseBranch::If(next)) => contains_return(&Stmt::If(next.clone())),
                    None => false,
                }
        }
        Stmt::Match(m) => m
            .arms
            .iter()
            .any(|arm| arm.body.stmts.iter().any(contains_return)),
        Stmt::For { body, .. } | Stmt::While { body, .. } => body.stmts.iter().any(contains_return),
        _ => false,
    }
}

/// The root names a statement assigns, and the locals it declares.
fn written_roots(statement: &Stmt, locals: &mut HashSet<String>, out: &mut Vec<String>) {
    match statement {
        Stmt::Let(declaration) => {
            locals.insert(declaration.name.text.clone());
        }
        Stmt::Assign { target, .. } => {
            let mut root = target;
            while let Expr::Index { base, .. } | Expr::Field { base, .. } = root {
                root = base;
            }
            if let Expr::Path(path) = root {
                if let [segment] = path.segments.as_slice() {
                    if !out.contains(&segment.text) {
                        out.push(segment.text.clone());
                    }
                }
            }
        }
        Stmt::If(chain) => {
            let mut link = Some(chain);
            while let Some(current) = link {
                for inner in &current.then.stmts {
                    written_roots(inner, locals, out);
                }
                link = match current.else_.as_deref() {
                    Some(ElseBranch::If(next)) => Some(next),
                    Some(ElseBranch::Block(block)) => {
                        for inner in &block.stmts {
                            written_roots(inner, locals, out);
                        }
                        None
                    }
                    None => None,
                };
            }
        }
        Stmt::Match(m) => {
            for arm in &m.arms {
                for inner in &arm.body.stmts {
                    written_roots(inner, locals, out);
                }
            }
        }
        Stmt::For { body, .. } | Stmt::While { body, .. } => {
            for inner in &body.stmts {
                written_roots(inner, locals, out);
            }
        }
        _ => {}
    }
}

/// One instance of an entity with methods, declared in the parent.
struct Instance {
    entity: String,
    /// The instance type's generic arguments, by parameter name.
    args: HashMap<String, GenericArg>,
}

/// Rewrite the calls an implementation makes on its children's methods.
fn rewrite_callers(
    implementation: &mut ImplDecl,
    methods: &HashMap<String, Vec<Method>>,
    entity_params: &HashMap<String, Vec<String>>,
    testbench: bool,
    spans: &mut Spans,
    sink: &mut DiagnosticSink,
) {
    let mut instances: HashMap<String, Instance> = HashMap::new();
    for item in &implementation.items {
        let ImplItem::Let(declaration) = item else {
            continue;
        };
        let Some(ty) = &declaration.ty else {
            continue;
        };
        let Some(entity) = head_name(ty).filter(|name| methods.contains_key(name)) else {
            continue;
        };
        let mut args = HashMap::new();
        if let Type::Generic { args: given, .. } = ty {
            let names = entity_params.get(&entity).cloned().unwrap_or_default();
            for (index, arg) in given.iter().enumerate() {
                match arg {
                    GenericArg::Named { name, .. } | GenericArg::NamedType { name, .. } => {
                        args.insert(name.text.clone(), arg.clone());
                    }
                    GenericArg::Positional(_) | GenericArg::PositionalType(_) => {
                        if let Some(name) = names.get(index) {
                            args.insert(name.clone(), arg.clone());
                        }
                    }
                }
            }
        }
        instances.insert(declaration.name.text.clone(), Instance { entity, args });
    }
    if instances.is_empty() {
        return;
    }
    let mut context = Callers {
        instances: &instances,
        methods,
        testbench,
        sites: HashMap::new(),
        used: Vec::new(),
        unconditional: HashSet::new(),
        depth: 0,
        clocked: 0,
        generated: Vec::new(),
        spans,
        sink,
    };
    for item in &mut implementation.items {
        match item {
            ImplItem::Process(process) => context.process(&mut process.body),
            ImplItem::Stmt(statement) => {
                let mut pre = Vec::new();
                context.statement(statement, &mut pre);
                context
                    .generated
                    .extend(pre.into_iter().map(ImplItem::Stmt));
            }
            _ => {}
        }
    }
    let mut generated = std::mem::take(&mut context.generated);
    // Hidden signals and their wiring into each instance's connections.
    for (instance, method) in context.used.clone() {
        let found = &instances[&instance];
        let Some(signature) = methods[&found.entity].iter().find(|m| m.name == method) else {
            continue;
        };
        let mut connections = Vec::new();
        let called = format!("{instance}#{method}#called");
        let bit = bit_type(context.spans);
        generated.push(ImplItem::Let(context.spans.let_decl(&called, &bit)));
        connections.push((format!("{method}#called"), called));
        for (name, ty) in &signature.params {
            let hidden = format!("{instance}#{method}#{name}");
            let ty = substitute(ty, &found.args);
            generated.push(ImplItem::Let(context.spans.let_decl(&hidden, &ty)));
            connections.push((format!("{method}#{name}"), hidden));
        }
        if let Some(ret) = &signature.ret {
            let hidden = format!("{instance}#{method}#ret");
            let ty = substitute(ret, &found.args);
            generated.push(ImplItem::Let(context.spans.let_decl(&hidden, &ty)));
            connections.push((format!("{method}#ret"), hidden));
        }
        for item in &mut implementation.items {
            let ImplItem::Let(declaration) = item else {
                continue;
            };
            if declaration.name.text != instance {
                continue;
            }
            // `= {}` parses as an empty concatenation; it is an empty
            // connection list here.
            if let Some(Expr::Concat { parts, span }) = &declaration.value {
                if parts.is_empty() {
                    declaration.value = Some(Expr::Construct {
                        ty: None,
                        args: Vec::new(),
                        spread: None,
                        span: *span,
                    });
                }
            }
            let value = declaration.value.get_or_insert_with(|| Expr::Construct {
                ty: None,
                args: Vec::new(),
                spread: None,
                span: declaration.span,
            });
            if let Expr::Construct { args, .. } = value {
                for (port, signal) in &connections {
                    let field = context.spans.ident(port);
                    let value = context.spans.path(signal);
                    args.push(ConnectArg {
                        span: field.span,
                        field: Some(field),
                        value: Some(value),
                    });
                }
            }
        }
    }
    implementation.items.splice(0..0, generated);
}

/// Replace the instance's generic parameters in a method's port type.
/// A type parameter (`T`) becomes the argument read as a type; a value
/// parameter (`W`) inside a width becomes the argument's expression.
fn substitute(ty: &Type, args: &HashMap<String, GenericArg>) -> Type {
    if let Type::Path(path) = ty {
        if let [segment] = path.segments.as_slice() {
            if let Some(replaced) = args.get(&segment.text).and_then(GenericArg::as_type) {
                return replaced;
            }
        }
    }
    let mut ty = match ty {
        Type::Indexed { base, index, span } => Type::Indexed {
            base: Box::new(substitute(base, args)),
            index: index.clone(),
            span: *span,
        },
        Type::Generic {
            base,
            args: inner,
            span,
        } => Type::Generic {
            base: Box::new(substitute(base, args)),
            args: inner
                .iter()
                .map(|arg| match arg {
                    GenericArg::PositionalType(t) => {
                        GenericArg::PositionalType(substitute(t, args))
                    }
                    GenericArg::NamedType { name, ty } => GenericArg::NamedType {
                        name: name.clone(),
                        ty: substitute(ty, args),
                    },
                    other => other.clone(),
                })
                .collect(),
            span: *span,
        },
        other => other.clone(),
    };
    crate::syntax::walk::walk_type(&mut ty, &mut |e| {
        if let Expr::Path(path) = e {
            if let [segment] = path.segments.as_slice() {
                if let Some(GenericArg::Positional(value) | GenericArg::Named { value, .. }) =
                    args.get(&segment.text)
                {
                    *e = value.clone();
                    return false;
                }
            }
        }
        true
    });
    ty
}

struct Callers<'a, 'b> {
    instances: &'a HashMap<String, Instance>,
    methods: &'a HashMap<String, Vec<Method>>,
    testbench: bool,
    /// Where each `(instance, method)` is called, for the one-site rule.
    sites: HashMap<(String, String), Span>,
    /// Each `(instance, method)` called, in first-call order.
    used: Vec<(String, String)>,
    /// Calls made at a process body's top level: they always strobe, so
    /// they need no default.
    unconditional: HashSet<(String, String)>,
    /// How deep inside a process body's blocks the walk is.
    depth: usize,
    /// How many clock-edge branches enclose the walk.
    clocked: usize,
    /// Hidden signals and concurrent strobes the rewrite adds to the parent.
    generated: Vec<ImplItem>,
    spans: &'b mut Spans,
    sink: &'b mut DiagnosticSink,
}

impl Callers<'_, '_> {
    /// A process, at the implementation root or built by a generate block:
    /// its top-level statements always run, and it defaults the strobes it
    /// drives, so no call means no strobe.
    fn process(&mut self, body: &mut Block) {
        let before = self.used.len();
        let depth = std::mem::replace(&mut self.depth, 0);
        let clocked = std::mem::replace(&mut self.clocked, 0);
        self.walk(body);
        self.depth = depth;
        self.clocked = clocked;
        let mut defaults = Vec::new();
        for index in before..self.used.len() {
            let (instance, method) = self.used[index].clone();
            defaults.extend(self.defaults(&instance, &method));
        }
        body.stmts.splice(0..0, defaults);
    }

    /// A nested block: the statements in it run under a condition.
    fn block(&mut self, block: &mut Block) {
        self.depth += 1;
        self.walk(block);
        self.depth -= 1;
    }

    /// Rewrite the calls in a block, statement by statement.
    fn walk(&mut self, block: &mut Block) {
        let mut index = 0;
        while index < block.stmts.len() {
            let mut pre = Vec::new();
            self.statement(&mut block.stmts[index], &mut pre);
            let count = pre.len();
            if count > 0 {
                block.stmts.splice(index..index, pre);
            }
            // A procedure call statement leaves nothing of its own.
            if matches!(&block.stmts[index + count], Stmt::Expr(Expr::Path(p)) if p.segments.is_empty())
            {
                block.stmts.remove(index + count);
                index += count;
                continue;
            }
            index += count + 1;
        }
    }

    /// Rewrite the calls in one statement's own expressions, gathering the
    /// strobes they need set just before it; nested blocks are handled as
    /// blocks.
    fn statement(&mut self, statement: &mut Stmt, pre: &mut Vec<Stmt>) {
        match statement {
            Stmt::Expr(Expr::Call { .. }) => {
                if let Stmt::Expr(expression) = statement {
                    if self.call(expression, pre, true) {
                        // Mark for removal by the block walker.
                        *expression = Expr::Path(Path {
                            segments: Vec::new(),
                            span: crate::syntax::ast::expr_span(expression),
                        });
                        return;
                    }
                    self.expression(expression, pre);
                }
            }
            Stmt::Expr(expression) => self.expression(expression, pre),
            Stmt::Assign { target, value, .. } => {
                self.expression(target, pre);
                self.expression(value, pre);
            }
            Stmt::Let(declaration) => {
                if let Some(value) = &mut declaration.value {
                    self.expression(value, pre);
                }
            }
            Stmt::If(chain) => {
                self.expression(&mut chain.cond, pre);
                // A branch under a clock edge, or an asynchronous reset ahead
                // of one, is clocked logic.
                let clocked = chain_has_edge(chain);
                self.clocked += usize::from(clocked);
                self.block(&mut chain.then);
                self.clocked -= usize::from(clocked);
                let mut link = chain.else_.as_deref_mut();
                while let Some(branch) = link {
                    match branch {
                        ElseBranch::Block(block) => {
                            self.block(block);
                            link = None;
                        }
                        ElseBranch::If(next) => {
                            self.expression(&mut next.cond, pre);
                            let clocked = chain_has_edge(next);
                            self.clocked += usize::from(clocked);
                            self.block(&mut next.then);
                            self.clocked -= usize::from(clocked);
                            link = next.else_.as_deref_mut();
                        }
                    }
                }
            }
            // A process a generate block builds is a process like any other.
            Stmt::Process(process) => self.process(&mut process.body),
            Stmt::Match(m) => {
                self.expression(&mut m.scrutinee, pre);
                for arm in &mut m.arms {
                    self.block(&mut arm.body);
                }
            }
            Stmt::For { range, body, .. } => {
                self.expression(range, pre);
                self.in_loop(body);
            }
            Stmt::While { cond, body, .. } => {
                self.expression(cond, pre);
                self.in_loop(body);
            }
            Stmt::Return {
                value: Some(value), ..
            } => self.expression(value, pre),
            _ => {}
        }
    }

    /// A loop body runs more than once: a call in it is several call sites.
    fn in_loop(&mut self, body: &mut Block) {
        let mut probe = body.clone();
        let mut found = Vec::new();
        for statement in &mut probe.stmts {
            crate::syntax::walk::walk_stmt(statement, &mut |e| {
                if let Some((instance, method, span)) = self.method_call(e) {
                    found.push((instance, method, span));
                }
                true
            });
        }
        for (instance, method, span) in found {
            self.sink.emit(
                Diagnostic::error(format!(
                    "`{instance}.{method}` is called inside a loop, which calls it more than once"
                ))
                .at(span)
                .help("an entity method has one call site per instance; call it outside the loop"),
            );
        }
    }

    /// Replace every method call inside an expression by its result. A
    /// call in a branch of an `if` expression or a `match` would be hoisted
    /// out of its condition, so it is reported instead.
    fn expression(&mut self, expression: &mut Expr, pre: &mut Vec<Stmt>) {
        let mut conditional = Vec::new();
        crate::syntax::walk::walk_expr(&mut expression.clone(), &mut |e| {
            let branches: Vec<&mut Expr> = match e {
                Expr::IfExpr { then, els, .. } => vec![then.as_mut(), els.as_mut()],
                Expr::Match { arms, .. } => arms
                    .iter_mut()
                    .flat_map(|arm| arm.body.stmts.iter_mut())
                    .filter_map(|statement| match statement {
                        Stmt::Expr(value) => Some(value),
                        _ => None,
                    })
                    .collect(),
                _ => return true,
            };
            for branch in branches {
                crate::syntax::walk::walk_expr(branch, &mut |inner| {
                    if let Some(found) = self.method_call(inner) {
                        conditional.push(found);
                    }
                    true
                });
            }
            true
        });
        if !conditional.is_empty() {
            for (instance, method, span) in conditional {
                self.sink.emit(
                    Diagnostic::error(format!(
                        "`{instance}.{method}` is called in a branch of a conditional expression"
                    ))
                    .at(span)
                    .help(
                        "a method call strobes whenever its statement runs: call it in an `if` \
                         statement instead, `if c { x = fifo.pop(); } else { x = 0; }`",
                    ),
                );
            }
            return;
        }
        crate::syntax::walk::walk_expr(expression, &mut |e| {
            if self.call(e, pre, false) {
                return false;
            }
            true
        });
    }

    /// `instance.method(args)` on a direct child, with its span.
    fn method_call(&self, e: &Expr) -> Option<(String, String, Span)> {
        let Expr::Call {
            callee,
            bang: false,
            span,
            ..
        } = e
        else {
            return None;
        };
        let Expr::Field { base, field, .. } = callee.as_ref() else {
            return None;
        };
        let Expr::Path(path) = base.as_ref() else {
            return None;
        };
        let [instance] = path.segments.as_slice() else {
            return None;
        };
        let found = self.instances.get(&instance.text)?;
        self.methods[&found.entity]
            .iter()
            .any(|m| m.name == field.text)
            .then(|| (instance.text.clone(), field.text.clone(), *span))
    }

    /// Rewrite one call: set its strobe and arguments in `pre`, and stand
    /// for its result. Returns whether `e` was a method call.
    fn call(&mut self, e: &mut Expr, pre: &mut Vec<Stmt>, statement: bool) -> bool {
        let Some((instance, method, span)) = self.method_call(e) else {
            return false;
        };
        if self.testbench {
            self.sink.emit(
                Diagnostic::error(format!(
                    "calling entity method `{instance}.{method}` from a testbench is not supported yet"
                ))
                .at(span)
                .help("drive the instance's ports, or call the method from a hardware entity"),
            );
            // Reported: handled, so no other walk reports it again.
            return true;
        }
        if self.clocked > 0 {
            self.sink.emit(
                Diagnostic::error(format!(
                    "`{instance}.{method}` is called under a clock edge"
                ))
                .at(span)
                .help(
                    "an entity method is called from combinational logic and the child acts \
                     on its own clock: move the call out of `if clk.rising()`, under the same \
                     condition",
                ),
            );
            return true;
        }
        let key = (instance.clone(), method.clone());
        if let Some(first) = self.sites.get(&key) {
            self.sink.emit(
                Diagnostic::error(format!(
                    "`{instance}.{method}` is already called; an entity method has one call site \
                     per instance"
                ))
                .at(span)
                .label(*first, "first call here")
                .help("merge the calls, choosing the argument by condition"),
            );
            return true;
        }
        self.sites.insert(key.clone(), span);
        if self.depth == 0 {
            self.unconditional.insert(key.clone());
        }
        self.used.push(key);
        let entity = &self.instances[&instance].entity;
        let signature = self.methods[entity]
            .iter()
            .find(|m| m.name == method)
            .cloned()
            .expect("found by method_call");
        let Expr::Call { args, .. } = e else {
            unreachable!()
        };
        // A call in an argument (`snk.push(src.pop())`) strobes first.
        for arg in args.iter_mut() {
            self.expression(arg, pre);
        }
        if args.len() != signature.params.len() {
            self.sink.emit(
                Diagnostic::error(format!(
                    "`{instance}.{method}` takes {} argument(s) but {} were given",
                    signature.params.len(),
                    args.len()
                ))
                .at(span),
            );
            return true;
        }
        let one = self.spans.bit('1');
        pre.push(
            self.spans
                .assign(&format!("{instance}#{method}#called"), one),
        );
        for ((name, _), arg) in signature.params.iter().zip(args.iter()) {
            pre.push(
                self.spans
                    .assign(&format!("{instance}#{method}#{name}"), arg.clone()),
            );
        }
        if statement {
            return true;
        }
        if signature.ret.is_none() {
            self.sink.emit(
                Diagnostic::error(format!("`{instance}.{method}` returns no value")).at(span),
            );
        }
        *e = self.spans.path(&format!("{instance}#{method}#ret"));
        true
    }

    /// The defaults a calling process gives what it drives: `'0'` for the
    /// strobe and each argument's type default.
    fn defaults(&mut self, instance: &str, method: &str) -> Vec<Stmt> {
        let found = &self.instances[instance];
        let Some(signature) = self.methods[&found.entity]
            .iter()
            .find(|m| m.name == method)
            .cloned()
        else {
            return Vec::new();
        };
        if self
            .unconditional
            .contains(&(instance.to_string(), method.to_string()))
        {
            return Vec::new();
        }
        let mut defaults = Vec::new();
        let zero = self.spans.bit('0');
        defaults.push(
            self.spans
                .assign(&format!("{instance}#{method}#called"), zero),
        );
        for (name, ty) in &signature.params {
            let none = format!("{instance}#{method}#none#{name}");
            let ty = substitute(ty, &found.args);
            self.generated
                .push(ImplItem::Let(self.spans.let_decl(&none, &ty)));
            let value = self.spans.path(&none);
            defaults.push(
                self.spans
                    .assign(&format!("{instance}#{method}#{name}"), value),
            );
        }
        defaults
    }
}

/// Whether a condition is a clock edge: `.rising()`, `.falling()`,
/// `.edge()` or `'event`, possibly combined with other terms.
fn is_edge(cond: &Expr) -> bool {
    let text = crate::syntax::pretty::expr_string(cond);
    [".rising()", ".falling()", ".edge()", "'event"]
        .iter()
        .any(|edge| text.contains(edge))
}

/// Whether this link of an `if` chain, or a later one, tests an edge: the
/// link is clocked logic, its own edge or an asynchronous reset ahead of one.
fn chain_has_edge(chain: &IfStmt) -> bool {
    is_edge(&chain.cond)
        || match chain.else_.as_deref() {
            Some(ElseBranch::If(next)) => chain_has_edge(next),
            _ => false,
        }
}
