//! Scoped blocks in implementations (docs/proposals/scoped-blocks.md): a
//! `label: { … }` or `{ … }` member groups signals, instances, processes and
//! concurrent statements under a scope of its own. Right after macro
//! expansion this pass flattens every block into the implementation it sits
//! in, so no later stage sees one and nothing about simulation changes:
//!
//! - a member of a labelled block takes the label as a path, `counter.n`
//!   (nested labels compose: `outer.inner.n`), which is its name in the IR,
//!   waveforms and the debugger; an unlabelled block adds no level;
//! - names inside a block are renamed to those paths, so a block sees its
//!   own members and every enclosing scope's;
//! - `pub let` exports a member one level: through the label
//!   (`counter.wrapped`) for a labelled block, by its bare name for an
//!   unlabelled one. Anything else is private to the block.
//!
//! Errors: a member repeating a name visible from an enclosing scope, or
//! colliding with another in the flattened namespace (E-P002); a private
//! member named from outside (E-P001 for a bare name, E-P024 through the
//! label); a type-level member (`fn`, `const`, a mode field) in a block.

use std::collections::{HashMap, HashSet};

use crate::diag::{codes, Diagnostic, DiagnosticSink, Span};
use crate::syntax::ast::*;

/// Flatten every scoped block of every implementation.
pub fn flatten(modules: &mut [Module], sink: &mut DiagnosticSink) {
    let ports: HashMap<String, Vec<String>> = modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            Item::Entity(entity) => Some((
                entity.name.text.clone(),
                entity.ports.iter().map(|p| p.name.text.clone()).collect(),
            )),
            _ => None,
        })
        .collect();
    for module in modules.iter_mut() {
        for item in &mut module.items {
            let Item::Impl(implementation) = item else {
                continue;
            };
            if !implementation
                .items
                .iter()
                .any(|item| matches!(item, ImplItem::Block(_)))
            {
                continue;
            }
            let mut visible: HashSet<String> = implementation_names(&implementation.items);
            // `impl<N: integer> Top<N>`: a member may not shadow `N` (#402).
            visible.extend(
                implementation
                    .params
                    .params
                    .iter()
                    .map(|parameter| parameter.name.text.clone()),
            );
            if let Some(target) = head_name(&implementation.target) {
                visible.extend(ports.get(&target).cloned().unwrap_or_default());
            }
            let items = std::mem::take(&mut implementation.items);
            let mut flat = Flat {
                taken: visible.clone(),
                generics: implementation
                    .params
                    .params
                    .iter()
                    .map(|parameter| parameter.name.text.clone())
                    .collect(),
                sink,
            };
            let mut out = Vec::new();
            // The root is a scope whose own members keep their names.
            let mut root = Vec::new();
            let mut blocks = Vec::new();
            for item in items {
                match item {
                    ImplItem::Block(block) => blocks.push(block),
                    other => root.push(other),
                }
            }
            let scope = Scope {
                prefix: String::new(),
                // Generic parameters are visible to every block (#402).
                visible: flat.generics.clone(),
                renames: HashMap::new(),
                labels: HashMap::new(),
            };
            flat.scope(root, blocks, &scope, true, &mut out);
            implementation.items = out;
        }
    }
}

fn head_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(path) => path.segments.last().map(|s| s.text.clone()),
        Type::Generic { base, .. } => head_name(base),
        _ => None,
    }
}

/// The names an implementation's root declares.
fn implementation_names(items: &[ImplItem]) -> HashSet<String> {
    items
        .iter()
        .filter_map(|item| match item {
            ImplItem::Let(declaration) => Some(declaration.name.text.clone()),
            ImplItem::Const(constant) => Some(constant.name.text.clone()),
            ImplItem::Fn(function) => Some(function.name.text.clone()),
            ImplItem::ModeField { name, .. } => Some(name.text.clone()),
            _ => None,
        })
        .collect()
}

/// What an enclosing scope hands a block.
struct Scope {
    /// The path prefix of this scope's members: `""` at the root,
    /// `counter.` inside `counter: { … }`.
    prefix: String,
    /// Names visible from enclosing scopes, which a member may not repeat.
    visible: HashSet<String>,
    /// Visible names whose flattened spelling differs: `n` -> `counter.n`.
    renames: HashMap<String, String>,
    /// Visible block labels: each one's exports and private names.
    labels: HashMap<String, Label>,
}

/// A labelled block, as the scopes that can see its label see it.
#[derive(Clone)]
struct Label {
    exports: HashMap<String, String>,
    private: HashSet<String>,
}

/// A child block, as its parent sees it.
struct Child {
    label: Option<String>,
    span: Span,
    /// Its `pub let` members, by name, to their flattened names.
    exports: HashMap<String, String>,
    /// Its other members, for the private-access errors.
    private: HashSet<String>,
}

