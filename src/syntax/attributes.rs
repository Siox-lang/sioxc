//! Declarative attributes: bindings and reads (spec 3.5).
//!
//! Metadata is declared once (`attr keep: Bool for let, port = false;`),
//! bound from outside the declaration it describes (`attr keep for probe =
//! true;`, or objectless `attr precedence = 40;` for the enclosing item), and
//! read back with the tick (`probe'keep`). This pass runs once over every
//! loaded module, after parsing and before name resolution:
//!
//! - **Bindings** are copied onto their target as an applied [`Attr`], the
//!   form every later stage already reads (target and value checks, the
//!   elaborated `Instance::attrs`, test discovery). The binding item stays in
//!   the tree for the printer and is otherwise ignored.
//! - **Reads** of a declared attribute are replaced by the value they denote:
//!   the binding on that object, else (for an instance) the binding on its
//!   entity, else the declaration's default. Bindings are elaboration-time
//!   constants, so a read is folded here rather than carried as an operation.
//!
//! Objects are found by name: a named binding inside an implementation names
//! one of that entity's `let`s (in any of its implementation blocks in the
//! module) or one of its ports; at module level it names an entity. An
//! objectless binding binds the implementation when the attribute is declared
//! `for impl`, and otherwise the entity the implementation is for — which is
//! how entity metadata is written, since an entity body holds only ports.
//!
//! Acceptance: a binding naming nothing is `E-P029`; binding one attribute
//! twice on one declaration is `E-P030` (a later binding never overrides); a
//! read with neither a binding nor a default is `E-P031`.

use std::collections::{HashMap, HashSet};

use crate::diag::{codes, Diagnostic, DiagnosticSink, Span};
use crate::syntax::ast::*;

/// What a declaration says about one attribute.
struct Declared {
    targets: Vec<String>,
    default: Option<Expr>,
}

/// Where a binding lands, by index into its module's items.
enum Target {
    Entity(usize),
    Impl(usize),
    Let { item: usize, member: usize },
    Port { item: usize, port: usize },
}

/// Attach every attribute binding to its target, then fold every read of a
/// declared attribute into its value. See the module documentation.
pub fn attach(modules: &mut [Module], sink: &mut DiagnosticSink) {
    let declared = declarations(modules);
    for module in modules.iter_mut() {
        bind_module(module, &declared, sink);
    }
    let entity_attrs = entity_attrs(modules);
    let defaults = declared_by_module(modules);
    for index in 0..modules.len() {
        let visible = visible_attrs(&modules[index], modules);
        let names: HashSet<&str> = declared
            .keys()
            .map(String::as_str)
            .filter(|name| visible.contains(*name))
            .collect();
        if names.is_empty() {
            continue;
        }
        let reads = Reads {
            names: &names,
            declared: &declared,
            defaults: &defaults,
            entity_attrs: &entity_attrs,
            within: Within::of(&modules[index]),
        };
        reads.fold_module(&mut modules[index], sink);
    }
}

/// Every attribute declaration, by name. The standard ones are seeded so
/// bindings of them work even before `std::attrs` is loaded, mirroring the
/// type checker's seeds.
fn declarations(modules: &[Module]) -> HashMap<String, Declared> {
    let mut out = HashMap::new();
    for (name, targets) in [
        ("test", &["entity"][..]),
        ("keep", &["let", "port"][..]),
        ("library", &["entity"][..]),
        ("name", &["entity"][..]),
        ("precedence", &["impl"][..]),
    ] {
        out.insert(
            name.to_string(),
            Declared {
                targets: targets.iter().map(|t| t.to_string()).collect(),
                default: None,
            },
        );
    }
    for module in modules {
        for item in &module.items {
            if let Item::AttrDecl(declaration) = item {
                out.insert(
                    declaration.name.text.clone(),
                    Declared {
                        targets: declaration.targets.iter().map(|t| t.text.clone()).collect(),
                        default: declaration.default.clone(),
                    },
                );
            }
        }
    }
    out
}

/// Attribute names a module can read by their bare name: its own
/// declarations, its imports, and the prelude's re-exports.
fn visible_attrs(module: &Module, modules: &[Module]) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut collect = |module: &Module, only_pub: bool| {
        for item in &module.items {
            match item {
                Item::AttrDecl(declaration) => {
                    out.insert(declaration.name.text.clone());
                }
                Item::Using(using) if !only_pub || using.is_pub => {
                    if let UsingKind::Import { names, .. } = &using.kind {
                        out.extend(names.iter().map(|name| name.binding().text.clone()));
                    }
                }
                _ => {}
            }
        }
    };
    collect(module, false);
    for prelude in ["core::prelude", "std::prelude"] {
        if let Some(prelude) = modules.iter().find(|m| path_is(&m.path, prelude)) {
            collect(prelude, true);
        }
    }
    out
}

