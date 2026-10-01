//! Macro expansion (proposals/macros.md, language §3.30).
//!
//! A pass over every parsed module, before imports are desugared and names
//! resolved. It finds each `name!(…)` that names a user `macro` — declared in
//! the module, imported with `use`, or written as a path — picks the form
//! whose parameters the arguments match, substitutes the argument tokens into
//! the body, and parses the result where the call stood: an expression,
//! statements, implementation members or items. The result is expanded
//! again, so macros may invoke macros, up to [`RECURSION_LIMIT`] deep.
//! Finally the `macro` declarations and the imports naming only macros are
//! removed, so later stages never see a macro.
//!
//! Hygiene follows Rust's macros 2.0, at the definition site:
//!
//! - A name the body *declares* (`let`, `for`, a label, an item) is renamed
//!   `name#n`, fresh per expansion, so it neither captures nor is captured
//!   by a caller's name.
//! - A free name in the body means what it means in the macro's module: when
//!   the macro is invoked from another module, it is qualified with the
//!   path of the item it names there.
//! - Names in the arguments are the caller's and are left alone.
//!
//! Built-in `assert!`, `print!` and `warn!` are not user macros and pass
//! through untouched, as does any call this pass cannot find a macro for.

use std::collections::{HashMap, HashSet};

use crate::diag::{codes, Diagnostic, DiagnosticSink, Span};
use crate::syntax::ast::*;
use crate::syntax::parser::Parser;
use crate::syntax::token::TokenKind;

/// How deep expansions may nest: rustc's default `recursion_limit`.
pub const RECURSION_LIMIT: usize = 128;

/// Expand every user macro in `modules`. Warnings that lint levels govern
/// (an unused macro import) are returned rather than emitted, because the
/// expansion's own `#[allow(..)]` directives are not in force yet.
pub fn expand(
    modules: &mut [Module],
    operators: &HashMap<String, u8>,
    sink: &mut DiagnosticSink,
) -> Vec<Diagnostic> {
    let index = Index::new(modules);
    let mut used = HashSet::new();
    let mut counter = 0;
    for module in modules.iter_mut() {
        let name = path_text(&module.path);
        let mut expander = Expander {
            index: &index,
            operators,
            sink: &mut *sink,
            module: name,
            args: std::mem::take(&mut module.macro_args),
            lints: Vec::new(),
            counter: &mut counter,
            used: &mut used,
            trusted: HashSet::new(),
        };
        let items = std::mem::take(&mut module.items);
        module.items = expander.items(items, 0);
        module.macro_args = std::mem::take(&mut expander.args);
        module.lints.extend(std::mem::take(&mut expander.lints));
    }
    // The declarations, and imports that name nothing but macros.
    let mut warnings = Vec::new();
    for module in modules.iter_mut() {
        let here = path_text(&module.path);
        module.items.retain(|item| !matches!(item, Item::Macro(_)));
        let had_names: Vec<bool> = module
            .items
            .iter()
            .map(|item| {
                matches!(item, Item::Using(Using { kind: UsingKind::Import { names, .. }, .. })
                    if !names.is_empty())
            })
            .collect();
        for item in &mut module.items {
            let Item::Using(Using {
                is_pub,
                kind: UsingKind::Import { base, names },
                ..
            }) = item
            else {
                continue;
            };
            let base = absolute(&segments(base), &here);
            names.retain(|leaf| {
                if leaf.glob || leaf.name.text == "self" {
                    return true;
                }
                let mut full = base.clone();
                full.extend(leaf.via.iter().map(|v| v.text.clone()));
                full.push(leaf.name.text.clone());
                let (owner, name) = full.split_at(full.len() - 1);
                let owner = owner.join("::");
                let is_macro = index.export(&owner, &name[0], 0).is_some();
                if !is_macro || index.is_item(&owner, &name[0]) {
                    return true;
                }
                let binding = leaf.binding().text.clone();
                // As for any import, a `pub use` re-export is not linted.
                if !*is_pub && !used.contains(&(here.clone(), binding.clone())) {
                    warnings.push(
                        Diagnostic::warning(format!("unused import: `{binding}`"))
                            .with_code(codes::UNUSED_IMPORT)
                            .at(leaf.name.span)
                            .help("remove it"),
                    );
                }
                false
            });
        }
        // An import left with no names imported only macros.
        let mut had = had_names.into_iter();
        module.items.retain(|item| {
            let had = had.next().unwrap_or(false);
            !(had
                && matches!(item, Item::Using(Using { kind: UsingKind::Import { names, .. }, .. })
                    if names.is_empty()))
        });
    }
    warnings
}