struct Flat<'s> {
    /// Every flattened name in the implementation, for collisions.
    taken: HashSet<String>,
    /// The implementation's generic parameters, visible everywhere in it.
    generics: HashSet<String>,
    sink: &'s mut DiagnosticSink,
}

impl Flat<'_> {
    /// Flatten one scope: its own `items` and its child `blocks`, members
    /// appended to `out` under their flattened names.
    fn scope(
        &mut self,
        items: Vec<ImplItem>,
        blocks: Vec<ImplBlock>,
        outer: &Scope,
        root: bool,
        out: &mut Vec<ImplItem>,
    ) {
        // This scope's own members, then what its children export into it.
        let mut renames = outer.renames.clone();
        let mut names = HashSet::new();
        for item in &items {
            if let ImplItem::Let(declaration) = item {
                let name = &declaration.name.text;
                if !root {
                    if outer.visible.contains(name) {
                        self.shadow(name, declaration.name.span);
                    }
                    let mut flat = format!("{}{name}", outer.prefix);
                    if !self.taken.insert(flat.clone()) {
                        self.collision(&flat, declaration.name.span);
                        // Reported once: a spelling of its own from here on.
                        flat = format!("{flat}#{}", self.taken.len());
                        self.taken.insert(flat.clone());
                    }
                    renames.insert(name.clone(), flat);
                }
                names.insert(name.clone());
            }
        }
        let children: Vec<Child> = blocks
            .iter()
            .map(|block| self.child(block, &outer.prefix))
            .collect();
        let mut labels = outer.labels.clone();
        let mut own_labels = HashSet::new();
        for child in &children {
            match &child.label {
                Some(label) => {
                    if names.contains(label) || !own_labels.insert(label.clone()) {
                        self.collision(label, child.span);
                    }
                    labels.insert(
                        label.clone(),
                        Label {
                            exports: child.exports.clone(),
                            private: child.private.clone(),
                        },
                    );
                }
                // An unlabelled block's `pub` members join this scope.
                None => {
                    for (name, flat) in &child.exports {
                        if outer.visible.contains(name) || names.contains(name) {
                            self.collision(name, child.span);
                        }
                        names.insert(name.clone());
                        renames.insert(name.clone(), flat.clone());
                    }
                }
            }
        }
        // Children see every enclosing name, this scope's included, except
        // the ones a child exports into it itself.
        let mut inner_visible = outer.visible.clone();
        inner_visible.extend(names.iter().cloned());
        inner_visible.extend(own_labels.iter().cloned());
        for (block, child) in blocks.into_iter().zip(&children) {
            let prefix = match &child.label {
                Some(label) => format!("{}{label}.", outer.prefix),
                None => outer.prefix.clone(),
            };
            let mut visible = inner_visible.clone();
            if child.label.is_none() {
                for name in child.exports.keys() {
                    visible.remove(name);
                }
            }
            let scope = Scope {
                prefix,
                visible,
                renames: renames.clone(),
                labels: labels.clone(),
            };
            let (own, nested): (Vec<ImplItem>, Vec<ImplItem>) = block
                .items
                .into_iter()
                .partition(|item| !matches!(item, ImplItem::Block(_)));
            let nested = nested
                .into_iter()
                .filter_map(|item| match item {
                    ImplItem::Block(block) => Some(block),
                    _ => None,
                })
                .collect();
            self.scope(own, nested, &scope, false, out);
        }
        // This scope's own members, renamed.
        let generics = self.generics.clone();
        let rewrite = Rewrite {
            renames: &renames,
            labels: &labels,
            children: &children,
            names: &names,
            generics: &generics,
        };
        for mut item in items {
            match &mut item {
                ImplItem::Fn(_) | ImplItem::Const(_) | ImplItem::ModeField { .. } if !root => {
                    self.sink.emit(
                        Diagnostic::error("a scoped block holds signals, instances and processes")
                            .with_code(codes::UNSUPPORTED_EXPR)
                            .at(impl_item_span(&item))
                            .help(
                                "functions, constants and mode fields belong at the \
                                 implementation root, shared by every `impl` of the entity",
                            ),
                    );
                    continue;
                }
                ImplItem::Let(declaration) => {
                    declaration.is_pub = false;
                    if let Some(flat) = renames.get(&declaration.name.text) {
                        if !root {
                            declaration.name.text = flat.clone();
                        }
                    }
                }
                ImplItem::Process(process) => {
                    self.process_locals(&process.body, &renames);
                    if let (Some(label), false) = (&mut process.label, root) {
                        label.text = format!("{}{}", outer.prefix, label.text);
                    }
                }
                // A generate `for` or `if` binds its own names too: its loop
                // variable hid a block member silently, and every use of
                // it read the member (#698).
                ImplItem::Stmt(statement) => {
                    let block = Block {
                        stmts: vec![statement.clone()],
                        span: crate::syntax::ast::stmt_span(statement),
                    };
                    self.process_locals(&block, &renames);
                }
                ImplItem::AttrBinding(binding) => {
                    if let Some(object) = &mut binding.object {
                        if let Some(flat) = renames.get(&object.text) {
                            object.text = flat.clone();
                        }
                    }
                }
                _ => {}
            }
            rewrite.item(&mut item, self.sink);
            out.push(item);
        }
    }

    /// What a child block exposes to its parent, from its declarations.
    fn child(&mut self, block: &ImplBlock, prefix: &str) -> Child {
        let label = block.label.as_ref().map(|l| l.text.clone());
        let own_prefix = match &label {
            Some(label) => format!("{prefix}{label}."),
            None => prefix.to_string(),
        };
        let mut exports = HashMap::new();
        let mut private = HashSet::new();
        for item in &block.items {
            if let ImplItem::Let(declaration) = item {
                let name = declaration.name.text.clone();
                if declaration.is_pub {
                    exports.insert(name.clone(), format!("{own_prefix}{name}"));
                } else {
                    private.insert(name);
                }
            }
        }
        Child {
            label,
            span: block.span,
            exports,
            private,
        }
    }

    /// A process-local `let` may not hide a block member.
    fn process_locals(&mut self, root: &Block, renames: &HashMap<String, String>) {
        let mut stack: Vec<&Block> = vec![root];
        while let Some(block) = stack.pop() {
            for statement in &block.stmts {
                match statement {
                    Stmt::Let(declaration) => {
                        if renames.contains_key(&declaration.name.text) {
                            self.shadow(&declaration.name.text, declaration.name.span);
                        }
                    }
                    Stmt::If(chain) => {
                        let mut link = Some(chain);
                        while let Some(current) = link {
                            stack.push(&current.then);
                            link = match current.else_.as_deref() {
                                Some(ElseBranch::If(next)) => Some(next),
                                Some(ElseBranch::Block(block)) => {
                                    stack.push(block);
                                    None
                                }
                                None => None,
                            };
                        }
                    }
                    Stmt::Match(m) => stack.extend(m.arms.iter().map(|arm| &arm.body)),
                    // A loop variable hides a member as a `let` does (#402).
                    Stmt::For { var, body, .. } => {
                        if renames.contains_key(&var.text) {
                            self.shadow(&var.text, var.span);
                        }
                        stack.push(body);
                    }
                    Stmt::While { body, .. } => stack.push(body),
                    Stmt::Process(process) => stack.push(&process.body),
                    _ => {}
                }
            }
        }
    }

    fn shadow(&mut self, name: &str, span: Span) {
        self.sink.emit(
            Diagnostic::error(format!(
                "`{name}` is already visible from an enclosing scope"
            ))
            .with_code(codes::DUPLICATE_ITEM)
            .at(span)
            .help("a block may not repeat an outer name: rename one of them"),
        );
    }

    fn collision(&mut self, name: &str, span: Span) {
        self.sink.emit(
            Diagnostic::error(format!("`{name}` is declared twice in this implementation"))
                .with_code(codes::DUPLICATE_ITEM)
                .at(span)
                .help(
                    "an unlabelled block's members share the enclosing namespace; label one block",
                ),
        );
    }
}