/// Every entity's applied attributes after binding, by entity name, with
/// the module declaring each: two modules' `Worker`s are two entities, and
/// a flat map let one's attributes stand for the other's (#649).
fn entity_attrs(modules: &[Module]) -> HashMap<String, Vec<(String, Vec<Attr>)>> {
    let mut out: HashMap<String, Vec<(String, Vec<Attr>)>> = HashMap::new();
    for module in modules {
        for item in &module.items {
            if let Item::Entity(entity) = item {
                out.entry(entity.name.text.clone())
                    .or_default()
                    .push((path_text(&module.path), entity.attrs.clone()));
            }
        }
    }
    out
}

/// Every attribute declaration's default by name, with its module, as
/// [`entity_attrs`] keeps entities apart.
fn declared_by_module(modules: &[Module]) -> HashMap<String, Vec<(String, Option<Expr>)>> {
    let mut out: HashMap<String, Vec<(String, Option<Expr>)>> = HashMap::new();
    for module in modules {
        for item in &module.items {
            if let Item::AttrDecl(declaration) = item {
                out.entry(declaration.name.text.clone())
                    .or_default()
                    .push((path_text(&module.path), declaration.default.clone()));
            }
        }
    }
    out
}

/// Where a module's bare names lead: itself, then what it imports.
struct Within {
    module: String,
    /// `Local` of `use m1::{Local = Worker}`: the module and declared name.
    imports: HashMap<String, (String, String)>,
    /// `m1` of `use m1::*`.
    globs: Vec<String>,
}

impl Within {
    fn of(module: &Module) -> Self {
        let mut imports = HashMap::new();
        let mut globs = Vec::new();
        for item in &module.items {
            if let Item::Using(Using {
                kind: UsingKind::Import { base, names },
                ..
            }) = item
            {
                for name in names {
                    let mut from = path_text(base);
                    for via in &name.via {
                        from = format!("{from}::{}", via.text);
                    }
                    if name.glob {
                        globs.push(from);
                    } else {
                        imports.insert(name.binding().text.clone(), (from, name.name.text.clone()));
                    }
                }
            }
        }
        Within {
            module: path_text(&module.path),
            imports,
            globs,
        }
    }

    /// The one of `candidates` (by declaring module) that `written` names
    /// from here: this module's own, an imported one, one a qualifier
    /// (`m1::Worker`) names, or the only one. `written` may be qualified.
    fn pick<'c, T>(&self, written: &str, candidates: &'c [(String, T)]) -> Option<&'c T> {
        let find = |module: &str| {
            candidates
                .iter()
                .find(|(from, _)| from == module || from.ends_with(&format!("::{module}")))
                .map(|(_, value)| value)
        };
        if let Some((qualifier, _)) = written.rsplit_once("::") {
            return find(qualifier);
        }
        find(&self.module)
            .or_else(|| {
                self.imports
                    .get(written)
                    .and_then(|(module, _)| find(module))
            })
            .or_else(|| self.globs.iter().find_map(|module| find(module)))
            .or_else(|| match candidates {
                [(_, only)] => Some(only),
                _ => candidates.last().map(|(_, value)| value),
            })
    }

    /// The declared name `written` stands for: `Worker` for an imported
    /// `Local = Worker`, the last segment of `m1::Worker`.
    fn declared<'w>(&'w self, written: &'w str) -> &'w str {
        match self.imports.get(written) {
            Some((_, name)) => name,
            None => written.rsplit("::").next().unwrap_or(written),
        }
    }
}