/// What every module declares and imports, for finding macros and for
/// resolving a macro body's free names where the macro is declared.
#[derive(Default)]
struct Index {
    /// Every loaded module's path.
    modules: HashSet<String>,
    /// `(module, name)` -> the macro's forms, in declaration order.
    macros: HashMap<(String, String), Vec<MacroDecl>>,
    /// Module -> names of its other items.
    items: HashMap<String, HashSet<String>>,
    /// Module -> its imports.
    imports: HashMap<String, Imports>,
}

/// One module's imports.
#[derive(Default)]
struct Imports {
    /// Binding -> (full path, `pub`) for each imported item.
    leaves: HashMap<String, (Vec<String>, bool)>,
    /// Binding -> module path for each imported module.
    modules: HashMap<String, Vec<String>>,
    /// Each glob's module, and whether it is `pub`.
    globs: Vec<(String, bool)>,
}

impl Index {
    fn new(modules: &[Module]) -> Self {
        let mut index = Index {
            modules: modules.iter().map(|m| path_text(&m.path)).collect(),
            ..Index::default()
        };
        for module in modules {
            let here = path_text(&module.path);
            let mut imports = Imports::default();
            for item in &module.items {
                match item {
                    Item::Macro(m) => index
                        .macros
                        .entry((here.clone(), m.name.text.clone()))
                        .or_default()
                        .push(m.clone()),
                    Item::Using(Using {
                        is_pub,
                        kind: UsingKind::Import { base, names },
                        ..
                    }) => {
                        let base = absolute(&segments(base), &here);
                        for leaf in names {
                            let mut full = base.clone();
                            full.extend(leaf.via.iter().map(|v| v.text.clone()));
                            if leaf.glob {
                                imports.globs.push((full.join("::"), *is_pub));
                                continue;
                            }
                            if leaf.name.text == "self" {
                                let binding = leaf.local.as_ref().map_or_else(
                                    || full.last().cloned().unwrap_or_default(),
                                    |l| l.text.clone(),
                                );
                                imports.modules.insert(binding, full);
                                continue;
                            }
                            full.push(leaf.name.text.clone());
                            let binding = leaf.binding().text.clone();
                            if index.modules.contains(&full.join("::")) {
                                imports.modules.insert(binding.clone(), full.clone());
                            }
                            imports.leaves.insert(binding, (full, *is_pub));
                        }
                    }
                    other => {
                        if let Some(name) = item_name(other) {
                            index.items.entry(here.clone()).or_default().insert(name);
                        }
                    }
                }
            }
            index.imports.insert(here, imports);
        }
        index
    }

    /// Whether `module` declares an item (not a macro) called `name`.
    fn is_item(&self, module: &str, name: &str) -> bool {
        self.items
            .get(module)
            .is_some_and(|names| names.contains(name))
    }

    /// The macro `module::name` is, following `pub use` re-exports and
    /// globs: its declaring module and name. Private macros are found too;
    /// the caller checks visibility.
    fn export(&self, module: &str, name: &str, depth: usize) -> Option<(String, String)> {
        if depth > 16 {
            return None;
        }
        let key = (module.to_string(), name.to_string());
        if self.macros.contains_key(&key) {
            return Some(key);
        }
        let imports = self.imports.get(module)?;
        if let Some((full, _)) = imports.leaves.get(name) {
            let (owner, leaf) = full.split_at(full.len() - 1);
            if let Some(found) = self.export(&owner.join("::"), &leaf[0], depth + 1) {
                return Some(found);
            }
        }
        imports
            .globs
            .iter()
            .find_map(|(glob, _)| self.export(glob, name, depth + 1))
    }