/// Renames one scope's own members' expressions.
struct Rewrite<'r> {
    renames: &'r HashMap<String, String>,
    labels: &'r HashMap<String, Label>,
    children: &'r [Child],
    names: &'r HashSet<String>,
    generics: &'r HashSet<String>,
}

impl Rewrite<'_> {
    fn item(&self, item: &mut ImplItem, sink: &mut DiagnosticSink) {
        let locals = bound_names(item);
        crate::syntax::walk::walk_impl_item(item, &mut |e| self.expr(e, &locals, sink));
    }

    fn expr(&self, e: &mut Expr, locals: &HashSet<String>, sink: &mut DiagnosticSink) -> bool {
        match e {
            Expr::Path(path) if path.segments.len() == 1 => {
                let name = path.segments[0].text.clone();
                let name = &name;
                // A parameter, `let` or loop variable of the function or
                // process: its own, never a block member's (#402). One that
                // hides a member is reported by `process_locals`.
                if locals.contains(name) || self.generics.contains(name) {
                    return true;
                }
                if let Some(flat) = self.renames.get(name) {
                    path.segments[0].text = flat.clone();
                } else if !self.names.contains(name) {
                    // A child's private member named from outside it.
                    if let Some(child) = self.children.iter().find(|c| c.private.contains(name)) {
                        let owner = child
                            .label
                            .as_ref()
                            .map_or("an unlabelled block".to_string(), |l| {
                                format!("block `{l}`")
                            });
                        // Reported here; name the member so resolution does
                        // not report it again.
                        if let Some(label) = &child.label {
                            path.segments[0].text = format!("{label}.{name}");
                        }
                        sink.emit(
                            Diagnostic::error(format!("`{name}` is private to {owner}"))
                                .with_code(codes::UNKNOWN_NAME)
                                .at(path.span)
                                .label(child.span, "declared in this block")
                                .help(format!("declare it `pub let {name}` to use it here")),
                        );
                    }
                }
                true
            }
            Expr::Field { base, field, span } => {
                let Expr::Path(path) = base.as_ref() else {
                    return true;
                };
                let [label] = path.segments.as_slice() else {
                    return true;
                };
                let Some(child) = self.labels.get(&label.text) else {
                    return true;
                };
                match child.exports.get(&field.text) {
                    Some(flat) => {
                        *e = Expr::Path(Path {
                            segments: vec![Ident {
                                text: flat.clone(),
                                span: field.span,
                            }],
                            span: *span,
                        });
                        false
                    }
                    None => {
                        let private = child.private.contains(&field.text);
                        let message = if private {
                            format!("`{}.{}` is private to its block", label.text, field.text)
                        } else {
                            format!("block `{}` has no member `{}`", label.text, field.text)
                        };
                        sink.emit(
                            Diagnostic::error(message)
                                .with_code(codes::PRIVATE_MEMBER)
                                .at(*span)
                                .help(format!(
                                    "declare it `pub let {}` in `{}` to use it here",
                                    field.text, label.text
                                )),
                        );
                        if private {
                            *e = Expr::Path(Path {
                                segments: vec![Ident {
                                    text: format!("{}.{}", label.text, field.text),
                                    span: field.span,
                                }],
                                span: *span,
                            });
                        }
                        false
                    }
                }
            }
            _ => true,
        }
    }
}