/// Resolve and apply one module's bindings.
fn bind_module(
    module: &mut Module,
    declared: &HashMap<String, Declared>,
    sink: &mut DiagnosticSink,
) {
    let mut plan: Vec<(Target, Attr, Option<Ident>)> = Vec::new();
    for (index, item) in module.items.iter().enumerate() {
        match item {
            // A lang item is read by the resolver (`core`/`std` only).
            Item::AttrBinding(binding)
                if path_is(&binding.name, "lang")
                    && module
                        .path
                        .segments
                        .first()
                        .is_some_and(|root| root.text == "core" || root.text == "std") => {}
            // `lang` naming no entity is the resolver's to judge.
            Item::AttrBinding(binding)
                if path_is(&binding.name, "lang")
                    && binding
                        .object
                        .as_ref()
                        .is_some_and(|object| entity_named(module, &object.text).is_none()) => {}
            Item::AttrBinding(binding) => {
                let target = match &binding.object {
                    Some(object) => entity_named(module, &object.text).map(Target::Entity),
                    None => {
                        sink.emit(
                            Diagnostic::error(format!(
                                "`attr {}` binds nothing here",
                                path_text(&binding.name)
                            ))
                            .with_code(codes::UNKNOWN_ATTR_OBJECT)
                            .at(binding.span)
                            .help(format!(
                                "an objectless binding binds the item it is written in; at \
                                 module level name the entity: `attr {} for <entity> = …;`",
                                path_text(&binding.name)
                            )),
                        );
                        continue;
                    }
                };
                match target {
                    Some(target) => plan.push((target, applied(binding), binding.object.clone())),
                    None => unknown_object(sink, binding, "entity in this module"),
                }
            }
            Item::Impl(implementation) => {
                for member in &implementation.items {
                    let ImplItem::AttrBinding(binding) = member else {
                        continue;
                    };
                    let target = match &binding.object {
                        Some(object) => member_named(module, implementation, &object.text),
                        None => Some(enclosing(module, index, implementation, binding, declared)),
                    };
                    match target {
                        Some(target) => {
                            plan.push((target, applied(binding), binding.object.clone()))
                        }
                        None => unknown_object(sink, binding, "`let` or port of this entity"),
                    }
                }
            }
            _ => {}
        }
    }
    for (target, attr, object) in plan {
        let attrs = match target {
            Target::Entity(item) => match &mut module.items[item] {
                Item::Entity(entity) => &mut entity.attrs,
                _ => continue,
            },
            Target::Impl(item) => match &mut module.items[item] {
                Item::Impl(implementation) => &mut implementation.attrs,
                _ => continue,
            },
            Target::Let { item, member } => match &mut module.items[item] {
                Item::Impl(implementation) => match &mut implementation.items[member] {
                    ImplItem::Let(declaration) => &mut declaration.attrs,
                    _ => continue,
                },
                _ => continue,
            },
            Target::Port { item, port } => match &mut module.items[item] {
                Item::Entity(entity) => &mut entity.ports[port].attrs,
                _ => continue,
            },
        };
        let name = attr_name(&attr);
        if let Some(previous) = attrs.iter().find(|a| attr_name(a) == name) {
            let on = object
                .as_ref()
                .map(|o| format!("`{}`", o.text))
                .unwrap_or_else(|| "this item".to_string());
            sink.emit(
                Diagnostic::error(format!("attribute `{name}` is bound twice on {on}"))
                    .with_code(codes::DUPLICATE_ATTR_BINDING)
                    .at(attr.span)
                    .label(previous.span, "first bound here")
                    .help("a later binding does not override an earlier one; remove one of them"),
            );
            continue;
        }
        attrs.push(attr);
    }
}

/// The applied attribute a binding contributes to its target.
fn applied(binding: &AttrBinding) -> Attr {
    Attr {
        name: binding.name.clone(),
        value: Some(binding.value.clone()),
        span: binding.span,
        directive: false,
    }
}

/// What an objectless binding inside `implementation` binds: the
/// implementation when the attribute targets `impl`, else the entity it
/// implements (an entity's body holds only ports, so its metadata is bound
/// from here). Anything else stays on the implementation for the type checker
/// to report against its declared targets.
fn enclosing(
    module: &Module,
    index: usize,
    implementation: &ImplDecl,
    binding: &AttrBinding,
    declared: &HashMap<String, Declared>,
) -> Target {
    let targets_impl = binding
        .name
        .segments
        .last()
        .and_then(|name| declared.get(&name.text))
        .is_none_or(|d| d.targets.iter().any(|t| t == "impl"));
    if !targets_impl && implementation.trait_.is_none() {
        if let Some(entity) =
            type_name(&implementation.target).and_then(|n| entity_named(module, n))
        {
            return Target::Entity(entity);
        }
    }
    Target::Impl(index)
}