    /// Whether every form of the macro is private.
    fn is_private(&self, key: &(String, String)) -> bool {
        self.macros
            .get(key)
            .is_some_and(|forms| forms.iter().all(|m| !m.is_pub))
    }
}

/// Where an invocation stands, which decides what its expansion must be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Position {
    /// One expression.
    Expr,
    /// Statements; `true` inside a process or function body.
    Stmts(bool),
    /// Implementation members.
    Members,
    /// Module items.
    Items,
}

/// The expansion of one invocation, parsed.
enum Expansion {
    Expr(Expr),
    Stmts(Vec<Stmt>),
    Members(Vec<ImplItem>),
    Items(Vec<Item>),
}

/// Expands the macros of one module.
struct Expander<'a> {
    index: &'a Index,
    operators: &'a HashMap<String, u8>,
    sink: &'a mut DiagnosticSink,
    /// The module being expanded.
    module: String,
    /// Its macro calls' argument tokens, including the expansions' own.
    args: HashMap<Span, MacroArgs>,
    /// Lint directives the expansions wrote.
    lints: Vec<crate::diag::lints::LintDirective>,
    /// Fresh hygiene marks, shared by every module.
    counter: &'a mut usize,
    /// `(module, binding)` of each macro import some call went through.
    used: &'a mut HashSet<(String, String)>,
    /// Macro names a body wrote and this pass qualified: they may name a
    /// private macro of the body's own module.
    trusted: HashSet<Span>,
}