/// The names a function or process binds for itself: parameters, generic
/// parameters, `let`s and loop variables anywhere in its body.
// ponytail: one set per item, not per lexical scope, so a name bound late in
// a body is also left alone before its binding; a block member it would hide
// is an error anyway.
fn bound_names(item: &ImplItem) -> HashSet<String> {
    let mut names = HashSet::new();
    let statement_block;
    let body = match item {
        ImplItem::Fn(function) => {
            names.extend(
                function
                    .params
                    .iter()
                    .filter_map(|parameter| parameter.name.as_ref().map(|name| name.text.clone())),
            );
            names.extend(function.generics.params.iter().map(|p| p.name.text.clone()));
            function.body.as_ref()
        }
        ImplItem::Process(process) => Some(&process.body),
        // A generate `for i in ..` binds `i` for its body (#698).
        ImplItem::Stmt(statement) => {
            statement_block = Block {
                stmts: vec![statement.clone()],
                span: crate::syntax::ast::stmt_span(statement),
            };
            Some(&statement_block)
        }
        _ => None,
    };
    let mut stack: Vec<&Block> = body.into_iter().collect();
    while let Some(block) = stack.pop() {
        for statement in &block.stmts {
            match statement {
                Stmt::Let(declaration) => {
                    names.insert(declaration.name.text.clone());
                }
                Stmt::For { var, body, .. } => {
                    names.insert(var.text.clone());
                    stack.push(body);
                }
                Stmt::While { body, .. } => stack.push(body),
                Stmt::Process(process) => stack.push(&process.body),
                Stmt::If(chain) => {
                    let mut link = Some(chain);
                    while let Some(current) = link {
                        stack.push(&current.then);
                        link = match current.else_.as_deref() {
                            Some(ElseBranch::If(next)) => Some(next),
                            Some(ElseBranch::Block(block)) => {
                                stack.push(block);
                                None
                            }
                            None => None,
                        };
                    }
                }
                Stmt::Match(m) => stack.extend(m.arms.iter().map(|arm| &arm.body)),
                _ => {}
            }
        }
    }
    names
}

fn impl_item_span(item: &ImplItem) -> Span {
    match item {
        ImplItem::Const(c) => c.span,
        ImplItem::Let(l) => l.span,
        ImplItem::Fn(f) => f.span,
        ImplItem::ModeField { span, .. } => *span,
        ImplItem::Process(p) => p.span,
        ImplItem::Stmt(s) => crate::syntax::ast::stmt_span(s),
        ImplItem::AttrBinding(b) => b.span,
        ImplItem::Attr(a) => a.span,
        ImplItem::Block(b) => b.span,
    }
}