/// The `let` or port called `name` that a binding inside `implementation`
/// names: a `let` in any of the same entity's implementation blocks in this
/// module, then a port of the entity.
fn member_named(module: &Module, implementation: &ImplDecl, name: &str) -> Option<Target> {
    let owner = type_name(&implementation.target)?;
    for (item, candidate) in module.items.iter().enumerate() {
        let Item::Impl(candidate) = candidate else {
            continue;
        };
        if candidate.trait_.is_some() || type_name(&candidate.target) != Some(owner) {
            continue;
        }
        for (member, declaration) in candidate.items.iter().enumerate() {
            if matches!(declaration, ImplItem::Let(l) if l.name.text == name) {
                return Some(Target::Let { item, member });
            }
        }
    }
    let item = entity_named(module, owner)?;
    let Item::Entity(entity) = &module.items[item] else {
        return None;
    };
    let port = entity
        .ports
        .iter()
        .position(|port| port.name.text == name)?;
    Some(Target::Port { item, port })
}

/// The index of the entity called `name` in `module`.
fn entity_named(module: &Module, name: &str) -> Option<usize> {
    module
        .items
        .iter()
        .position(|item| matches!(item, Item::Entity(entity) if entity.name.text == name))
}

/// Report a named binding whose object does not exist where it looks.
fn unknown_object(sink: &mut DiagnosticSink, binding: &AttrBinding, expected: &str) {
    let object = binding
        .object
        .as_ref()
        .map(|o| o.text.as_str())
        .unwrap_or("");
    let span = binding.object.as_ref().map_or(binding.span, |o| o.span);
    sink.emit(
        Diagnostic::error(format!(
            "`attr {} for {object}` names no {expected}",
            path_text(&binding.name)
        ))
        .with_code(codes::UNKNOWN_ATTR_OBJECT)
        .at(span),
    );
}

/// Folding attribute reads in one module.
struct Reads<'r> {
    /// Declared attribute names this module can read.
    names: &'r HashSet<&'r str>,
    declared: &'r HashMap<String, Declared>,
    /// Every declaration's default, by module.
    defaults: &'r HashMap<String, Vec<(String, Option<Expr>)>>,
    /// Every entity's attributes after binding, for instance fallbacks.
    entity_attrs: &'r HashMap<String, Vec<(String, Vec<Attr>)>>,
    /// The module being folded, for which entity or declaration a name means.
    within: Within,
}

impl Reads<'_> {
    /// The attributes of the entity `written` names from this module.
    fn entity(&self, written: &str) -> Option<&Vec<Attr>> {
        let candidates = self.entity_attrs.get(self.within.declared(written))?;
        self.within.pick(written, candidates)
    }
}

/// The names a read can address inside one implementation: its `let`s (with
/// their attributes and, for an instance, its entity) and its ports.
#[derive(Default)]
struct Scope {
    members: HashMap<String, (Vec<Attr>, Option<String>)>,
}

