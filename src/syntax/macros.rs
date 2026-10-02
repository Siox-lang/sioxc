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
//! `assert!`, `warn!`, `print!` and `error!` are ordinary macros declared in
//! `core::assert` over the primitive `builtin # name(…)`, which the parser
//! reads as the bang call the compiler lowers. A primitive has no argument
//! tokens; only `core`'s macro bodies may write one, and it takes the
//! outermost invocation's span so a failure names the call site.

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
    // Every diagnostic inside a body says which macro it belongs to.
    let mut bodies = Vec::new();
    for forms in index.macros.values() {
        for form in forms {
            if let (Some(first), Some(last)) = (form.body.first(), form.body.last()) {
                let body = first.span.to(last.span);
                sink.note_inside(body, format!("in an expansion of `{}!`", form.name.text));
                bodies.push((body, form_home(&index, form)));
            }
        }
    }
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
            site: None,
            bodies: &bodies,
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
    args: MacroArgTable,
    /// Lint directives the expansions wrote.
    lints: Vec<crate::diag::lints::LintDirective>,
    /// Fresh hygiene marks, shared by every module.
    counter: &'a mut usize,
    /// `(module, binding)` of each macro import some call went through.
    used: &'a mut HashSet<(String, String)>,
    /// Macro names a body wrote and this pass qualified: they may name a
    /// private macro of the body's own module.
    trusted: HashSet<Span>,
    /// The outermost invocation being expanded, where a built-in macro
    /// written in a body reports its location.
    site: Option<Span>,
    /// Every macro body's extent.
    bodies: &'a [(Span, bool)],
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
            let glob = imports.and_then(|imports| {
                imports
                    .globs
                    .iter()
                    .find_map(|(glob, _)| index.export(glob, name, 0))
            });
            // Beneath everything, the preludes: `assert!` and the other
            // built-in macros are `core`'s.
            return glob.or_else(|| {
                ["core::prelude", "std::prelude"]
                    .iter()
                    .find_map(|prelude| index.export(prelude, name, 0))
            });
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
        let call = self.take_args(span)?;
        let args = split_args(&call.tokens);
        let index = self.index;
        let forms = &index.macros[key];
        let form = forms.iter().find(|form| {
            let variadic = form.params.last().is_some_and(|p| p.variadic);
            let fixed = form.params.len() - usize::from(variadic);
            let count_fits = if variadic {
                args.len() >= fixed
            } else {
                args.len() == fixed
            };
            count_fits
                && args.iter().enumerate().all(|(i, arg)| {
                    let param = &form.params[i.min(form.params.len() - 1)];
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
                        .map(|p| {
                            let many = if p.variadic { "..." } else { "" };
                            format!("${}: {}{many}", p.name.text, p.kind.name())
                        })
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
        merge_macro_args(&mut self.args, parser.take_macro_args());
        self.lints.extend(parser.take_lints());
        for diagnostic in scratch.diagnostics() {
            self.sink.emit(
                diagnostic
                    .clone()
                    .note(format!("in an expansion of `{name}!`")),
            );
        }
        Some(expansion)
    }

    /// The body with arguments substituted, repetitions unrolled, hygiene
    /// applied, and free names qualified for the declaring module.
    fn substitute(
        &mut self,
        form: &MacroDecl,
        args: &[Vec<MacroToken>],
        home: &str,
        call: Span,
    ) -> Option<Vec<MacroToken>> {
        let variadic = form.params.last().is_some_and(|p| p.variadic);
        let fixed = form.params.len() - usize::from(variadic);
        let mut bindings = HashMap::new();
        for (i, param) in form.params.iter().enumerate() {
            let values: Vec<&[MacroToken]> = if param.variadic {
                args[fixed..].iter().map(Vec::as_slice).collect()
            } else {
                vec![args[i].as_slice()]
            };
            bindings.insert(
                param.name.text.clone(),
                Binding {
                    kind: param.kind,
                    values,
                },
            );
        }
        // Tokens with whether they come from the body.
        let mut out: Vec<(MacroToken, bool)> = Vec::new();
        if !self.unroll(&form.body, &bindings, &form.name.text, call, &mut out) {
            return None;
        }
        self.hygiene(&mut out);
        Some(self.qualify(out, home))
    }

    /// Substitute `body` into `out`: `$x`, `$xs'length`, and
    /// `for macro $x in $xs [join T] { … }`. `false` after an error.
    fn unroll(
        &mut self,
        body: &[MacroToken],
        bindings: &HashMap<String, Binding<'_>>,
        macro_name: &str,
        call: Span,
        out: &mut Vec<(MacroToken, bool)>,
    ) -> bool {
        use TokenKind as K;
        let mut ok = true;
        let mut i = 0;
        while i < body.len() {
            let token = &body[i];
            if token.kind == K::For && body.get(i + 1).is_some_and(|t| t.kind == K::Macro) {
                match self.repetition(body, i, bindings, macro_name, call, out) {
                    Some(next) => i = next,
                    None => return false,
                }
                continue;
            }
            if token.kind != K::Dollar {
                out.push((token.clone(), true));
                i += 1;
                continue;
            }
            let Some(name) = body.get(i + 1).filter(|t| t.kind == K::Ident) else {
                self.sink.emit(
                    Diagnostic::error("expected a parameter name after `$`")
                        .with_code(codes::MACRO_EXPANSION)
                        .at(token.span),
                );
                ok = false;
                i += 1;
                continue;
            };
            i += 2;
            let Some(binding) = bindings.get(&name.text) else {
                self.sink.emit(
                    Diagnostic::error(format!(
                        "`${}` is not a parameter of `{macro_name}!`",
                        name.text
                    ))
                    .with_code(codes::MACRO_EXPANSION)
                    .at(name.span),
                );
                ok = false;
                continue;
            };
            // `$xs'length`: how many arguments.
            if body.get(i).is_some_and(|t| t.kind == K::Tick)
                && body.get(i + 1).is_some_and(|t| t.text == "length")
            {
                out.push((
                    punct(K::Int, &binding.values.len().to_string(), name.span),
                    false,
                ));
                i += 2;
                continue;
            }
            for (n, value) in binding.values.iter().enumerate() {
                if n > 0 {
                    out.push((punct(K::Comma, ",", name.span), false));
                }
                push_argument(binding.kind, value, call, out);
            }
        }
        ok
    }

    /// Unroll the `for macro` at `body[at]`; the index after it.
    fn repetition(
        &mut self,
        body: &[MacroToken],
        at: usize,
        bindings: &HashMap<String, Binding<'_>>,
        macro_name: &str,
        call: Span,
        out: &mut Vec<(MacroToken, bool)>,
    ) -> Option<usize> {
        use TokenKind as K;
        let kind = |i: usize| body.get(at + i).map(|t| &t.kind);
        let text = |i: usize| body.get(at + i).map_or("", |t| t.text.as_str());
        let shaped = kind(2) == Some(&K::Dollar)
            && kind(3) == Some(&K::Ident)
            && kind(4) == Some(&K::In)
            && kind(5) == Some(&K::Dollar)
            && kind(6) == Some(&K::Ident);
        let joined = shaped && text(7) == "join" && kind(8).is_some();
        let open = if joined { at + 9 } else { at + 7 };
        let close = shaped
            .then(|| body.get(open).filter(|t| t.kind == K::LBrace))
            .flatten()
            .and_then(|_| matching_close(body, open));
        let Some(close) = close else {
            self.sink.emit(
                Diagnostic::error("expected `for macro $x in $xs { … }`")
                    .with_code(codes::MACRO_EXPANSION)
                    .at(body[at].span)
                    .help(
                        "a separator goes before the braces: `for macro $x in $xs join + { $x }`",
                    ),
            );
            return None;
        };
        let item = text(3).to_string();
        let list = text(6);
        let Some(source) = bindings.get(list) else {
            self.sink.emit(
                Diagnostic::error(format!("`${list}` is not a parameter of `{macro_name}!`"))
                    .with_code(codes::MACRO_EXPANSION)
                    .at(body[at + 6].span),
            );
            return None;
        };
        let separator = joined.then(|| body[at + 8].clone());
        let inner = &body[open + 1..close];
        for (n, value) in source.values.iter().enumerate() {
            if n > 0 {
                if let Some(separator) = &separator {
                    out.push((separator.clone(), true));
                }
            }
            let mut scope = bindings.clone();
            scope.insert(
                item.clone(),
                Binding {
                    kind: source.kind,
                    values: vec![value],
                },
            );
            if !self.unroll(inner, &scope, macro_name, call, out) {
                return None;
            }
        }
        Some(close + 1)
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
        // A bang call without argument tokens is a `builtin #` primitive,
        // never a macro invocation.
        if !self.has_args(*span) {
            return None;
        }
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
                                let outer = self.enter(span);
                                out.extend(self.items(expanded, depth + 1));
                                self.site = outer;
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
                        let outer = self.enter(span);
                        out.extend(self.members(expanded, depth + 1));
                        self.site = outer;
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
                        let outer = self.enter(span);
                        out.extend(self.stmts(expanded, sequential, depth + 1));
                        self.site = outer;
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
                let outer = self.enter(span);
                self.expr(&mut expanded, depth + 1);
                self.site = outer;
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
                    let builtin = matches!(callee.as_ref(), Expr::Path(p)
                        if p.segments.len() == 1
                            && matches!(p.segments[0].text.as_str(), "assert" | "print" | "warn"));
                    match self.take_args(*span) {
                        // `builtin # …`: a bang call with no argument tokens.
                        // Only `core`'s macro bodies may write it, and it
                        // reports where the outermost invocation is, as
                        // Rust's `line!()` does.
                        None => {
                            if self.body_of(*span) != Some(true) {
                                self.sink.emit(
                                    Diagnostic::error("`builtin #` is reserved to `core`'s macros")
                                        .with_code(codes::MACRO_EXPANSION)
                                        .at(*span)
                                        .help("call the macro `core` declares over it, such as `assert!`"),
                                );
                            }
                            if let Some(site) = self.site {
                                if builtin {
                                    *span = site;
                                }
                            }
                        }
                        // A `name!` no macro claims.
                        Some(call) => {
                            if let Expr::Path(path) = callee.as_ref() {
                                if !builtin || !call.parsed {
                                    let path = path.clone();
                                    self.unknown(&path);
                                }
                            }
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

    /// The next call's arguments at `span`, consumed.
    fn take_args(&mut self, span: Span) -> Option<MacroArgs> {
        let calls = self.args.get_mut(&span)?;
        (!calls.is_empty()).then(|| calls.remove(0))
    }

    /// Whether a call at `span` still has arguments waiting: a macro call
    /// rather than a `builtin #` primitive.
    fn has_args(&self, span: Span) -> bool {
        self.args.get(&span).is_some_and(|calls| !calls.is_empty())
    }

    /// Start walking an expansion of the call at `span`; the site to
    /// restore afterwards.
    fn enter(&mut self, span: Span) -> Option<Span> {
        let outer = self.site;
        self.site.get_or_insert(span);
        outer
    }

    /// Whether `span` lies in a macro body, and if so whether that macro is
    /// `core`'s.
    fn body_of(&self, span: Span) -> Option<bool> {
        self.bodies
            .iter()
            .find(|(b, _)| b.file == span.file && b.start <= span.start && span.end <= b.end)
            .map(|(_, core)| *core)
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

/// What a macro parameter is bound to while its body is substituted.
#[derive(Clone)]
struct Binding<'t> {
    /// The parameter's fragment kind.
    kind: FragmentKind,
    /// The arguments: one, or for a `...` parameter any number.
    values: Vec<&'t [MacroToken]>,
}

/// Push one argument: an `expr` as one parenthesized operand.
fn push_argument(
    kind: FragmentKind,
    tokens: &[MacroToken],
    call: Span,
    out: &mut Vec<(MacroToken, bool)>,
) {
    if kind != FragmentKind::Expr {
        out.extend(tokens.iter().map(|t| (t.clone(), false)));
        return;
    }
    let first = tokens.first().map_or(call, |t| t.span);
    let last = tokens.last().map_or(call, |t| t.span);
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
    out.extend(tokens.iter().map(|t| (t.clone(), false)));
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
}

/// The index of the delimiter closing the one at `open`.
fn matching_close(tokens: &[MacroToken], open: usize) -> Option<usize> {
    use TokenKind as K;
    let mut depth = 0usize;
    for (i, token) in tokens.iter().enumerate().skip(open) {
        match token.kind {
            K::LParen | K::LBracket | K::LBrace => depth += 1,
            K::RParen | K::RBracket | K::RBrace => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether `form` is declared in `core`.
fn form_home(index: &Index, form: &MacroDecl) -> bool {
    index
        .macros
        .iter()
        .find(|(_, forms)| forms.iter().any(|f| f.span == form.span))
        .is_some_and(|((module, _), _)| module == "core" || module.starts_with("core::"))
}
