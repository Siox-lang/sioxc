//! Resolved declarative attributes: bindings and reads (spec 3.5).
//!
//! Metadata is declared once (`attr keep: Bool for let, port = false;`),
//! bound from outside the declaration it describes (`attr keep for probe =
//! true;`, or objectless `attr precedence = 40;` for the enclosing item), and
//! read back with the tick (`probe'keep`). Raw names and values resolve first;
//! attachment and read substitution then retain those declaration identities:
//!
//! - **Bindings** are copied onto their target as an applied [`Attr`], the
//!   form every later stage already reads (target and value checks, the
//!   elaborated `Instance::attrs`). The binding item stays in
//!   the tree for the printer and is otherwise ignored.
//! - **Reads** of a declared attribute are replaced by the value they denote:
//!   the binding on that object, else (for an instance) the binding on its
//!   entity, else the declaration's default. Bindings are elaboration-time
//!   constants, so a read is substituted here rather than carried as an operation.
//!   Integer values are evaluated by `types` before elaboration.
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

use std::collections::HashMap;

use super::{DefId, Resolved};
use crate::diag::{codes, Diagnostic, DiagnosticSink, Span};
use crate::syntax::ast::*;

pub(crate) fn is_target_kind(name: &str) -> bool {
    matches!(
        name,
        "entity"
            | "let"
            | "port"
            | "impl"
            | "struct"
            | "enum"
            | "view"
            | "trait"
            | "fn"
            | "attr"
            | "type"
            | "module"
    )
}

/// The identity of a written type head, never a leaf-name search.
pub(crate) fn type_identity(ty: &Type, resolved: &Resolved) -> Option<DefId> {
    match ty {
        Type::Path(path) => resolved
            .resolved(path.span)
            .map(|id| resolved.entity_id(id).unwrap_or(id)),
        Type::Generic { base, .. } | Type::Indexed { base, .. } => type_identity(base, resolved),
        Type::View { view, .. } => resolved.resolved(view.span),
        Type::Tuple { .. } => None,
    }
}

/// Output strings retain identity even when imports used aliases or equal leaves.
pub(crate) fn output_name(attr: &Attr, resolved: &Resolved) -> String {
    if let Some(id) = resolved.resolved(attr.name.span) {
        if matches!(
            resolved.lang_of(id),
            Some("metadata_keep" | "metadata_library" | "metadata_name" | "metadata_precedence")
        ) {
            if let Some(definition) = resolved.def(id) {
                return definition.name.clone();
            }
        }
        if let Some(name) = resolved.qualified_name_str(id) {
            return name.to_string();
        }
    }
    path_text(&attr.name)
}

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

/// Attach resolved bindings to their targets, then call [`fold_reads`].
pub fn attach(modules: &mut [Module], resolved: &Resolved, sink: &mut DiagnosticSink) {
    let declared = declarations(modules, resolved);
    for module in modules.iter_mut() {
        bind_module(module, &declared, resolved, sink);
    }
}

/// Substitute reads after resolving bindings/defaults in their original scopes.
pub fn fold_reads(modules: &mut [Module], resolved: &Resolved, sink: &mut DiagnosticSink) {
    let declared = declarations(modules, resolved);
    let entity_attrs = entity_attrs(modules, resolved);
    let reads = Reads {
        declared: &declared,
        entity_attrs: &entity_attrs,
        resolved,
    };
    for module in modules {
        reads.fold_module(module, sink);
    }
}