impl Reads<'_> {
    /// Fold every read in `module`.
    fn fold_module(&self, module: &mut Module, sink: &mut DiagnosticSink) {
        let scopes: Vec<Scope> = module
            .items
            .iter()
            .map(|item| match item {
                Item::Impl(implementation) => scope_of(module, implementation),
                _ => Scope::default(),
            })
            .collect();
        for (item, scope) in module.items.iter_mut().zip(&scopes) {
            match item {
                Item::Impl(implementation) => {
                    for member in &mut implementation.items {
                        match member {
                            ImplItem::Const(constant) => {
                                self.fold_type(&mut constant.ty, scope, sink);
                                self.fold_expr(&mut constant.value, scope, sink)
                            }
                            ImplItem::Let(declaration) => {
                                if let Some(ty) = &mut declaration.ty {
                                    self.fold_type(ty, scope, sink);
                                }
                                if let Some(value) = &mut declaration.value {
                                    self.fold_expr(value, scope, sink);
                                }
                            }
                            ImplItem::Fn(function) => self.fold_fn(function, scope, sink),
                            ImplItem::Process(process) => {
                                self.fold_block(&mut process.body, scope, sink)
                            }
                            ImplItem::Stmt(statement) => self.fold_stmt(statement, scope, sink),
                            ImplItem::ModeField { .. }
                            | ImplItem::AttrBinding(_)
                            | ImplItem::Attr(_)
                            | ImplItem::Block(_) => {}
                        }
                    }
                }
                Item::Fn(function) => self.fold_fn(function, scope, sink),
                Item::Const(constant) => {
                    self.fold_type(&mut constant.ty, scope, sink);
                    self.fold_expr(&mut constant.value, scope, sink)
                }
                _ => {}
            }
        }
    }

    fn fold_block(&self, block: &mut Block, scope: &Scope, sink: &mut DiagnosticSink) {
        for statement in &mut block.stmts {
            self.fold_stmt(statement, scope, sink);
        }
    }

    fn fold_stmt(&self, statement: &mut Stmt, scope: &Scope, sink: &mut DiagnosticSink) {
        match statement {
            Stmt::Use(_) => {}
            Stmt::Process(process) => self.fold_block(&mut process.body, scope, sink),
            Stmt::Let(declaration) => {
                if let Some(ty) = &mut declaration.ty {
                    self.fold_type(ty, scope, sink);
                }
                if let Some(value) = &mut declaration.value {
                    self.fold_expr(value, scope, sink);
                }
            }
            Stmt::Assign {
                target,
                value,
                after,
                ..
            } => {
                self.fold_expr(target, scope, sink);
                self.fold_expr(value, scope, sink);
                if let Some(after) = after {
                    self.fold_expr(after, scope, sink);
                }
            }
            Stmt::If(iff) => self.fold_if(iff, scope, sink),
            Stmt::Match(m) => {
                self.fold_expr(&mut m.scrutinee, scope, sink);
                for arm in &mut m.arms {
                    self.fold_block(&mut arm.body, scope, sink);
                }
            }
            Stmt::For {
                range: cond, body, ..
            }
            | Stmt::While { cond, body, .. } => {
                self.fold_expr(cond, scope, sink);
                self.fold_block(body, scope, sink);
            }
            Stmt::Expr(expression) => self.fold_expr(expression, scope, sink),
            Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    self.fold_expr(value, scope, sink);
                }
            }
        }
    }

    fn fold_if(&self, iff: &mut IfStmt, scope: &Scope, sink: &mut DiagnosticSink) {
        self.fold_expr(&mut iff.cond, scope, sink);
        self.fold_block(&mut iff.then, scope, sink);
        match iff.else_.as_deref_mut() {
            Some(ElseBranch::Block(block)) => self.fold_block(block, scope, sink),
            Some(ElseBranch::If(inner)) => self.fold_if(inner, scope, sink),
            None => {}
        }
    }

    /// Fold the reads inside a type: `unsigned[w'speed]` sizes by the
    /// attribute (#697), as a value read does.
    fn fold_type(&self, ty: &mut Type, scope: &Scope, sink: &mut DiagnosticSink) {
        match ty {
            Type::Path(_) => {}
            Type::Indexed { base, index, .. } => {
                self.fold_type(base, scope, sink);
                if let Some(index) = index {
                    self.fold_expr(index, scope, sink);
                }
            }
            Type::Generic { base, args, .. } => {
                self.fold_type(base, scope, sink);
                self.fold_generic_args(args, scope, sink);
            }
            Type::Tuple { elems, .. } => {
                for elem in elems {
                    self.fold_type(elem, scope, sink);
                }
            }
            Type::View { target, .. } => self.fold_type(target, scope, sink),
        }
    }

    fn fold_generic_args(&self, args: &mut [GenericArg], scope: &Scope, sink: &mut DiagnosticSink) {
        for arg in args {
            match arg {
                GenericArg::Positional(value) | GenericArg::Named { value, .. } => {
                    self.fold_expr(value, scope, sink)
                }
                GenericArg::PositionalType(ty) | GenericArg::NamedType { ty, .. } => {
                    self.fold_type(ty, scope, sink)
                }
            }
        }
    }

    /// A function's parameter and return types, then its body.
    fn fold_fn(&self, function: &mut FnDecl, scope: &Scope, sink: &mut DiagnosticSink) {
        for parameter in &mut function.params {
            if let Some(ty) = &mut parameter.ty {
                self.fold_type(ty, scope, sink);
            }
        }
        if let Some(ret) = &mut function.ret {
            self.fold_type(ret, scope, sink);
        }
        if let Some(body) = &mut function.body {
            self.fold_block(body, scope, sink);
        }
    }

    fn fold_expr(&self, expression: &mut Expr, scope: &Scope, sink: &mut DiagnosticSink) {
        if let Expr::SysAttr { base, attr, span } = expression {
            if self.names.contains(attr.text.as_str()) {
                if let Some(value) = self.read(base, attr, *span, scope, sink) {
                    *expression = value;
                }
                return;
            }
        }
        match expression {
            Expr::Int { .. }
            | Expr::SuffixLit { .. }
            | Expr::BitStrLit { .. }
            | Expr::CharLit { .. }
            | Expr::StrLit { .. }
            | Expr::Path(_) => {}
            Expr::Field { base, .. } | Expr::SysAttr { base, .. } => {
                self.fold_expr(base, scope, sink)
            }
            Expr::Index { base, index, .. } => {
                self.fold_expr(base, scope, sink);
                self.fold_expr(index, scope, sink);
            }
            Expr::Range { lo, hi, .. } => {
                self.fold_expr(lo, scope, sink);
                self.fold_expr(hi, scope, sink);
            }
            Expr::PartialRange { lo, hi, .. } => {
                for bound in [lo, hi].into_iter().flatten() {
                    self.fold_expr(bound, scope, sink);
                }
            }
            Expr::Unary { rhs, .. } => self.fold_expr(rhs, scope, sink),
            Expr::Binary { lhs, rhs, .. } => {
                self.fold_expr(lhs, scope, sink);
                self.fold_expr(rhs, scope, sink);
            }
            Expr::IfExpr {
                cond, then, els, ..
            } => {
                self.fold_expr(cond, scope, sink);
                self.fold_expr(then, scope, sink);
                self.fold_expr(els, scope, sink);
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.fold_expr(scrutinee, scope, sink);
                for arm in arms {
                    self.fold_block(&mut arm.body, scope, sink);
                }
            }
            Expr::Call {
                callee,
                args,
                type_args,
                ..
            } => {
                self.fold_expr(callee, scope, sink);
                self.fold_generic_args(type_args, scope, sink);
                for arg in args {
                    self.fold_expr(arg, scope, sink);
                }
            }
            Expr::Construct {
                ty, args, spread, ..
            } => {
                if let Some(ty) = ty {
                    self.fold_type(ty, scope, sink);
                }
                for arg in args {
                    if let Some(value) = &mut arg.value {
                        self.fold_expr(value, scope, sink);
                    }
                }
                if let Some(spread) = spread {
                    self.fold_expr(spread, scope, sink);
                }
            }
            Expr::Concat { parts: items, .. }
            | Expr::Array { elems: items, .. }
            | Expr::Tuple { elems: items, .. } => {
                for item in items {
                    self.fold_expr(item, scope, sink);
                }
            }
        }
    }

    /// The value `base'attr` denotes, or `None` after reporting why it has
    /// none.
    fn read(
        &self,
        base: &Expr,
        attr: &Ident,
        span: Span,
        scope: &Scope,
        sink: &mut DiagnosticSink,
    ) -> Option<Expr> {
        let name = attr.text.as_str();
        // `m1::Worker'speed` names another module's entity (#697), which
        // `Within` resolves; a local is always one segment.
        let object = match base {
            Expr::Path(path) => path_text(path),
            _ => {
                sink.emit(
                    Diagnostic::error(format!("`'{name}` reads a declaration's attribute"))
                        .with_code(codes::UNKNOWN_ATTR_OBJECT)
                        .at(span)
                        .help("name a `let`, port, instance or entity: `probe'keep`"),
                );
                return None;
            }
        };
        let object = object.as_str();
        let bound = if let Some((attrs, entity)) = scope.members.get(object) {
            find(attrs, name).or_else(|| {
                entity
                    .as_ref()
                    .and_then(|entity| self.entity(entity))
                    .and_then(|attrs| find(attrs, name))
            })
        } else if let Some(attrs) = self.entity(object) {
            find(attrs, name)
        } else {
            sink.emit(
                Diagnostic::error(format!("`{object}'{name}` names no declaration"))
                    .with_code(codes::UNKNOWN_ATTR_OBJECT)
                    .at(base_span(base, span))
                    .help("a read names a `let`, port or instance of this entity, or an entity"),
            );
            return None;
        };
        let value = match bound {
            // `#[keep]` is the flag shorthand for `= true`.
            Some(Attr { value: None, .. }) => Some(Expr::Path(Path {
                segments: ["Bool", "true"]
                    .iter()
                    .map(|text| Ident {
                        text: text.to_string(),
                        span: attr.span,
                    })
                    .collect(),
                span: attr.span,
            })),
            Some(Attr { value, .. }) => value.clone(),
            None => match self.defaults.get(name) {
                Some(candidates) => self.within.pick(name, candidates).cloned().flatten(),
                None => self.declared.get(name).and_then(|d| d.default.clone()),
            },
        };
        if value.is_none() {
            sink.emit(
                Diagnostic::error(format!("`{object}'{name}` has no value"))
                    .with_code(codes::ATTR_WITHOUT_VALUE)
                    .at(span)
                    .help(format!(
                        "bind it (`attr {name} for {object} = …;`) or give the declaration a \
                         default (`attr {name}: … for … = …;`)"
                    )),
            );
        }
        value
    }
}