impl Expander<'_> {
    // --- finding the macro ---------------------------------------------------

    /// The macro a call's callee names, if it is a user macro.
    fn find(&mut self, path: &Path) -> Option<(String, String)> {
        let index = self.index;
        let written = segments(path);
        let abs = absolute(&written, &self.module);
        let imports = index.imports.get(&self.module);
        if let [name] = &abs[..] {
            if self
                .index
                .macros
                .contains_key(&(self.module.clone(), name.clone()))
            {
                return Some((self.module.clone(), name.clone()));
            }
            if let Some((full, _)) = imports.and_then(|i| i.leaves.get(name)) {
                let (owner, leaf) = full.split_at(full.len() - 1);
                let found = self.index.export(&owner.join("::"), &leaf[0], 0)?;
                self.used.insert((self.module.clone(), name.clone()));
                self.check_visible(&found, &owner.join("::"), path.span);
                return Some(found);
            }
            return imports?
                .globs
                .iter()
                .find_map(|(glob, _)| self.index.export(glob, name, 0));
        }
        let (owner, name) = abs.split_at(abs.len() - 1);
        let mut owner = owner.to_vec();
        if let Some(module) = imports.and_then(|i| i.modules.get(&owner[0])) {
            owner.splice(..1, module.iter().cloned());
        }
        let owner = owner.join("::");
        let found = self.index.export(&owner, &name[0], 0)?;
        let last = path.segments.last().map_or(path.span, |s| s.span);
        self.check_visible(&found, &owner, last);
        Some(found)
    }

    /// Report a private macro used from another module, unless the use was
    /// written inside a macro of the macro's own module.
    fn check_visible(&mut self, found: &(String, String), via: &str, span: Span) {
        if found.0 != self.module
            && via != self.module
            && !self.trusted.contains(&span)
            && self.index.is_private(found)
        {
            self.sink.emit(
                Diagnostic::error(format!("macro `{}` is private", found.1))
                    .with_code(codes::PRIVATE_IMPORT)
                    .at(span)
                    .help("mark it `pub macro` to use it from another module"),
            );
        }
    }

    // --- expanding one call --------------------------------------------------

    /// Expand the call at `span` to macro `key`, parsed for `position`.
    /// `None` after an error was reported.
    fn expand(
        &mut self,
        key: &(String, String),
        span: Span,
        position: Position,
        depth: usize,
    ) -> Option<Expansion> {
        let name = &key.1;
        if depth >= RECURSION_LIMIT {
            self.sink.emit(
                Diagnostic::error(format!(
                    "macro expansion limit exceeded while expanding `{name}!`"
                ))
                .with_code(codes::MACRO_EXPANSION)
                .at(span)
                .note(format!("expansions nest at most {RECURSION_LIMIT} deep")),
            );
            return None;
        }
        let call = self.args.get(&span).cloned()?;
        let args = split_args(&call.tokens);
        let index = self.index;
        let forms = &index.macros[key];
        let form = forms.iter().find(|form| {
            form.params.len() == args.len()
                && form.params.iter().zip(&args).all(|(param, arg)| {
                    let mut scratch = DiagnosticSink::new();
                    Parser::from_macro_tokens(arg, &mut scratch, self.operators, false, span)
                        .is_fragment(param.kind)
                })
        });
        let Some(form) = form.cloned() else {
            let shapes = forms
                .iter()
                .map(|form| {
                    let params = form
                        .params
                        .iter()
                        .map(|p| format!("${}: {}", p.name.text, p.kind.name()))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("`{name}!({params})`")
                })
                .collect::<Vec<_>>()
                .join(", ");
            self.sink.emit(
                Diagnostic::error(format!("no form of `{name}!` accepts these arguments"))
                    .with_code(codes::MACRO_EXPANSION)
                    .at(span)
                    .help(format!("it takes {shapes}")),
            );
            return None;
        };
        let tokens = self.substitute(&form, &args, &key.0, span)?;

        let mut scratch = DiagnosticSink::new();
        let mut parser = Parser::from_macro_tokens(
            &tokens,
            &mut scratch,
            self.operators,
            matches!(position, Position::Stmts(true)),
            span,
        );
        let expansion = match position {
            Position::Expr => Expansion::Expr(parser.expansion_expr()),
            Position::Stmts(_) => Expansion::Stmts(parser.expansion_stmts()),
            Position::Members => Expansion::Members(parser.expansion_impl_items()),
            Position::Items => Expansion::Items(parser.expansion_items()),
        };
        self.args.extend(parser.take_macro_args());
        self.lints.extend(parser.take_lints());
        for diagnostic in scratch.diagnostics() {
            self.sink.emit(
                diagnostic
                    .clone()
                    .note(format!("while expanding `{name}!`")),
            );
        }
        Some(expansion)
    }

    /// The body with arguments substituted, hygiene applied, and free names
    /// qualified for the declaring module.
    fn substitute(
        &mut self,
        form: &MacroDecl,
        args: &[Vec<MacroToken>],
        home: &str,
        call: Span,
    ) -> Option<Vec<MacroToken>> {
        // Tokens with whether they come from the body.
        let mut out: Vec<(MacroToken, bool)> = Vec::new();
        let mut body = form.body.iter().peekable();
        let mut ok = true;
        while let Some(token) = body.next() {
            if token.kind != TokenKind::Dollar {
                out.push((token.clone(), true));
                continue;
            }
            let Some(name) = body.next_if(|t| t.kind == TokenKind::Ident) else {
                self.sink.emit(
                    Diagnostic::error("expected a parameter name after `$`")
                        .with_code(codes::MACRO_EXPANSION)
                        .at(token.span),
                );
                ok = false;
                continue;
            };
            let Some(index) = form.params.iter().position(|p| p.name.text == name.text) else {
                self.sink.emit(
                    Diagnostic::error(format!(
                        "`${}` is not a parameter of `{}!`",
                        name.text, form.name.text
                    ))
                    .with_code(codes::MACRO_EXPANSION)
                    .at(name.span),
                );
                ok = false;
                continue;
            };
            let arg = &args[index];
            if form.params[index].kind == FragmentKind::Expr {
                // One operand, whatever the operators around it.
                let first = arg.first().map_or(call, |t| t.span);
                let last = arg.last().map_or(call, |t| t.span);
                out.push((
                    punct(
                        TokenKind::LParen,
                        "(",
                        Span {
                            end: first.start,
                            ..first
                        },
                    ),
                    false,
                ));
                out.extend(arg.iter().map(|t| (t.clone(), false)));
                out.push((
                    punct(
                        TokenKind::RParen,
                        ")",
                        Span {
                            start: last.end,
                            ..last
                        },
                    ),
                    false,
                ));
            } else {
                out.extend(arg.iter().map(|t| (t.clone(), false)));
            }
        }
        if !ok {
            return None;
        }
        self.hygiene(&mut out);
        Some(self.qualify(out, home))
    }

    /// Rename every name the body declares, everywhere the body uses it.
    fn hygiene(&mut self, tokens: &mut [(MacroToken, bool)]) {
        use TokenKind as K;
        let mut declared = HashSet::new();
        for i in 0..tokens.len() {
            let (token, from_body) = &tokens[i];
            if !from_body || token.kind != K::Ident {
                continue;
            }
            let after_keyword = i > 0
                && matches!(
                    tokens[i - 1].0.kind,
                    K::Let
                        | K::For
                        | K::Fn
                        | K::Struct
                        | K::Enum
                        | K::Entity
                        | K::Const
                        | K::Type
                        | K::Trait
                        | K::View
                );
            let label = tokens.get(i + 1).is_some_and(|t| t.0.kind == K::Colon)
                && tokens
                    .get(i + 2)
                    .is_some_and(|t| matches!(t.0.kind, K::Process | K::For | K::If));
            if after_keyword || label {
                declared.insert(token.text.clone());
            }
        }
        if declared.is_empty() {
            return;
        }
        *self.counter += 1;
        let mark = *self.counter;
        for i in 0..tokens.len() {
            let member = i > 0 && matches!(tokens[i - 1].0.kind, K::Dot | K::ColonColon | K::Tick);
            let (token, from_body) = &mut tokens[i];
            if *from_body && token.kind == K::Ident && !member && declared.contains(&token.text) {
                token.text = format!("{}#{mark}", token.text);
            }
        }
    }

    /// Qualify the body's free names with what they name in `home`, the
    /// declaring module, when the call is in another module.
    fn qualify(&mut self, tokens: Vec<(MacroToken, bool)>, home: &str) -> Vec<MacroToken> {
        use TokenKind as K;
        if home == self.module {
            return tokens.into_iter().map(|(t, _)| t).collect();
        }
        let index = self.index;
        let imports = index.imports.get(home);
        let mut out = Vec::with_capacity(tokens.len());
        for i in 0..tokens.len() {
            let (token, from_body) = &tokens[i];
            let head = *from_body
                && token.kind == K::Ident
                && !token.text.contains('#')
                && (i == 0
                    || !matches!(
                        tokens[i - 1].0.kind,
                        K::Dot | K::ColonColon | K::Tick | K::Dollar
                    ));
            let next = tokens.get(i + 1).map(|t| &t.0.kind);
            let target: Option<Vec<String>> = if !head {
                None
            } else if next == Some(&K::ColonColon) {
                // A module alias of the declaring module.
                imports.and_then(|i| i.modules.get(&token.text)).cloned()
            } else if next == Some(&K::Bang) {
                // A macro: found from the declaring module, which may use
                // its own private macros.
                let found = index.export(home, &token.text, 0);
                if found.is_some() {
                    self.trusted.insert(token.span);
                }
                found.map(|(module, name)| path_of(&module, &name))
            } else if self.index.is_item(home, &token.text) {
                Some(path_of(home, &token.text))
            } else {
                imports
                    .and_then(|i| i.leaves.get(&token.text))
                    .map(|(full, _)| full.clone())
            };
            let Some(target) = target else {
                out.push(token.clone());
                continue;
            };
            let at = Span {
                end: token.span.start,
                ..token.span
            };
            let (last, qualifiers) = target.split_last().expect("a path has a segment");
            for segment in qualifiers {
                out.push(ident_token(segment, at));
                out.push(punct(K::ColonColon, "::", at));
            }
            out.push(MacroToken {
                text: last.clone(),
                ..token.clone()
            });
        }
        out
    }

    // --- walking -------------------------------------------------------------

    /// The user macro a bang call names.
    fn user_call(&mut self, expr: &Expr) -> Option<((String, String), Span)> {
        let Expr::Call {
            callee,
            bang: true,
            span,
            ..
        } = expr
        else {
            return None;
        };
        let Expr::Path(path) = callee.as_ref() else {
            return None;
        };
        self.find(path).map(|key| (key, *span))
    }

    fn items(&mut self, items: Vec<Item>, depth: usize) -> Vec<Item> {
        let mut out = Vec::with_capacity(items.len());
        for mut item in items {
            match &mut item {
                Item::MacroCall { path, span } => {
                    let span = *span;
                    match self.find(path) {
                        Some(key) => {
                            if let Some(Expansion::Items(expanded)) =
                                self.expand(&key, span, Position::Items, depth)
                            {
                                for generated in &expanded {
                                    if let Item::Macro(m) = generated {
                                        self.sink.emit(
                                            Diagnostic::error(
                                                "a macro expansion cannot declare a macro",
                                            )
                                            .with_code(codes::MACRO_EXPANSION)
                                            .at(m.name.span)
                                            .note(format!("while expanding `{}!`", key.1)),
                                        );
                                    }
                                }
                                let expanded = expanded
                                    .into_iter()
                                    .filter(|item| !matches!(item, Item::Macro(_)))
                                    .collect();
                                out.extend(self.items(expanded, depth + 1));
                            }
                        }
                        None => self.unknown(path),
                    }
                    continue;
                }
                Item::Fn(f) => self.fn_body(f, depth),
                Item::Const(c) => self.expr(&mut c.value, depth),
                Item::Impl(implementation) => {
                    let members = std::mem::take(&mut implementation.items);
                    implementation.items = self.members(members, depth);
                }
                Item::AttrBinding(b) => self.expr(&mut b.value, depth),
                _ => {}
            }
            out.push(item);
        }
        out
    }

    fn members(&mut self, members: Vec<ImplItem>, depth: usize) -> Vec<ImplItem> {
        let mut out = Vec::with_capacity(members.len());
        for mut member in members {
            if let ImplItem::Stmt(Stmt::Expr(call)) = &member {
                if let Some((key, span)) = self.user_call(call) {
                    if let Some(Expansion::Members(expanded)) =
                        self.expand(&key, span, Position::Members, depth)
                    {
                        out.extend(self.members(expanded, depth + 1));
                    }
                    continue;
                }
            }
            match &mut member {
                ImplItem::Const(c) => self.expr(&mut c.value, depth),
                ImplItem::Let(l) => {
                    if let Some(value) = &mut l.value {
                        self.expr(value, depth);
                    }
                }
                ImplItem::Fn(f) => self.fn_body(f, depth),
                ImplItem::Process(p) => self.block(&mut p.body, true, depth),
                ImplItem::Stmt(s) => self.stmt(s, false, depth),
                ImplItem::AttrBinding(b) => self.expr(&mut b.value, depth),
                ImplItem::ModeField { .. } => {}
            }
            out.push(member);
        }
        out
    }

    fn fn_body(&mut self, f: &mut FnDecl, depth: usize) {
        if let Some(body) = &mut f.body {
            self.block(body, true, depth);
        }
    }

    fn block(&mut self, block: &mut Block, sequential: bool, depth: usize) {
        let stmts = std::mem::take(&mut block.stmts);
        block.stmts = self.stmts(stmts, sequential, depth);
    }

    fn stmts(&mut self, stmts: Vec<Stmt>, sequential: bool, depth: usize) -> Vec<Stmt> {
        let mut out = Vec::with_capacity(stmts.len());
        for mut statement in stmts {
            if let Stmt::Expr(call) = &statement {
                if let Some((key, span)) = self.user_call(call) {
                    if let Some(Expansion::Stmts(expanded)) =
                        self.expand(&key, span, Position::Stmts(sequential), depth)
                    {
                        out.extend(self.stmts(expanded, sequential, depth + 1));
                    }
                    continue;
                }
            }
            self.stmt(&mut statement, sequential, depth);
            out.push(statement);
        }
        out
    }

    fn stmt(&mut self, statement: &mut Stmt, sequential: bool, depth: usize) {
        match statement {
            Stmt::Let(l) => {
                if let Some(value) = &mut l.value {
                    self.expr(value, depth);
                }
            }
            Stmt::Use(_) => {}
            Stmt::Assign {
                target,
                value,
                after,
                ..
            } => {
                self.expr(target, depth);
                self.expr(value, depth);
                if let Some(after) = after {
                    self.expr(after, depth);
                }
            }
            Stmt::If(iff) => self.if_stmt(iff, sequential, depth),
            Stmt::Match(m) => {
                self.expr(&mut m.scrutinee, depth);
                for arm in &mut m.arms {
                    self.block(&mut arm.body, sequential, depth);
                }
            }
            Stmt::For { range, body, .. } => {
                self.expr(range, depth);
                self.block(body, sequential, depth);
            }
            Stmt::Expr(e) => self.expr(e, depth),
            Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    self.expr(value, depth);
                }
            }
        }
    }

    fn if_stmt(&mut self, iff: &mut IfStmt, sequential: bool, depth: usize) {
        self.expr(&mut iff.cond, depth);
        self.block(&mut iff.then, sequential, depth);
        match iff.else_.as_deref_mut() {
            Some(ElseBranch::Block(b)) => self.block(b, sequential, depth),
            Some(ElseBranch::If(inner)) => self.if_stmt(inner, sequential, depth),
            None => {}
        }
    }

    fn expr(&mut self, expression: &mut Expr, depth: usize) {
        if let Some((key, span)) = self.user_call(expression) {
            if let Some(Expansion::Expr(mut expanded)) =
                self.expand(&key, span, Position::Expr, depth)
            {
                self.expr(&mut expanded, depth + 1);
                *expression = expanded;
            }
            return;
        }
        match expression {
            Expr::Int { .. }
            | Expr::SuffixLit { .. }
            | Expr::BitStrLit { .. }
            | Expr::CharLit { .. }
            | Expr::StrLit { .. }
            | Expr::Path(_) => {}
            Expr::Field { base, .. } | Expr::SysAttr { base, .. } => self.expr(base, depth),
            Expr::Index { base, index, .. } => {
                self.expr(base, depth);
                self.expr(index, depth);
            }
            Expr::Range { lo, hi, .. } => {
                self.expr(lo, depth);
                self.expr(hi, depth);
            }
            Expr::PartialRange { lo, hi, .. } => {
                for bound in [lo, hi].into_iter().flatten() {
                    self.expr(bound, depth);
                }
            }
            Expr::Unary { rhs, .. } => self.expr(rhs, depth),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs, depth);
                self.expr(rhs, depth);
            }
            Expr::IfExpr {
                cond, then, els, ..
            } => {
                self.expr(cond, depth);
                self.expr(then, depth);
                self.expr(els, depth);
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee, depth);
                for arm in arms {
                    self.block(&mut arm.body, true, depth);
                }
            }
            Expr::Call {
                callee,
                args,
                bang,
                span,
                ..
            } => {
                if *bang {
                    // Not a user macro: it must be a built-in one.
                    let builtin = matches!(callee.as_ref(), Expr::Path(p)
                        if p.segments.len() == 1
                            && matches!(p.segments[0].text.as_str(), "assert" | "print" | "warn"));
                    let readable = self.args.get(span).is_none_or(|a| a.parsed);
                    if let Expr::Path(path) = callee.as_ref() {
                        if !builtin || !readable {
                            let path = path.clone();
                            self.unknown(&path);
                        }
                    }
                }
                self.expr(callee, depth);
                for arg in args {
                    self.expr(arg, depth);
                }
            }
            Expr::Construct { args, spread, .. } => {
                for arg in args {
                    if let Some(value) = &mut arg.value {
                        self.expr(value, depth);
                    }
                }
                if let Some(spread) = spread {
                    self.expr(spread, depth);
                }
            }
            Expr::Concat { parts: items, .. } | Expr::Array { elems: items, .. } => {
                for item in items {
                    self.expr(item, depth);
                }
            }
        }
    }

    /// A call to a macro nobody declared.
    fn unknown(&mut self, path: &Path) {
        self.sink.emit(
            Diagnostic::error(format!("cannot find macro `{}!`", path_text(path)))
                .with_code(codes::UNKNOWN_NAME)
                .at(path.span)
                .help("declare it with `macro`, or import it with `use`"),
        );
    }
}