/// Every attribute declaration, by resolved identity. The standard ones are seeded so
/// bindings of them work even before `std::attrs` is loaded, mirroring the
/// type checker's seeds.
fn declarations(modules: &[Module], resolved: &Resolved) -> HashMap<DefId, Declared> {
    let mut out = HashMap::new();
    for (name, targets) in [
        ("test", &["entity"][..]),
        ("keep", &["let", "port"][..]),
        ("library", &["entity"][..]),
        ("name", &["entity"][..]),
        ("precedence", &["impl"][..]),
    ] {
        let Some(id) = resolved.builtin_attribute(name) else {
            continue;
        };
        out.insert(
            id,
            Declared {
                targets: targets.iter().map(|t| t.to_string()).collect(),
                default: None,
            },
        );
    }
    for module in modules {
        for item in &module.items {
            if let Item::AttrDecl(declaration) = item {
                let Some(id) = resolved.declared(declaration.name.span) else {
                    continue;
                };
                out.insert(
                    id,
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

/// Every entity's applied attributes after binding, by resolved entity identity.
fn entity_attrs(modules: &[Module], resolved: &Resolved) -> HashMap<DefId, Vec<Attr>> {
    let mut out = HashMap::new();
    for module in modules {
        for item in &module.items {
            if let Item::Entity(entity) = item {
                if let Some(id) = resolved.declared(entity.name.span) {
                    out.insert(id, entity.attrs.clone());
                }
            }
        }
    }
    out
}

/// Resolve and apply one module's bindings.
fn bind_module(
    module: &mut Module,
    declared: &HashMap<DefId, Declared>,
    resolved: &Resolved,
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
                        Some(object) => {
                            member_named(module, implementation, &object.text, resolved)
                        }
                        None => Some(enclosing(
                            module,
                            index,
                            implementation,
                            binding,
                            declared,
                            resolved,
                        )),
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
        let id = resolved.resolved(attr.name.span);
        if let Some(previous) = attrs
            .iter()
            .find(|a| id.is_some() && resolved.resolved(a.name.span) == id)
        {
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
    declared: &HashMap<DefId, Declared>,
    resolved: &Resolved,
) -> Target {
    let targets_impl = resolved
        .resolved(binding.name.span)
        .and_then(|id| declared.get(&id))
        .is_none_or(|d| d.targets.iter().any(|t| t == "impl"));
    if !targets_impl && implementation.trait_.is_none() {
        if let Some(entity) = entity_with_id(
            module,
            type_identity(&implementation.target, resolved),
            resolved,
        ) {
            return Target::Entity(entity);
        }
    }
    Target::Impl(index)
}

/// The `let` or port called `name` that a binding inside `implementation`
/// names: a `let` in any of the same entity's implementation blocks in this
/// module, then a port of the entity.
fn member_named(
    module: &Module,
    implementation: &ImplDecl,
    name: &str,
    resolved: &Resolved,
) -> Option<Target> {
    let owner = type_identity(&implementation.target, resolved)?;
    for (item, candidate) in module.items.iter().enumerate() {
        let Item::Impl(candidate) = candidate else {
            continue;
        };
        if candidate.trait_.is_some() || type_identity(&candidate.target, resolved) != Some(owner) {
            continue;
        }
        for (member, declaration) in candidate.items.iter().enumerate() {
            if matches!(declaration, ImplItem::Let(l) if l.name.text == name) {
                return Some(Target::Let { item, member });
            }
        }
    }
    let item = entity_with_id(module, Some(owner), resolved)?;
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

fn entity_with_id(module: &Module, id: Option<DefId>, resolved: &Resolved) -> Option<usize> {
    let id = id?;
    module.items.iter().position(|item| {
        matches!(item, Item::Entity(entity)
        if resolved.declared(entity.name.span) == Some(id))
    })
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
    declared: &'r HashMap<DefId, Declared>,
    /// Every entity's attributes after binding, for instance fallbacks.
    entity_attrs: &'r HashMap<DefId, Vec<Attr>>,
    resolved: &'r Resolved,
}

/// The names a read can address inside one implementation: its `let`s (with
/// their attributes and, for an instance, its entity) and its ports.
#[derive(Default)]
struct Scope {
    members: HashMap<String, (Vec<Attr>, Option<DefId>)>,
}

impl Reads<'_> {
    /// Fold every read in `module`.
    fn fold_module(&self, module: &mut Module, sink: &mut DiagnosticSink) {
        let scopes: Vec<Scope> = module
            .items
            .iter()
            .map(|item| match item {
                Item::Impl(implementation) => scope_of(module, implementation, self.resolved),
                _ => Scope::default(),
            })
            .collect();
        for (item, scope) in module.items.iter_mut().zip(&scopes) {
            match item {
                Item::Impl(implementation) => {
                    for member in &mut implementation.items {
                        match member {
                            ImplItem::Const(constant) => {
                                self.fold_expr(&mut constant.value, scope, sink)
                            }
                            ImplItem::Let(declaration) => {
                                if let Some(value) = &mut declaration.value {
                                    self.fold_expr(value, scope, sink);
                                }
                            }
                            ImplItem::Fn(function) => {
                                if let Some(body) = &mut function.body {
                                    self.fold_block(body, scope, sink);
                                }
                            }
                            ImplItem::Process(process) => {
                                self.fold_block(&mut process.body, scope, sink)
                            }
                            ImplItem::Stmt(statement) => self.fold_stmt(statement, scope, sink),
                            ImplItem::ModeField { .. }
                            | ImplItem::AttrBinding(_)
                            | ImplItem::Attr(_) => {}
                        }
                    }
                }
                Item::Fn(function) => {
                    if let Some(body) = &mut function.body {
                        self.fold_block(body, scope, sink);
                    }
                }
                Item::Const(constant) => self.fold_expr(&mut constant.value, scope, sink),
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
            Stmt::Let(declaration) => {
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
            Stmt::For { range, body, .. } => {
                self.fold_expr(range, scope, sink);
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

    fn fold_expr(&self, expression: &mut Expr, scope: &Scope, sink: &mut DiagnosticSink) {
        if let Expr::SysAttr { base, attr, span } = expression {
            if self
                .resolved
                .resolved(attr.span)
                .is_some_and(|id| self.declared.contains_key(&id))
            {
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
            Expr::Call { callee, args, .. } => {
                self.fold_expr(callee, scope, sink);
                for arg in args {
                    self.fold_expr(arg, scope, sink);
                }
            }
            Expr::Construct { args, spread, .. } => {
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
        let id = self.resolved.resolved(attr.span)?;
        let path = match base {
            Expr::Path(path) => path,
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
        let object = path_text(path);
        let bound = if let Some((attrs, entity)) = scope.members.get(&object) {
            find(attrs, id, self.resolved).or_else(|| {
                entity
                    .as_ref()
                    .and_then(|entity| self.entity_attrs.get(entity))
                    .and_then(|attrs| find(attrs, id, self.resolved))
            })
        } else if let Some(attrs) = self
            .resolved
            .resolved(path.span)
            .and_then(|id| self.resolved.entity_id(id))
            .and_then(|entity| self.entity_attrs.get(&entity))
        {
            find(attrs, id, self.resolved)
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
            // Compiler directives do not synthesize Boolean metadata values.
            Some(Attr { value, .. }) => value.clone(),
            None => self.declared.get(&id).and_then(|d| d.default.clone()),
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
fn scope_of(module: &Module, implementation: &ImplDecl, resolved: &Resolved) -> Scope {
    let mut scope = Scope::default();
    let Some(owner) = type_identity(&implementation.target, resolved) else {
        return scope;
    };
    for item in &module.items {
        match item {
            Item::Impl(candidate)
                if candidate.trait_.is_none()
                    && type_identity(&candidate.target, resolved) == Some(owner) =>
            {
                for member in &candidate.items {
                    if let ImplItem::Let(declaration) = member {
                        let entity = match &declaration.value {
                            Some(Expr::Construct { ty: Some(ty), .. }) => {
                                type_identity(ty, resolved)
                            }
                            _ => declaration
                                .ty
                                .as_ref()
                                .and_then(|ty| type_identity(ty, resolved)),
                        };
                        scope.members.insert(
                            declaration.name.text.clone(),
                            (declaration.attrs.clone(), entity),
                        );
                    }
                }
            }
            Item::Entity(entity) if resolved.declared(entity.name.span) == Some(owner) => {
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

/// The applied attribute with identity `id` among `attrs`.
fn find<'a>(attrs: &'a [Attr], id: DefId, resolved: &Resolved) -> Option<&'a Attr> {
    attrs
        .iter()
        .find(|a| resolved.resolved(a.name.span) == Some(id))
}

/// The last segment of an applied attribute's name.
fn attr_name(attr: &Attr) -> &str {
    attr.name.segments.last().map_or("", |s| s.text.as_str())
}

/// The declared name a type expression names, for an instance or an impl
/// target: the last segment of its path. An array of instances is not one.
#[cfg(test)]
fn type_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Path(path) => path.segments.last().map(|s| s.text.as_str()),
        Type::Generic { base, .. } => type_name(base),
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
        let count = modules.len();
        for source in [
            include_str!("../../core/primitive.siox"),
            "module fixtures; pub enum Bit { '0', '1' } pub struct unsigned(integer);",
            "module std::prelude; pub use core::primitive::Bool; pub use fixtures::{Bit, unsigned};",
        ] {
            modules.push(crate::syntax::parse_module(FileId(modules.len() as u32), source, &mut sink));
        }
        let resolved = super::super::resolve(&modules, &mut sink);
        attach(&mut modules, &resolved, &mut sink);
        fold_reads(&mut modules, &resolved, &mut sink);
        modules.truncate(count);
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