/// The members a read inside `implementation` can name.
fn scope_of(module: &Module, implementation: &ImplDecl) -> Scope {
    let mut scope = Scope::default();
    let Some(owner) = type_name(&implementation.target) else {
        return scope;
    };
    for item in &module.items {
        match item {
            Item::Impl(candidate)
                if candidate.trait_.is_none() && type_name(&candidate.target) == Some(owner) =>
            {
                for member in &candidate.items {
                    if let ImplItem::Let(declaration) = member {
                        let entity = match &declaration.value {
                            Some(Expr::Construct { ty: Some(ty), .. }) => type_path(ty),
                            _ => declaration.ty.as_ref().and_then(type_path),
                        };
                        scope.members.insert(
                            declaration.name.text.clone(),
                            (declaration.attrs.clone(), entity),
                        );
                    }
                }
            }
            Item::Entity(entity) if entity.name.text == owner => {
                for port in &entity.ports {
                    scope
                        .members
                        .entry(port.name.text.clone())
                        .or_insert_with(|| (port.attrs.clone(), None));
                }
            }
            _ => {}
        }
    }
    scope
}

/// The applied attribute called `name` among `attrs`.
fn find<'a>(attrs: &'a [Attr], name: &str) -> Option<&'a Attr> {
    attrs.iter().find(|a| attr_name(a) == name)
}