/// A call's arguments, split at top-level commas. A trailing comma is
/// allowed; no tokens are no arguments.
fn split_args(tokens: &[MacroToken]) -> Vec<Vec<MacroToken>> {
    use TokenKind as K;
    let mut args = vec![Vec::new()];
    let mut depth = 0usize;
    for token in tokens {
        match token.kind {
            K::LParen | K::LBracket | K::LBrace => depth += 1,
            K::RParen | K::RBracket | K::RBrace => depth = depth.saturating_sub(1),
            K::Comma if depth == 0 => {
                args.push(Vec::new());
                continue;
            }
            _ => {}
        }
        args.last_mut().expect("never empty").push(token.clone());
    }
    if args.last().is_some_and(Vec::is_empty) {
        args.pop();
    }
    args
}

/// The name an item declares, for the items a macro body may name.
fn item_name(item: &Item) -> Option<String> {
    Some(match item {
        Item::Const(c) => c.name.text.clone(),
        Item::Fn(f) => f.name.text.clone(),
        Item::Struct(s) => s.name.text.clone(),
        Item::View(v) => v.name.text.clone(),
        Item::Enum(e) => e.name.text.clone(),
        Item::Entity(e) => e.name.text.clone(),
        Item::Trait(t) => t.name.text.clone(),
        Item::AttrDecl(a) => a.name.text.clone(),
        Item::Using(Using {
            kind: UsingKind::Alias { name, .. },
            ..
        }) => name.text.clone(),
        _ => return None,
    })
}