/// The last segment of an applied attribute's name.
fn attr_name(attr: &Attr) -> &str {
    attr.name.segments.last().map_or("", |s| s.text.as_str())
}

/// The declared name a type expression names, for an instance or an impl
/// target: the last segment of its path. An array of instances is not one.
fn type_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Path(path) => path.segments.last().map(|s| s.text.as_str()),
        Type::Generic { base, .. } => type_name(base),
        Type::Indexed { .. } | Type::View { .. } | Type::Tuple { .. } => None,
    }
}

/// [`type_name`] with its qualifier: `m1::Worker`, which module's it is.
fn type_path(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(path) => Some(path_text(path)),
        Type::Generic { base, .. } => type_path(base),
        Type::Indexed { .. } | Type::View { .. } | Type::Tuple { .. } => None,
    }
}

/// `a::b` as written.
fn path_text(path: &Path) -> String {
    path.segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("::")
}

/// Whether `path` spells `text`.
fn path_is(path: &Path, text: &str) -> bool {
    path_text(path) == text
}

/// The span of a read's base, falling back to the whole read.
fn base_span(base: &Expr, fallback: Span) -> Span {
    match base {
        Expr::Path(path) => path.span,
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::FileId;

    /// Parse each source as its own module, attach, and return the modules
    /// with the diagnostics.
    fn attached(sources: &[&str]) -> (Vec<Module>, Vec<Diagnostic>) {
        let mut sink = DiagnosticSink::new();
        let mut modules: Vec<Module> = sources
            .iter()
            .enumerate()
            .map(|(i, src)| crate::syntax::parse_module(FileId(i as u32), src, &mut sink))
            .collect();
        assert_eq!(
            sink.error_count(),
            0,
            "parse errors: {:#?}",
            sink.diagnostics()
        );
        attach(&mut modules, &mut sink);
        (modules, sink.diagnostics().to_vec())
    }

    fn entity<'m>(module: &'m Module, name: &str) -> &'m EntityDecl {
        module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Entity(e) if e.name.text == name => Some(e),
                _ => None,
            })
            .expect("entity")
    }

    fn impl_of<'m>(module: &'m Module, name: &str) -> &'m ImplDecl {
        module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Impl(im) if type_name(&im.target) == Some(name) => Some(im),
                _ => None,
            })
            .expect("impl")
    }

    fn names(attrs: &[Attr]) -> Vec<String> {
        attrs
            .iter()
            .map(|a| {
                format!(
                    "{}={}",
                    attr_name(a),
                    a.value
                        .as_ref()
                        .map_or("_".into(), crate::syntax::pretty::expr)
                )
            })
            .collect()
    }

    /// A named binding lands on the `let` or port it names, wherever in the
    /// entity's implementation blocks it is written; an objectless one lands
    /// on the implementation for an `impl` attribute and on the entity
    /// otherwise; a module-level one names an entity.
    #[test]
    fn bindings_attach_to_their_targets() {
        let (modules, diags) = attached(&["module m;\n\
             attr keep: Bool for let, port = false;\n\
             attr speed: integer for entity = 1;\n\
             attr precedence: integer for impl;\n\
             entity E { clk: Bit in }\n\
             impl E { attr keep for probe = true; attr keep for clk = true; attr speed = 9; }\n\
             impl E { let probe: Bit; }\n\
             impl CustomOperator<\"^^\", Bit, Bit> for Bit { attr precedence = 40; }\n\
             entity F {}\n\
             attr speed for F = 3;\n"]);
        assert!(diags.is_empty(), "{diags:#?}");
        let m = &modules[0];
        assert_eq!(names(&entity(m, "E").attrs), ["speed=9"]);
        assert_eq!(names(&entity(m, "E").ports[0].attrs), ["keep=true"]);
        assert_eq!(names(&entity(m, "F").attrs), ["speed=3"]);
        let probe = m
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Impl(im) => Some(im),
                _ => None,
            })
            .flat_map(|im| &im.items)
            .find_map(|member| match member {
                ImplItem::Let(l) if l.name.text == "probe" => Some(l),
                _ => None,
            })
            .expect("probe");
        assert_eq!(names(&probe.attrs), ["keep=true"]);
        assert_eq!(names(&impl_of(m, "Bit").attrs), ["precedence=40"]);
    }

    /// A read folds to the object's binding, then (for an instance) its
    /// entity's binding, then the declared default.
    #[test]
    fn reads_fold_to_binding_then_entity_then_default() {
        let (modules, diags) = attached(&["module m;\n\
             attr speed: integer for entity, let = 1;\n\
             entity Pll {}\n\
             impl Pll { attr speed = 250; }\n\
             entity Other {}\n\
             entity Top { y: unsigned[8] out }\n\
             impl Top {\n\
               attr speed for fast = 7;\n\
               let fast: unsigned[8];\n\
               let p: Pll = {};\n\
               let o: Other = {};\n\
               y = fast'speed + p'speed + o'speed + Pll'speed;\n\
             }\n"]);
        assert!(diags.is_empty(), "{diags:#?}");
        let top = impl_of(&modules[0], "Top");
        let ImplItem::Stmt(Stmt::Assign { value, .. }) = top.items.last().unwrap() else {
            panic!("expected the assignment")
        };
        assert_eq!(crate::syntax::pretty::expr(value), "7 + 250 + 1 + 250");
    }

    /// A read only folds an attribute the module can name: its own, an
    /// imported one, or one the prelude re-exports.
    #[test]
    fn reads_see_imported_and_prelude_attributes_only() {
        let (modules, diags) = attached(&[
            "module vendor; pub attr speed: integer for let = 5; pub attr hidden: integer for let = 6;",
            "module std::prelude; pub use vendor::speed;",
            "module m; entity T { y: unsigned[8] out } impl T { let s: unsigned[8]; y = s'speed + s'hidden; }",
        ]);
        assert!(diags.is_empty(), "{diags:#?}");
        let ImplItem::Stmt(Stmt::Assign { value, .. }) =
            impl_of(&modules[2], "T").items.last().unwrap()
        else {
            panic!("expected the assignment")
        };
        assert_eq!(crate::syntax::pretty::expr(value), "5 + s'hidden");
    }

    /// The binding errors: no such object, nothing to bind at module level,
    /// a second binding of one attribute, and a read with no value.
    #[test]
    fn binding_and_read_errors() {
        let (_, diags) = attached(&["module m;\n\
             attr speed: integer for let;\n\
             attr flag: Bool for let = false;\n\
             entity T { y: unsigned[8] out }\n\
             impl T {\n\
               attr speed for ghost = 1;\n\
               attr flag for v = true;\n\
               attr flag for v = false;\n\
               let v: unsigned[8];\n\
               y = v'speed;\n\
             }\n\
             attr speed = 3;\n\
             attr speed for Nowhere = 3;\n"]);
        let codes: Vec<_> = diags.iter().filter_map(|d| d.code).collect();
        assert_eq!(
            codes,
            [
                codes::UNKNOWN_ATTR_OBJECT,
                codes::UNKNOWN_ATTR_OBJECT,
                codes::UNKNOWN_ATTR_OBJECT,
                codes::DUPLICATE_ATTR_BINDING,
                codes::ATTR_WITHOUT_VALUE,
            ],
            "{diags:#?}"
        );
    }
}