/// A path's segments as text.
fn segments(path: &Path) -> Vec<String> {
    path.segments.iter().map(|s| s.text.clone()).collect()
}

/// `a::b` as text.
fn path_text(path: &Path) -> String {
    segments(path).join("::")
}

/// `module::name` as segments.
fn path_of(module: &str, name: &str) -> Vec<String> {
    module
        .split("::")
        .map(str::to_string)
        .chain([name.to_string()])
        .collect()
}

/// A path with a leading `self::`/`super::` made absolute from `here`.
fn absolute(path: &[String], here: &str) -> Vec<String> {
    let mut module: Vec<String> = here.split("::").map(str::to_string).collect();
    match path.first().map(String::as_str) {
        Some("self") => module
            .into_iter()
            .chain(path[1..].iter().cloned())
            .collect(),
        Some("super") => {
            let mut rest = path;
            while rest.first().map(String::as_str) == Some("super") {
                module.pop();
                rest = &rest[1..];
            }
            module.into_iter().chain(rest.iter().cloned()).collect()
        }
        _ => path.to_vec(),
    }
}

fn punct(kind: TokenKind, text: &str, span: Span) -> MacroToken {
    MacroToken {
        kind,
        span,
        text: text.to_string(),
    }
}

fn ident_token(text: &str, span: Span) -> MacroToken {
    punct(TokenKind::Ident, text, span)
}
