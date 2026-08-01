//! Name resolution and module system for siox Phase 1 (spec Stage 3).
//!
//! Resolves identifiers to declarations: top-level item names, `using`
//! imports/aliases, `::` path resolution, associated items (`State::Idle`),
//! impl/instance type names, and attribute names. Each declaration gets a
//! stable [`DefId`]; every resolved use-site is recorded by span so later
//! stages (types, elaboration) can look up what a name refers to.
//!
//! Acceptance (spec Stage 3):
//! - unknown names reported ([`crate::diag::codes::UNKNOWN_NAME`])
//! - duplicate items reported ([`crate::diag::codes::DUPLICATE_ITEM`])
//! - attribute usage fails if the attribute was not declared/imported
//! - associated paths like `State::Idle` resolve correctly
//!
//! Phase-1 scope notes:
//! - The kernel base types (`integer`, `real`, `Char`) are seeded as builtins.
//!   Digital scalars such as `Bit`, `Logic`, and `Bool` resolve from std like
//!   any user enum.
//! - Type references, enum-variant paths, and attribute names are resolved
//!   strictly (an unknown one is an error). Plain value identifiers (signals,
//!   ports, locals) are resolved best-effort and never produce a false
//!   "unknown name" — full value/port/field scoping lands with type checking.
//! - Declarations still occupy one crate-wide namespace, but accesses from a
//!   different source module must cross a `pub` boundary.

use std::collections::{HashMap, HashSet};

use crate::diag::{codes, Diagnostic, DiagnosticSink, Span};
use crate::syntax::ast::*;
use crate::syntax::Module;

/// The single operator trait (spec 3.25): every operator dispatches through
/// `impl Operator<"<sym>", Input, Output> for T` (method `apply`), keyed by the
/// symbol in its first template argument. `a + b` -> `Operator<"+", _, _>`,
/// `and` -> `Operator<"and", _, _>`, unary `not` -> `Operator<"not", _, _>`,
/// and one three-way `Operator<"<=>", _, Ordering>` derives all six
/// comparisons. Seeded as a builtin so `impl Operator<..> for T` needs no
/// import.
pub const OPERATORS: &[&str] = &["Operator"];

/// Stable id for a resolved declaration. Later stages key off this instead of
/// raw names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DefId(pub u32);

/// What kind of thing a [`DefId`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefKind {
    /// Primitive type or seeded attribute (`Bit`, `unsigned`, `top`, ...).
    Builtin,
    Struct,
    View,
    Enum,
    EnumVariant,
    Entity,
    Trait,
    Const,
    /// A module-level function (inlined at lowering; const-evaluable).
    Fn,
    TypeAlias,
    /// Declared metadata attribute (`attr top: Bool for entity;`).
    Attr,
    /// Generic/elaboration parameter (`<W: integer>`).
    Param,
    /// `let`/`const`/method/mode-field name local to an impl or block.
    Local,
}

/// Metadata for one declaration.
#[derive(Clone, Debug)]
pub struct DefInfo {
    pub name: String,
    pub kind: DefKind,
    pub is_pub: bool,
    /// Declaration site, or `None` for builtins.
    pub span: Option<Span>,
    /// Owning definition, e.g. the enum a variant belongs to.
    pub parent: Option<DefId>,
}

/// The result of resolving a set of modules: the definition table plus a map
/// from every resolved name-use site (keyed by its span) to its [`DefId`].
#[derive(Default)]
pub struct Resolved {
    defs: Vec<DefInfo>,
    uses: HashMap<Span, DefId>,
}

impl Resolved {
    pub fn def(&self, id: DefId) -> Option<&DefInfo> {
        self.defs.get(id.0 as usize)
    }

    pub fn defs(&self) -> &[DefInfo] {
        &self.defs
    }

    /// The declaration a use-site (identified by its span) resolved to.
    pub fn resolved(&self, span: Span) -> Option<DefId> {
        self.uses.get(&span).copied()
    }

    pub fn kind_of(&self, id: DefId) -> Option<DefKind> {
        self.def(id).map(|d| d.kind)
    }
}

/// Resolve a crate's worth of parsed modules.
pub fn resolve(modules: &[Module], sink: &mut DiagnosticSink) -> Resolved {
    let mut r = Resolver::new(sink);
    r.seed_builtins();
    r.loaded_modules = modules
        .iter()
        .map(|m| {
            m.path
                .segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join("::")
        })
        .collect();
    for m in modules {
        for item in &m.items {
            r.collect_item(item);
        }
    }
    r.check_declaration_cycles(modules);
    r.inherit_enum_variants();
    for m in modules {
        for item in &m.items {
            r.resolve_imports(item);
        }
    }
    for m in modules {
        for item in &m.items {
            r.resolve_item(item);
        }
    }
    // The std library is not linted (its imports serve the whole library, not
    // this compilation); only warn about unused imports in the user's files.
    let std_files: std::collections::HashSet<crate::diag::FileId> = modules
        .iter()
        .filter(|m| m.path.segments.first().map(|s| s.text.as_str()) == Some("std"))
        .map(|m| m.span.file)
        .collect();
    r.lint_private_imports(&std_files);
    r.lint_unused_imports(&std_files);
    r.lint_unused_params(&std_files);
    r.out
}

/// The head identifier of a type expression (`unsigned[8]` -> `unsigned`), for
/// derivation-base lookup.
fn type_head(t: &Type) -> Option<&str> {
    match t {
        Type::Path(p) => p.segments.first().map(|s| s.text.as_str()),
        Type::Generic { base, .. } | Type::Indexed { base, .. } => type_head(base),
        Type::View { view, .. } => view.segments.last().map(|i| i.text.as_str()),
    }
}

struct Resolver<'a> {
    sink: &'a mut DiagnosticSink,
    out: Resolved,
    /// Module-level + builtin type/value namespace.
    globals: HashMap<String, DefId>,
    /// Attribute namespace (kept separate; attrs share no names with types).
    attrs: HashMap<String, DefId>,
    /// Enum `DefId` -> (variant name -> variant `DefId`).
    enum_variants: HashMap<DefId, HashMap<String, DefId>>,
    /// Enum name -> its `DefId`, and enum name -> base head name (derivation).
    enum_ids: HashMap<String, DefId>,
    enum_derives: HashMap<String, String>,
    /// Lexical scopes for params/locals, innermost last.
    scopes: Vec<HashMap<String, DefId>>,
    /// `using` import sites `(name span, imported DefId)`, for the unused-import
    /// lint after all references are resolved.
    import_sites: Vec<(Span, DefId)>,
    /// Generic-parameter declaration sites (`<W>`, `<T>` on an entity/struct/
    /// trait/fn), for the unused-parameter lint. Impl params are excluded — a
    /// type parameter used only in the impl target reads as used.
    param_sites: Vec<(Span, DefId)>,
    /// Declaration generic `(owner, name)` -> its definition, used to merge
    /// uses from a separate `impl Owner<T>` parameter scope.
    decl_params: HashMap<(String, String), DefId>,
    /// Owner -> its declared parameters in order, so an impl binder that
    /// *renames* (`impl<M> Counter<M>` for `entity Counter<W>`) can be matched
    /// to the declaration by position, the way Rust matches it.
    decl_param_order: HashMap<String, Vec<DefId>>,
    /// Inside the current impl: its binder's names mapped to the declaration
    /// parameters they stand for.
    current_impl_renames: HashMap<String, DefId>,
    impl_used_decl_params: HashSet<DefId>,
    current_impl_owner: Option<String>,
    /// The `module` path of every source that was actually loaded. A `using`
    /// naming a path absent from this set imports from a file the compiler
    /// never read, which is a different mistake from importing a name the
    /// module does not have — and used to be reported as the latter.
    loaded_modules: HashSet<String>,
}

impl<'a> Resolver<'a> {
    fn new(sink: &'a mut DiagnosticSink) -> Self {
        Resolver {
            sink,
            out: Resolved::default(),
            globals: HashMap::new(),
            attrs: HashMap::new(),
            enum_variants: HashMap::new(),
            enum_ids: HashMap::new(),
            enum_derives: HashMap::new(),
            scopes: Vec::new(),
            import_sites: Vec::new(),
            param_sites: Vec::new(),
            decl_params: HashMap::new(),
            decl_param_order: HashMap::new(),
            current_impl_renames: HashMap::new(),
            impl_used_decl_params: HashSet::new(),
            current_impl_owner: None,
            loaded_modules: HashSet::new(),
        }
    }

    fn seed_builtins(&mut self) {
        // The numeric and character kernels are intrinsic. Digital scalar
        // enums and indexed families come from std declarations.
        for name in ["integer", "real", "Char", "string", "range"] {
            let id = self.add_def(name.to_string(), DefKind::Builtin, true, None, None);
            self.globals.insert(name.to_string(), id);
        }
        // Operator traits and the literal suffix/prefix hooks are compiler
        // mechanisms (spec 3.24/3.25): `impl Add for T` / `impl Suffix for T`
        // need no trait declaration or import.
        for name in OPERATORS.iter().copied().chain(["Suffix", "Prefix"]) {
            let id = self.add_def(name.to_string(), DefKind::Builtin, true, None, None);
            self.globals.insert(name.to_string(), id);
        }
        // std::attrs metadata attributes (spec 3.5).
        for name in ["top", "test", "keep", "library", "name", "precedence"] {
            let id = self.add_def(name.to_string(), DefKind::Builtin, true, None, None);
            self.attrs.insert(name.to_string(), id);
        }
    }

    /// Nominal enum derivation: a derived enum's associated-variant paths
    /// (`Child::InheritedVariant`) resolve to the base's variants. Merge
    /// base-chain variant entries into each derived enum's table.
    fn inherit_enum_variants(&mut self) {
        let names: Vec<String> = self.enum_ids.keys().cloned().collect();
        for name in &names {
            // Walk the derivation chain (nearest base first) collecting the
            // variant maps of every ancestor enum.
            let mut inherited: Vec<HashMap<String, DefId>> = Vec::new();
            let mut cur = name.clone();
            let mut seen = HashSet::new();
            while let Some(base) = self.enum_derives.get(&cur).cloned() {
                if !seen.insert(cur.clone()) {
                    break;
                }
                let Some(&bid) = self.enum_ids.get(&base) else {
                    break;
                };
                if let Some(m) = self.enum_variants.get(&bid) {
                    inherited.push(m.clone());
                }
                cur = base;
            }
            if inherited.is_empty() {
                continue;
            }
            let id = self.enum_ids[name];
            let own = self.enum_variants.entry(id).or_default();
            // Ancestors furthest-first, without overwriting nearer/own entries.
            for m in inherited.into_iter().rev() {
                for (v, vid) in m {
                    own.entry(v).or_insert(vid);
                }
            }
        }
    }

    // --- collection (declarations) -----------------------------------------

    fn collect_item(&mut self, item: &Item) {
        match item {
            Item::Fn(f) => {
                self.declare(&f.name.text, DefKind::Fn, true, f.name.span);
            }
            Item::ExternBlock { fns, .. } => {
                for f in fns {
                    self.declare(&f.name.text, DefKind::Fn, true, f.name.span);
                }
            }
            Item::Using(u) => match &u.kind {
                UsingKind::Alias { name, .. } => {
                    self.declare(&name.text, DefKind::TypeAlias, u.is_pub, name.span);
                }
                // Imports bind to declarations from other loaded modules, so
                // they are validated after every module has been collected
                // (see `resolve_imports`).
                UsingKind::Import { .. } => {}
            },
            Item::Const(c) => {
                self.declare(&c.name.text, DefKind::Const, c.is_pub, c.name.span);
            }
            Item::Struct(s) => {
                self.declare(&s.name.text, DefKind::Struct, s.is_pub, s.name.span);
                // Two fields of one name give the struct two same-named leaf
                // signals; every later reference silently picks one.
                self.check_duplicate_names(
                    s.fields.iter().map(|f| (&f.name.text, f.name.span)),
                    "field",
                    &s.name.text,
                );
            }
            Item::View(v) => {
                // Views overload by backing struct (`Source Stream`,
                // `Source Queue`). Keep one name-resolution representative;
                // type checking selects the declaration from the applied pair.
                let id = self.add_def(
                    v.name.text.clone(),
                    DefKind::View,
                    v.is_pub,
                    Some(v.name.span),
                    None,
                );
                self.globals.entry(v.name.text.clone()).or_insert(id);
            }
            Item::Enum(e) => {
                let id = self.declare(&e.name.text, DefKind::Enum, e.is_pub, e.name.span);
                self.check_duplicate_names(
                    e.variants.iter().map(|v| (&v.name.text, v.name.span)),
                    "variant",
                    &e.name.text,
                );
                let mut vars = HashMap::new();
                for v in &e.variants {
                    let vid = self.add_def(
                        v.name.text.clone(),
                        DefKind::EnumVariant,
                        e.is_pub,
                        Some(v.name.span),
                        Some(id),
                    );
                    vars.insert(v.name.text.clone(), vid);
                }
                self.enum_variants.insert(id, vars);
                self.enum_ids.insert(e.name.text.clone(), id);
                if let Some(t) = &e.repr {
                    if let Some(h) = type_head(t) {
                        self.enum_derives.insert(e.name.text.clone(), h.to_string());
                    }
                }
            }
            Item::Entity(e) => {
                self.declare(&e.name.text, DefKind::Entity, e.is_pub, e.name.span);
                // Two ports of one name leave the second unreachable and make
                // a connection by name ambiguous.
                self.check_duplicate_names(
                    e.ports.iter().map(|p| (&p.name.text, p.name.span)),
                    "port",
                    &e.name.text,
                );
            }
            Item::Trait(t) => {
                self.declare(&t.name.text, DefKind::Trait, t.is_pub, t.name.span);
            }
            Item::AttrDecl(a) => {
                let id = self.add_def(
                    a.name.text.clone(),
                    DefKind::Attr,
                    a.is_pub,
                    Some(a.name.span),
                    None,
                );
                if self.attrs.contains_key(&a.name.text) {
                    // Redeclaring a seeded/known attribute is harmless; keep the
                    // user's declaration as the resolution target.
                }
                self.attrs.insert(a.name.text.clone(), id);
            }
            // Impls declare no top-level name.
            Item::Impl(_) => {}
        }
    }

    /// Unused-import lint (W-P005): a `using base::{name}` whose imported
    /// declaration is never referenced elsewhere in the same file. Usage is
    /// scoped by file (an import serves its own module), and the import's own
    /// name span is excluded so the binding doesn't count as a use of itself.
    /// Reject a `using` that imports a non-`pub` item from another module.
    fn lint_private_imports(
        &mut self,
        _std_files: &std::collections::HashSet<crate::diag::FileId>,
    ) {
        let sites = self.import_sites.clone();
        for (imp_span, id) in sites {
            let bad = self.out.def(id).map(|d| {
                (
                    d.name.clone(),
                    !d.is_pub
                        && d.span
                            .is_some_and(|decl_span| decl_span.file != imp_span.file),
                )
            });
            if let Some((name, true)) = bad {
                self.sink.emit(
                    Diagnostic::error(format!(
                        "`{name}` is not `pub`; importing a private item from another module"
                    ))
                    .with_code(codes::PRIVATE_IMPORT)
                    .at(imp_span)
                    .help("mark it `pub` in its module to export it"),
                );
            }
        }
    }

    fn lint_unused_imports(&mut self, std_files: &std::collections::HashSet<crate::diag::FileId>) {
        let sites = std::mem::take(&mut self.import_sites);
        for (imp_span, id) in sites {
            if std_files.contains(&imp_span.file) {
                continue;
            }
            let used = self
                .out
                .uses
                .iter()
                .any(|(s, d)| *d == id && s.file == imp_span.file && *s != imp_span);
            if !used {
                let name = self.out.def(id).map(|d| d.name.clone()).unwrap_or_default();
                self.sink.emit(
                    Diagnostic::warning(format!("unused import: `{name}`"))
                        .with_code(codes::UNUSED_IMPORT)
                        .at(imp_span)
                        .help("remove it"),
                );
            }
        }
    }

    /// Warn about a generic parameter that is never referenced. Declaration
    /// parameters are unified with their separately scoped implementation
    /// parameters through `decl_params`/`impl_used_decl_params`.
    fn lint_unused_params(&mut self, std_files: &std::collections::HashSet<crate::diag::FileId>) {
        let sites = std::mem::take(&mut self.param_sites);
        for (span, id) in sites {
            if std_files.contains(&span.file) {
                continue;
            }
            let used = self.impl_used_decl_params.contains(&id)
                || self
                    .out
                    .uses
                    .iter()
                    .any(|(s, d)| *d == id && s.file == span.file && *s != span);
            if !used {
                let name = self.out.def(id).map(|d| d.name.clone()).unwrap_or_default();
                self.sink.emit(
                    Diagnostic::warning(format!("unused type parameter: `{name}`"))
                        .with_code(codes::UNUSED_PARAM)
                        .at(span)
                        .help("remove it or use it in the signature or body"),
                );
            }
        }
    }

    /// Bind each `using base::{names}` name to the declaration another loaded
    /// module (or a builtin) provides. Runs after all modules are collected;
    /// an import that matches nothing is a hard error.
    fn resolve_imports(&mut self, item: &Item) {
        let Item::Using(u) = item else { return };
        let UsingKind::Import { base, names } = &u.kind else {
            return;
        };
        for n in names {
            let found = self
                .globals
                .get(&n.text)
                .or_else(|| self.attrs.get(&n.text))
                .copied();
            match found {
                Some(id) => {
                    self.out.uses.insert(n.span, id);
                    self.import_sites.push((n.span, id));
                }
                None => {
                    let base_str = base
                        .segments
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect::<Vec<_>>()
                        .join("::");
                    // A module that was never loaded is a different mistake
                    // from a module that lacks the name. Only `std::` paths
                    // are read from disk, so `using mylib::{Inc}` reported
                    // "no `Inc` in `mylib`" — blaming the import list for a
                    // file the compiler had never opened.
                    if !base.segments.is_empty() && !self.loaded_modules.contains(&base_str) {
                        self.sink.emit(
                            Diagnostic::error(format!("no module `{base_str}` was loaded"))
                                .with_code(codes::UNRESOLVED_IMPORT)
                                .at(base.span)
                                .help(
                                    "a compilation is one source file plus the standard \
                                     library: only `std::` paths are read from disk (via \
                                     `--std <dir>`), so a module declared in another file \
                                     is not visible here",
                                ),
                        );
                        continue;
                    }
                    let mut diag = Diagnostic::error(format!(
                        "unresolved import: no `{}` in `{base_str}`",
                        n.text
                    ))
                    .with_code(codes::UNRESOLVED_IMPORT)
                    .at(n.span);
                    // std::rand / std::fs ship runtime-provided functions that
                    // are documented but not declared — callable bare.
                    if matches!(base_str.as_str(), "std::rand" | "std::fs") {
                        diag = diag.help(format!(
                            "`{base_str}` functions are runtime-provided: call \
                             `{}(..)` directly, no import needed",
                            n.text
                        ));
                    } else if let Some(s) = self.suggest(&n.text) {
                        diag = diag.help(format!("did you mean `{s}`?"));
                    }
                    self.sink.emit(diag);
                }
            }
        }
    }

    /// A type declaration that reaches itself (`using A = B; using B = A`, or
    /// `struct A : B` with `struct B : A`) made every later stage recurse
    /// until the stack overflowed — the compiler aborted with a core dump.
    /// Report it once, here, where the declarations are all in hand.
    fn check_declaration_cycles(&mut self, modules: &[Module]) {
        // name -> (what it points at, where it was declared, what to call it)
        let mut edges: HashMap<&str, (&str, Span, &'static str)> = HashMap::new();
        for m in modules {
            for item in &m.items {
                match item {
                    Item::Using(u) => {
                        if let UsingKind::Alias { name, ty } = &u.kind {
                            if let Some(head) = type_head(ty) {
                                edges.insert(name.text.as_str(), (head, name.span, "alias"));
                            }
                        }
                    }
                    Item::Struct(st) => {
                        if let Some(head) = st.base.as_ref().and_then(type_head) {
                            edges.insert(st.name.text.as_str(), (head, st.name.span, "type"));
                        }
                    }
                    // An enum derives its variants from its base the way a
                    // struct derives its fields, so a cycle is just as
                    // meaningless — and this arm was missing, so
                    // `enum A(B); enum B(A);` reported nothing at all while
                    // the struct spelling of it was caught here.
                    Item::Enum(en) => {
                        if let Some(head) = en.repr.as_ref().and_then(type_head) {
                            edges.insert(en.name.text.as_str(), (head, en.name.span, "enum"));
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut reported: HashSet<&str> = HashSet::new();
        for &start in edges.keys() {
            let mut seen: HashSet<&str> = HashSet::new();
            let mut cur = start;
            while let Some(&(next, span, what)) = edges.get(cur) {
                if !seen.insert(cur) {
                    // Only the first name of each cycle reports it.
                    if reported.insert(cur) {
                        self.sink.emit(
                            Diagnostic::error(format!(
                                "{what} `{cur}` is defined in terms of itself"
                            ))
                            .with_code(codes::DUPLICATE_ITEM)
                            .at(span)
                            .help("break the cycle: a type cannot derive from itself"),
                        );
                    }
                    break;
                }
                cur = next;
            }
        }
    }

    /// Report a repeated member name within one declaration (a struct's fields
    /// or an enum's variants). Unlike a top-level clash these never reached
    /// [`Self::declare`], so they used to pass silently — leaving an ambiguous
    /// `S::A`, or a struct with two identically-named leaf signals.
    fn check_duplicate_names<'n>(
        &mut self,
        names: impl Iterator<Item = (&'n String, Span)>,
        what: &str,
        owner: &str,
    ) {
        let mut seen: HashMap<&'n str, Span> = HashMap::new();
        for (name, span) in names {
            if let Some(prev) = seen.insert(name.as_str(), span) {
                self.sink.emit(
                    Diagnostic::error(format!("duplicate {what} `{name}` in `{owner}`"))
                        .with_code(codes::DUPLICATE_ITEM)
                        .at(span)
                        .label(prev, format!("`{name}` first declared here"))
                        .help("rename or remove one of them"),
                );
            }
        }
    }

    /// Add a named global, reporting a duplicate when it collides with another
    /// user declaration (shadowing a builtin is allowed).
    fn declare(&mut self, name: &str, kind: DefKind, is_pub: bool, span: Span) -> DefId {
        let id = self.add_def(name.to_string(), kind, is_pub, Some(span), None);
        if let Some(prev) = self.globals.get(name).copied() {
            if self.out.kind_of(prev) != Some(DefKind::Builtin) {
                let mut diag = Diagnostic::error(format!("duplicate item `{name}`"))
                    .with_code(codes::DUPLICATE_ITEM)
                    .at(span)
                    .help("rename or remove one of them");
                if let Some(prev_span) = self.out.def(prev).and_then(|d| d.span) {
                    diag = diag.label(prev_span, format!("`{name}` first declared here"));
                }
                self.sink.emit(diag);
                return id; // keep the first declaration as the resolution target
            }
        }
        self.globals.insert(name.to_string(), id);
        id
    }

    fn add_def(
        &mut self,
        name: String,
        kind: DefKind,
        is_pub: bool,
        span: Option<Span>,
        parent: Option<DefId>,
    ) -> DefId {
        let id = DefId(self.out.defs.len() as u32);
        self.out.defs.push(DefInfo {
            name,
            kind,
            is_pub,
            span,
            parent,
        });
        id
    }

    // --- resolution (uses) --------------------------------------------------

    fn resolve_item(&mut self, item: &Item) {
        match item {
            // An alias target is a type reference and must resolve — a
            // `using Word = NoSuchType;` used to pass silently, leaving every
            // signal typed through it at unknown width.
            Item::Using(u) => {
                if let UsingKind::Alias { ty, .. } = &u.kind {
                    self.resolve_type(ty);
                }
            }
            Item::Fn(f) => {
                // A generic fn's type params (`<T: Ord>`) scope over its
                // signature, so `a: T` resolves.
                self.enter();
                self.bind_params(&f.generics, true, None);
                for p in &f.params {
                    if let Some(t) = &p.ty {
                        self.resolve_type(t);
                    }
                }
                if let Some(t) = &f.ret {
                    self.resolve_type(t);
                }
                self.exit();
            }
            Item::ExternBlock { .. } => {}
            Item::Const(c) => {
                self.resolve_type(&c.ty);
                self.resolve_expr(&c.value);
            }
            Item::Struct(s) => {
                self.enter();
                self.bind_params(&s.params, true, Some(&s.name.text));
                // The derivation base is a type reference like any other —
                // it was the one spot that never got resolved, so
                // `struct B : NoSuchType` passed silently.
                if let Some(base) = &s.base {
                    self.resolve_type(base);
                }
                for f in &s.fields {
                    self.resolve_type(&f.ty);
                }
                self.exit();
            }
            Item::View(v) => {
                self.enter();
                self.bind_params(&v.params, true, Some(&v.name.text));
                self.resolve_type(&v.target);
                self.exit();
            }
            Item::Enum(e) => {
                if let Some(repr) = &e.repr {
                    self.resolve_type(repr);
                }
                for v in &e.variants {
                    if let Some(val) = &v.value {
                        self.resolve_expr(val);
                    }
                }
            }
            Item::Entity(e) => {
                self.enter();
                self.bind_params(&e.params, true, Some(&e.name.text));
                for a in &e.attrs {
                    self.resolve_attr(a);
                }
                for p in &e.ports {
                    self.resolve_type(&p.ty);
                }
                self.exit();
            }
            Item::Impl(im) => self.resolve_impl(im),
            Item::Trait(t) => {
                self.enter();
                self.bind_params(&t.params, true, Some(&t.name.text));
                // `Self` refers to the implementing type inside a trait body.
                self.bind_local("Self");
                for f in &t.items {
                    self.resolve_fn(f);
                }
                self.exit();
            }
            Item::AttrDecl(a) => self.resolve_type(&a.ty),
        }
    }

    fn resolve_impl(&mut self, im: &ImplDecl) {
        self.enter();
        self.bind_params(&im.params, false, None);
        // `impl Reg<T>` declares the type parameter `T` for the body (like
        // Rust's `impl<T> Reg<T>`): a bare single-name generic argument on the
        // target that isn't already a known type is a type parameter.
        let generic_target = match &im.target {
            Type::Generic { args, .. } => Some(args),
            Type::View { target, .. } => match target.as_ref() {
                Type::Generic { args, .. } => Some(args),
                _ => None,
            },
            _ => None,
        };
        if let Some(args) = generic_target {
            for a in args {
                if let GenericArg::Positional(Expr::Path(p)) = a {
                    if p.segments.len() == 1 && self.lookup(&p.segments[0].text).is_none() {
                        let name = p.segments[0].text.clone();
                        let id = self.add_def(
                            name.clone(),
                            DefKind::Param,
                            false,
                            Some(p.segments[0].span),
                            None,
                        );
                        self.bind(&name, id);
                    }
                }
            }
        }
        // `Self` refers to the impl target type inside the body.
        self.bind_local("Self");
        // Impl-level names are visible to the whole body regardless of order.
        for it in &im.items {
            match it {
                ImplItem::Let(l) => self.bind_local(&l.name.text),
                ImplItem::Const(c) => self.bind_local(&c.name.text),
                ImplItem::Fn(f) => self.bind_local(&f.name.text),
                ImplItem::ModeField { name, .. } => self.bind_local(&name.text),
                ImplItem::Stmt(_) => {}
            }
        }
        self.resolve_type(&im.target);
        for attr in &im.attrs {
            self.resolve_attr(attr);
        }
        if let Some(tr) = &im.trait_ {
            self.resolve_type_path(tr);
        }
        for arg in &im.trait_args {
            match arg {
                GenericArg::Positional(value) | GenericArg::Named { value, .. } => {
                    self.resolve_expr(value);
                }
            }
        }
        // The impl target necessarily mentions its parameters (`Owner<T>`);
        // that alone does not make the declaration's `T` meaningful. Snapshot
        // use counts here, then only merge parameters referenced by an impl
        // item/body into the owning declaration's lint facts.
        let impl_params: Vec<(String, DefId, usize)> = self
            .scopes
            .last()
            .into_iter()
            .flat_map(|scope| scope.iter())
            .filter(|(_, id)| self.out.kind_of(**id) == Some(DefKind::Param))
            .map(|(name, &id)| {
                let uses = self.out.uses.values().filter(|&&used| used == id).count();
                (name.clone(), id, uses)
            })
            .collect();
        // `impl<M: integer> Counter<M>`: the binder names are the impl's own,
        // matched to the declaration by position. Without this the lint looked
        // the declaration's `W` up by the *name* `M` and found nothing, so a
        // renamed parameter counted as unused.
        let saved_renames = std::mem::take(&mut self.current_impl_renames);
        if let Some(owner) = type_head(&im.target) {
            if let Some(order) = self.decl_param_order.get(owner).cloned() {
                let args = match &im.target {
                    Type::Generic { args, .. } => args.as_slice(),
                    _ => &[],
                };
                for (i, arg) in args.iter().enumerate() {
                    let GenericArg::Positional(Expr::Path(path)) = arg else {
                        continue;
                    };
                    let ([seg], Some(&decl_id)) = (path.segments.as_slice(), order.get(i)) else {
                        continue;
                    };
                    self.current_impl_renames.insert(seg.text.clone(), decl_id);
                }
            }
        }
        self.check_impl_binder(im);
        let saved_impl_owner = self
            .current_impl_owner
            .replace(type_head(&im.target).unwrap_or_default().to_string());
        for it in &im.items {
            self.resolve_impl_item(it);
        }
        self.current_impl_owner = saved_impl_owner;
        self.current_impl_renames = saved_renames;
        if let Some(owner) = type_head(&im.target) {
            for (name, impl_id, before) in impl_params {
                let after = self
                    .out
                    .uses
                    .values()
                    .filter(|&&used| used == impl_id)
                    .count();
                if after > before {
                    if let Some(&decl_id) = self.decl_params.get(&(owner.to_string(), name.clone()))
                    {
                        self.impl_used_decl_params.insert(decl_id);
                    }
                }
            }
        }
        self.exit();
    }

    fn resolve_impl_item(&mut self, item: &ImplItem) {
        match item {
            ImplItem::Const(c) => {
                self.resolve_type(&c.ty);
                self.resolve_expr(&c.value);
            }
            ImplItem::Let(l) => {
                if let Some(t) = &l.ty {
                    self.resolve_type(t);
                }
                if let Some(v) = &l.value {
                    self.resolve_expr(v);
                }
            }
            ImplItem::Fn(f) => self.resolve_fn(f),
            ImplItem::ModeField { .. } => {}
            ImplItem::Stmt(s) => self.resolve_stmt(s),
        }
    }

    fn resolve_fn(&mut self, f: &FnDecl) {
        self.enter();
        let mut has_self = false;
        for p in &f.params {
            if p.is_self {
                has_self = true;
            }
            if let Some(name) = &p.name {
                self.bind_local(&name.text);
            }
            if let Some(t) = &p.ty {
                self.resolve_type(t);
            }
        }
        if has_self {
            self.bind_local("self");
        }
        if let Some(r) = &f.ret {
            self.resolve_type(r);
        }
        if let Some(body) = &f.body {
            self.resolve_block(body);
        }
        self.exit();
    }

    fn resolve_block(&mut self, b: &Block) {
        self.enter();
        for s in &b.stmts {
            if let Stmt::Let(l) = s {
                self.bind_local(&l.name.text);
            }
        }
        for s in &b.stmts {
            self.resolve_stmt(s);
        }
        self.exit();
    }

    fn resolve_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let(l) => {
                if let Some(t) = &l.ty {
                    self.resolve_type(t);
                }
                if let Some(v) = &l.value {
                    self.resolve_expr(v);
                }
            }
            Stmt::Assign { target, value, .. } => {
                self.resolve_expr(target);
                self.resolve_expr(value);
            }
            Stmt::If(i) => self.resolve_if(i),
            Stmt::Match(m) => {
                self.resolve_expr(&m.scrutinee);
                for arm in &m.arms {
                    self.resolve_pattern(&arm.pattern);
                    self.resolve_block(&arm.body);
                }
            }
            Stmt::For {
                var, range, body, ..
            } => {
                self.resolve_expr(range);
                self.enter();
                self.bind_local(&var.text);
                self.resolve_block(body);
                self.exit();
            }
            Stmt::Expr(e) => self.resolve_expr(e),
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.resolve_expr(v);
                }
            }
        }
    }

    fn resolve_if(&mut self, i: &IfStmt) {
        self.resolve_expr(&i.cond);
        self.resolve_block(&i.then);
        match i.else_.as_deref() {
            Some(ElseBranch::Block(b)) => self.resolve_block(b),
            Some(ElseBranch::If(inner)) => self.resolve_if(inner),
            None => {}
        }
    }

    fn resolve_attr(&mut self, a: &Attr) {
        let segs = &a.name.segments;
        let last = segs.last().map(|s| s.text.as_str()).unwrap_or("");
        if segs.len() == 1 {
            if let Some(id) = self.attrs.get(last).copied() {
                self.out.uses.insert(a.name.span, id);
            } else {
                self.error(
                    codes::UNKNOWN_NAME,
                    a.name.span,
                    format!("unknown attribute `{last}` (declare it with `attr` before use)"),
                );
            }
        } else if let Some(id) = self.attrs.get(last).copied() {
            self.record_qualified_use(a.name.span, id);
        }
        if let Some(v) = &a.value {
            self.resolve_expr(v);
        }
    }

    fn resolve_type(&mut self, ty: &Type) {
        match ty {
            Type::Path(p) => self.resolve_type_path(p),
            Type::Indexed { base, index, .. } => {
                self.resolve_type(base);
                if let Some(index) = index {
                    self.resolve_expr(index);
                }
            }
            Type::Generic { base, args, .. } => {
                self.resolve_type(base);
                for a in args {
                    match a {
                        GenericArg::Positional(e) => self.resolve_expr(e),
                        GenericArg::Named { value, .. } => self.resolve_expr(value),
                    }
                }
            }
            Type::View { view, target, .. } => {
                self.resolve_type_path(view);
                self.resolve_type(target);
            }
        }
    }

    fn resolve_type_path(&mut self, p: &Path) {
        if p.segments.is_empty() {
            return;
        }
        if p.segments.len() == 1 {
            let name = p.segments[0].text.clone();
            if let Some(id) = self.lookup(&name) {
                self.out.uses.insert(p.span, id);
                self.mark_impl_param_use(id);
            } else {
                let help = match self.suggest(&name) {
                    Some(s) => format!("did you mean `{s}`?"),
                    None => "declare it, or import it with `using`".to_string(),
                };
                self.sink.emit(
                    Diagnostic::error(format!("unknown type `{name}`"))
                        .with_code(codes::UNKNOWN_NAME)
                        .at(p.span)
                        .help(help),
                );
            }
        } else {
            let last = p.segments.last().unwrap().text.clone();
            if let Some(id) = self.globals.get(&last).copied() {
                self.record_qualified_use(p.span, id);
            }
        }
    }

    fn resolve_expr(&mut self, e: &Expr) {
        match e {
            // Literal leaves; a suffix (`1ns`) is not a value path — it binds
            // to a suffix definition during type checking.
            Expr::Int { .. }
            | Expr::SuffixLit { .. }
            | Expr::BitStrLit { .. }
            | Expr::CharLit { .. }
            | Expr::StrLit { .. } => {}
            Expr::Path(p) => self.resolve_value_path(p),
            Expr::IfExpr {
                cond, then, els, ..
            } => {
                self.resolve_expr(cond);
                self.resolve_expr(then);
                self.resolve_expr(els);
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.resolve_expr(scrutinee);
                for arm in arms {
                    self.resolve_pattern(&arm.pattern);
                    self.resolve_block(&arm.body);
                }
            }
            Expr::Field { base, .. } => self.resolve_expr(base),
            Expr::SysAttr { base, .. } => self.resolve_expr(base),
            Expr::Index { base, index, .. } => {
                self.resolve_expr(base);
                self.resolve_expr(index);
            }
            Expr::Range { lo, hi, .. } => {
                self.resolve_expr(lo);
                self.resolve_expr(hi);
            }
            Expr::PartialRange { lo, hi, .. } => {
                if let Some(lo) = lo {
                    self.resolve_expr(lo);
                }
                if let Some(hi) = hi {
                    self.resolve_expr(hi);
                }
            }
            Expr::Unary { rhs, .. } => self.resolve_expr(rhs),
            Expr::Binary { lhs, rhs, .. } => {
                self.resolve_expr(lhs);
                self.resolve_expr(rhs);
            }
            Expr::Call { callee, args, .. } => {
                self.resolve_expr(callee);
                for a in args {
                    self.resolve_expr(a);
                }
            }
            Expr::Construct { ty, args, .. } => {
                if let Some(ty) = ty {
                    self.resolve_type(ty);
                }
                for c in args {
                    if let Some(v) = &c.value {
                        self.resolve_expr(v);
                    }
                }
            }
            Expr::Concat { parts, .. } => {
                for p in parts {
                    self.resolve_expr(p);
                }
            }
            Expr::Array { elems, .. } => {
                for e in elems {
                    self.resolve_expr(e);
                }
            }
        }
    }

    /// Resolve a value-position path. `Enum::Variant` is checked strictly;
    /// a plain identifier is recorded if known but never errors if not (signal
    /// / port / field scoping is completed by the type checker).
    /// Resolve a match pattern's names — an enum-variant path, or each
    /// alternative of an or-pattern (`A | B`).
    fn resolve_pattern(&mut self, pattern: &Pattern) {
        match pattern {
            Pattern::Path(p) => self.resolve_value_path(p),
            Pattern::Or { alts, .. } => {
                for a in alts {
                    self.resolve_pattern(a);
                }
            }
            _ => {}
        }
    }

    fn resolve_value_path(&mut self, p: &Path) {
        if p.segments.len() >= 2 {
            let head = p.segments[0].text.clone();
            if let Some(id) = self.lookup(&head) {
                if self.out.kind_of(id) == Some(DefKind::Enum) {
                    let var = p.segments[1].text.clone();
                    // `Phase::new()` is the `New` trait's constructor, not a
                    // variant — the same default `Phase()` builds. Resolving
                    // `Enum::x` as a variant first made the documented pair
                    // disagree: `Pair::new()` worked and `Phase::new()` was
                    // "not a variant of enum `Phase`".
                    if var == "new" {
                        self.out.uses.insert(p.segments[0].span, id);
                        return;
                    }
                    match self.variant(id, &var) {
                        Some(vid) => {
                            self.out.uses.insert(p.span, vid);
                            // Record the qualifier too. A derived enum shares
                            // its base's variant ids, so `Mid::B` and `Base::B`
                            // resolve to the same def — only the name written
                            // here says which type the value has. It also marks
                            // the enum itself used, for the import lint.
                            //
                            // Skip it when the spans coincide: a desugared path
                            // (`true` -> `Bool::true`) synthesizes a qualifier
                            // over the whole path, and overwriting that entry
                            // would lose the variant the path resolves to.
                            if p.segments[0].span != p.span {
                                self.out.uses.insert(p.segments[0].span, id);
                            }
                        }
                        None => self.error(
                            codes::UNKNOWN_NAME,
                            p.span,
                            format!("`{var}` is not a variant of enum `{head}`"),
                        ),
                    }
                    return;
                }
                self.out.uses.insert(p.segments[0].span, id);
                return;
            }
            // A module-qualified declaration (`pkg::VALUE`). Modules are not
            // definitions themselves yet, so resolve the exported leaf.
            if let Some(last) = p.segments.last() {
                if let Some(id) = self.globals.get(&last.text).copied() {
                    self.record_qualified_use(p.span, id);
                }
            }
        } else if let Some(name) = p.segments.first() {
            if let Some(id) = self.lookup(&name.text) {
                self.out.uses.insert(p.span, id);
                self.mark_impl_param_use(id);
            }
        }
    }

    /// A generic entity's inherent impl must bind its parameters the way Rust
    /// does: `impl<W: integer> Counter<W>` introduces `W` and applies it to
    /// the target. Two shapes used to be accepted and are not equivalent —
    /// `impl<W: integer> Counter<W>` uses `W` without binding it, and bare
    /// `impl Counter` leaves the parameters implicit. Only view-applied
    /// targets (`impl Stream<T> StreamSource`) and trait impls are exempt.
    fn check_impl_binder(&mut self, im: &ImplDecl) {
        if im.trait_.is_some() || matches!(im.target, Type::View { .. }) {
            return;
        }
        let Some(owner) = type_head(&im.target) else {
            return;
        };
        let Some(declared) = self.decl_param_order.get(owner).map(Vec::len) else {
            return;
        };
        if declared == 0 {
            return;
        }
        let owner = owner.to_string();
        let span = im.span;
        let args = match &im.target {
            Type::Generic { args, .. } => args.len(),
            _ => 0,
        };
        if args == 0 {
            let message = if im.params.params.is_empty() {
                format!("missing generic arguments for `{owner}`")
            } else {
                format!("`{owner}` is written without its generic arguments here")
            };
            self.sink.emit(
                Diagnostic::error(message)
                    .with_code(codes::TYPE_MISMATCH)
                    .at(span)
                    .help(format!(
                        "bind the parameters and apply them: \
                         `impl<..> {owner}<..>`, as `impl<T> Vec<T>` does in Rust"
                    )),
            );
            return;
        }
        if args != declared {
            self.sink.emit(
                Diagnostic::error(format!(
                    "`{owner}` declares {declared} parameter(s) but {args} were applied here"
                ))
                .with_code(codes::TYPE_MISMATCH)
                .at(span),
            );
            return;
        }
        if im.params.params.len() != declared {
            self.sink.emit(
                Diagnostic::error(format!(
                    "`impl` binds {} parameter(s) but applies {args} to `{owner}`",
                    im.params.params.len()
                ))
                .with_code(codes::TYPE_MISMATCH)
                .at(span)
                .help("every parameter applied to the target must be bound by the `impl`"),
            );
        }
    }

    fn mark_impl_param_use(&mut self, id: DefId) {
        if self.out.kind_of(id) != Some(DefKind::Param) {
            return;
        }
        let Some(owner) = self.current_impl_owner.as_deref() else {
            return;
        };
        let Some(name) = self.out.def(id).map(|def| def.name.clone()) else {
            return;
        };
        if let Some(&decl_id) = self.current_impl_renames.get(&name) {
            self.impl_used_decl_params.insert(decl_id);
            return;
        }
        if let Some(&decl_id) = self.decl_params.get(&(owner.to_string(), name)) {
            self.impl_used_decl_params.insert(decl_id);
        }
    }

    fn record_qualified_use(&mut self, use_span: Span, id: DefId) {
        let private = self.out.def(id).and_then(|d| {
            d.span
                .filter(|decl_span| !d.is_pub && decl_span.file != use_span.file)
                .map(|decl_span| (d.name.clone(), decl_span))
        });
        if let Some((name, decl_span)) = private {
            self.sink.emit(
                Diagnostic::error(format!(
                    "`{name}` is private and cannot be accessed from another module"
                ))
                .with_code(codes::PRIVATE_IMPORT)
                .at(use_span)
                .label(decl_span, "declared private here")
                .help("mark it `pub` in its module to export it"),
            );
        } else {
            self.out.uses.insert(use_span, id);
        }
    }

    // --- scopes & lookup ----------------------------------------------------

    fn enter(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn exit(&mut self) {
        self.scopes.pop();
    }

    /// `lint`: record each param for the unused-parameter lint (true for a
    /// declaration's generics, false for an impl's — an impl type param used
    /// only in the target would otherwise read as unused).
    fn bind_params(&mut self, params: &Params, lint: bool, owner: Option<&str>) {
        for p in &params.params {
            let id = self.add_def(
                p.name.text.clone(),
                DefKind::Param,
                false,
                Some(p.name.span),
                None,
            );
            self.bind(&p.name.text, id);
            if lint {
                self.param_sites.push((p.name.span, id));
                if let Some(owner) = owner {
                    self.decl_params
                        .insert((owner.to_string(), p.name.text.clone()), id);
                    self.decl_param_order
                        .entry(owner.to_string())
                        .or_default()
                        .push(id);
                }
            }
            if let Some(bound) = &p.bound {
                self.resolve_type(bound);
            }
        }
    }

    fn bind_local(&mut self, name: &str) {
        let id = self.add_def(name.to_string(), DefKind::Local, false, None, None);
        self.bind(name, id);
    }

    fn bind(&mut self, name: &str, id: DefId) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), id);
        }
    }

    fn lookup(&self, name: &str) -> Option<DefId> {
        for scope in self.scopes.iter().rev() {
            if let Some(id) = scope.get(name) {
                return Some(*id);
            }
        }
        self.globals.get(name).copied()
    }

    /// The closest in-scope name to `name` (edit distance <= 2), for a
    /// "did you mean?" suggestion.
    fn suggest(&self, name: &str) -> Option<String> {
        let candidates = self
            .scopes
            .iter()
            .flat_map(|s| s.keys())
            .chain(self.globals.keys());
        let mut best: Option<(usize, &String)> = None;
        for cand in candidates {
            let d = levenshtein(name, cand);
            if (1..=2).contains(&d) && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, cand));
            }
        }
        best.map(|(_, s)| s.clone())
    }

    fn variant(&self, enum_id: DefId, name: &str) -> Option<DefId> {
        self.enum_variants
            .get(&enum_id)
            .and_then(|m| m.get(name))
            .copied()
    }

    fn error(&mut self, code: &'static str, span: Span, msg: String) {
        self.sink
            .emit(Diagnostic::error(msg).with_code(code).at(span));
    }
}

/// Levenshtein edit distance between two ASCII-ish identifiers.
fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::FileId;

    const DIGITAL_PRELUDE: &str = "\n\
        enum Bit { '0', '1' }\n\
        enum Logic { '0', '1', 'Z', 'X', 'U', 'W', 'L', 'H', '-' }\n\
        enum Bool { false, true }\n";

    fn resolve_src(src: &str) -> (Resolved, usize) {
        let src = format!("{src}{DIGITAL_PRELUDE}");
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        assert_eq!(sink.error_count(), 0, "source failed to parse:\n{src}");
        let resolved = resolve(std::slice::from_ref(&module), &mut sink);
        (resolved, sink.error_count())
    }

    /// Only `std::` paths are read from disk, so importing from a module in
    /// another file named one that was never opened. Reporting it as "no `Inc`
    /// in `mylib`" blamed the import list for a file the compiler had not read.
    #[test]
    fn importing_from_an_unloaded_module_says_so() {
        let sink = diagnostics("module m;\nusing mylib::{Inc};\n");
        let d = sink
            .diagnostics()
            .iter()
            .find(|d| d.message.contains("mylib"))
            .expect("a diagnostic naming the module");
        assert!(
            d.message.contains("no module `mylib` was loaded"),
            "{:?}",
            d.message
        );
        assert!(d.help.as_ref().is_some_and(|h| h.contains("--std")));
    }

    /// A module that *was* loaded and lacks the name keeps the message that
    /// describes that, so the two failures stay distinguishable.
    #[test]
    fn importing_a_missing_name_from_a_loaded_module_is_unchanged() {
        let sink = diagnostics("module m;\nusing m::{NoSuch};\n");
        let d = sink
            .diagnostics()
            .iter()
            .find(|d| d.message.contains("NoSuch"))
            .expect("a diagnostic naming the import");
        assert!(
            d.message.contains("unresolved import: no `NoSuch` in `m`"),
            "{:?}",
            d.message
        );
    }

    /// Resolve and return the raw diagnostics, for inspecting help/labels.
    fn diagnostics(src: &str) -> DiagnosticSink {
        let src = format!("{src}{DIGITAL_PRELUDE}");
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        resolve(std::slice::from_ref(&module), &mut sink);
        sink
    }

    #[test]
    fn unused_import_lint() {
        // A provider module (`std::lib`) and a user module that imports two of
        // its names but only uses one. The unused one warns; the used one and
        // the std module's own items do not.
        let mut sink = DiagnosticSink::new();
        let provider = crate::syntax::parse_module(
            FileId(0),
            "module std::lib;\npub enum Used { A, B }\npub enum Dead { C }\n",
            &mut sink,
        );
        let user = crate::syntax::parse_module(
            FileId(1),
            "module m;\nusing std::lib::{Used, Dead};\nentity E { a: Used in, }\n",
            &mut sink,
        );
        resolve(&[provider, user], &mut sink);
        let unused: Vec<&str> = sink
            .diagnostics()
            .iter()
            .filter(|d| d.code == Some(codes::UNUSED_IMPORT))
            .map(|d| d.message.as_str())
            .collect();
        assert_eq!(unused.len(), 1, "one unused-import warning: {unused:?}");
        assert!(
            unused[0].contains("Dead"),
            "flags Dead, not Used: {unused:?}"
        );
    }

    #[test]
    fn private_import_is_rejected() {
        // A provider with a private and a `pub` item; a user imports both. Only
        // importing the non-`pub` one is a cross-module visibility violation.
        let mut sink = DiagnosticSink::new();
        let provider = crate::syntax::parse_module(
            FileId(0),
            "module a;\nenum Secret { A }\npub enum Public { B }\n",
            &mut sink,
        );
        let user = crate::syntax::parse_module(
            FileId(1),
            "module m;\nusing a::{Secret, Public};\nentity E { s: Secret in, p: Public in, }\n",
            &mut sink,
        );
        resolve(&[provider, user], &mut sink);
        let private_errors: Vec<&str> = sink
            .diagnostics()
            .iter()
            .filter(|d| d.code == Some(codes::PRIVATE_IMPORT))
            .map(|d| d.message.as_str())
            .collect();
        assert_eq!(
            private_errors.len(),
            1,
            "one private-import error: {private_errors:?}"
        );
        assert!(
            private_errors[0].contains("Secret"),
            "flags Secret, not Public: {private_errors:?}"
        );
    }

    #[test]
    fn qualified_private_access_is_rejected() {
        let mut sink = DiagnosticSink::new();
        let provider = crate::syntax::parse_module(
            FileId(0),
            "module a;\nenum Secret { A }\npub enum Public { B }\n",
            &mut sink,
        );
        let user = crate::syntax::parse_module(
            FileId(1),
            "module m;\nentity E { s: a::Secret in, p: a::Public in, }\n",
            &mut sink,
        );
        resolve(&[provider, user], &mut sink);
        assert_eq!(
            sink.diagnostics()
                .iter()
                .filter(|d| d.code == Some(codes::PRIVATE_IMPORT))
                .count(),
            1
        );
    }

    #[test]
    fn pub_using_alias_is_exported() {
        let mut sink = DiagnosticSink::new();
        let provider =
            crate::syntax::parse_module(FileId(0), "module a;\npub using Word = Bit;\n", &mut sink);
        let user = crate::syntax::parse_module(
            FileId(1),
            "module m;\nusing a::Word;\nentity E { w: Word in, }\n",
            &mut sink,
        );
        resolve(&[provider, user], &mut sink);
        assert!(
            sink.diagnostics()
                .iter()
                .all(|d| d.code != Some(codes::PRIVATE_IMPORT)),
            "{:?}",
            sink.diagnostics()
        );
    }

    #[test]
    fn unused_fn_type_parameter_lint() {
        // `dead`'s `<T>` is never referenced; `used`'s `<T>` is used in the
        // signature. Only the dead one warns.
        let sink = diagnostics(
            "module m;\n\
             fn used<T: Ord>(x: T) -> T { return x; }\n\
             fn dead<T>() -> integer { return 0; }\n",
        );
        let unused: Vec<&str> = sink
            .diagnostics()
            .iter()
            .filter(|d| d.code == Some(codes::UNUSED_PARAM))
            .map(|d| d.message.as_str())
            .collect();
        assert_eq!(unused.len(), 1, "one unused-param warning: {unused:?}");
        assert!(unused[0].contains("`T`"), "flags the dead T: {unused:?}");
    }

    /// A parameter used only as a *value* recorded no use, so the lint told
    /// the author to delete a parameter the design computes with. With a
    /// Rust-style binder the name is the implementation's own, so the lint
    /// also has to follow a *rename* back to the declaration by position.
    #[test]
    fn a_parameter_used_as_a_value_counts_as_used() {
        let unused = |src: &str| {
            diagnostics(src)
                .diagnostics()
                .iter()
                .filter(|d| d.code == Some(codes::UNUSED_PARAM))
                .count()
        };
        assert_eq!(
            unused(
                "module m;\n\
                 entity E<N: integer> { a: Bit in, y: Bit out }\n\
                 impl<N: integer> E<N> { y = if a == N { '1' } else { '0' }; }\n"
            ),
            0,
            "value use under the declaration's own name"
        );
        // The binder renames; the declaration's parameter is still used.
        assert_eq!(
            unused(
                "module m;\n\
                 entity E<N: integer> { a: Bit in, y: Bit out }\n\
                 impl<M: integer> E<M> { y = if a == M { '1' } else { '0' }; }\n"
            ),
            0,
            "value use under a renamed binder"
        );
        // A genuinely unused parameter is still reported.
        assert_eq!(
            unused(
                "module m;\n\
                 entity E<N: integer> { a: Bit in, y: Bit out }\n\
                 impl<N: integer> E<N> { y = a; }\n"
            ),
            1,
            "unused"
        );
    }

    /// Rust binds a generic implementation's parameters and applies them to
    /// the target; siox used to accept three spellings that are not
    /// equivalent, one of which left the parameters implicit entirely.
    #[test]
    fn a_generic_impl_must_bind_and_apply_its_parameters() {
        let errs = |src: &str| resolve_src(src).1;
        // `unsigned` is not in this helper's prelude, so the parameter is
        // exercised as a value rather than as a width.
        let entity = "module m;\nentity E<W: integer> { a: Bit in, y: Bit out }\n";
        let with = |head: &str, param: &str| {
            format!("{entity}impl {head} {{ y = if a == {param} {{ '1' }} else {{ '0' }}; }}\n")
        };
        // The canonical form, and the same implementation renamed.
        assert_eq!(errs(&with("<W: integer> E<W>", "W")), 0);
        assert_eq!(errs(&with("<M: integer> E<M>", "M")), 0);
        // Using a parameter without binding it, and leaving them implicit.
        assert!(errs(&with("E<W: integer>", "W")) >= 1);
        assert!(errs(&with("E", "W")) >= 1);
        // Arity has to agree with the declaration.
        assert!(errs(&with("<W: integer> E<W, W>", "W")) >= 1);
        // A non-generic entity is untouched.
        assert_eq!(
            errs("module m;\nentity F { y: Bit out }\nimpl F { y = '0'; }\n"),
            0
        );
    }

    #[test]
    fn declaration_params_merge_uses_from_impls() {
        let sink = diagnostics(
            "module m;\n\
             entity E<T, U> { value: T out }\n\
             impl E<T, U> { let cached: T; }\n\
             struct Pair<A, B> { first: A }\n\
             trait Convert<X, Y> { fn apply(self, value: X) -> X; }\n",
        );
        let unused: Vec<&str> = sink
            .diagnostics()
            .iter()
            .filter(|d| d.code == Some(codes::UNUSED_PARAM))
            .map(|d| d.message.as_str())
            .collect();
        assert_eq!(
            unused.len(),
            3,
            "U, Pair::B, and Convert::Y are unused: {unused:?}"
        );
        assert_eq!(unused.iter().filter(|m| m.contains("`U`")).count(), 1);
        assert_eq!(unused.iter().filter(|m| m.contains("`B`")).count(), 1);
        assert_eq!(unused.iter().filter(|m| m.contains("`Y`")).count(), 1);
    }

    #[test]
    fn operator_traits_resolve_and_reject_unknown_operators() {
        // The operator trait and its impl resolve cleanly.
        let (_, errs) = resolve_src(
            "module m;\nstruct V { a: Bit }\nimpl Operator<\"+\", V, V> for V {\n  fn apply(self, rhs: V) -> V {\n    return self;\n  }\n}\n",
        );
        assert_eq!(errs, 0);

        // Quoted operator traits were removed with the Rust-style pivot.
        let sink = diagnostics("module m;\npub trait \"+\" {\n  fn apply(self) -> Self;\n}\n");
        assert!(
            sink.diagnostics()
                .iter()
                .any(|d| d.message.contains("quoted operator traits")),
            "expected the removal error"
        );
    }

    /// A type defined in terms of itself made every later stage recurse until
    /// the stack overflowed — the compiler aborted with a core dump on
    /// perfectly ordinary bad input.
    #[test]
    fn declaration_cycles_are_reported_not_fatal() {
        let (_, errs) = resolve_src("module m;\nusing A = B;\nusing B = A;\n");
        assert!(errs >= 1, "alias cycle");

        let (_, errs) = resolve_src("module m;\nstruct A(B);\nstruct B(A);\n");
        assert!(errs >= 1, "derivation cycle");

        // A self-reference is the one-step case.
        let (_, errs) = resolve_src("module m;\nusing A = A;\n");
        assert!(errs >= 1, "self-alias");

        // An enum derives its variants from its base the way a struct derives
        // its fields, and this arm was missing: the enum spelling of the same
        // cycle reported nothing while the struct spelling was caught.
        let (_, errs) = resolve_src("module m;\nenum A(B);\nenum B(A);\n");
        assert!(errs >= 1, "enum derivation cycle");

        let (_, errs) = resolve_src("module m;\nenum E(E);\n");
        assert!(errs >= 1, "self-deriving enum");

        // A legitimate derivation chain stays legal — std derives `Logic`
        // from `ULogic` exactly this way.
        let (_, errs) = resolve_src("module m;\nenum P { A, B }\nenum Q(P);\nenum R(Q);\n");
        assert_eq!(errs, 0, "a derivation chain is not a cycle");

        // Legitimate chains are untouched.
        let (_, errs) = resolve_src("module m;\nstruct A { x: Bit }\nstruct B(A);\nusing C = B;\n");
        assert_eq!(errs, 0);
    }

    /// An alias target is a type reference and must resolve.
    #[test]
    fn unknown_alias_target_is_reported() {
        let (_, errs) = resolve_src("module m;\nusing Word = NoSuchType;\n");
        assert_eq!(errs, 1);
        let (_, errs) = resolve_src("module m;\nusing Word = Bit;\n");
        assert_eq!(errs, 0);
    }

    /// A derivation base is a type reference like any other, but it was the
    /// one spot resolution skipped — `struct B(NoSuchType)` passed silently
    /// while the same name in a port was rejected.
    #[test]
    fn unknown_derivation_base_is_reported() {
        let (_, errs) = resolve_src("module m;\nstruct B(NoSuchType);\n");
        assert_eq!(errs, 1);
        let (_, errs) = resolve_src("module m;\nstruct A { x: Bit }\nstruct B(A);\n");
        assert_eq!(errs, 0);
    }

    /// A repeated member name inside one declaration never reached the
    /// top-level duplicate check: an enum got an ambiguous variant, and a
    /// struct got two identically-named leaf signals.
    #[test]
    fn duplicate_members_within_a_declaration_are_reported() {
        let (_, errs) = resolve_src("module m;\nenum S { A, B, A }\n");
        assert_eq!(errs, 1, "duplicate enum variant");

        let (_, errs) = resolve_src("module m;\nstruct P { x: Bit, x: Bit }\n");
        assert_eq!(errs, 1, "duplicate struct field");

        let (_, errs) = resolve_src("module m;\nentity E { a: Bit in, a: Bit in, y: Bit out, }\n");
        assert_eq!(errs, 1, "duplicate port");

        // Distinct members, and the same name in *different* declarations, are
        // both fine.
        let (_, errs) = resolve_src(
            "module m;\nenum S { A, B }\nenum T { A, B }\nstruct P { x: Bit, y: Bit }\n\
             entity E { a: Bit in, y: Bit out }\n",
        );
        assert_eq!(errs, 0);
    }

    #[test]
    fn unknown_type_suggests_a_close_name() {
        let sink = diagnostics("module m;\nstruct Packet { a: Bit }\nentity E { y: Packe out, }\n");
        let d = sink
            .diagnostics()
            .iter()
            .find(|d| d.code == Some(codes::UNKNOWN_NAME))
            .unwrap();
        assert_eq!(d.help.as_deref(), Some("did you mean `Packet`?"));
    }

    #[test]
    fn levenshtein_basics() {
        assert_eq!(levenshtein("Packe", "Packet"), 1);
        assert_eq!(levenshtein("signed", "singed"), 2);
        assert_eq!(levenshtein("abc", "abc"), 0);
    }

    #[test]
    fn duplicate_item_points_to_the_first() {
        let sink = diagnostics("module m;\nstruct P { a: Bit }\nstruct P { b: Bit }\n");
        let d = sink
            .diagnostics()
            .iter()
            .find(|d| d.code == Some(codes::DUPLICATE_ITEM))
            .unwrap();
        assert!(d.help.is_some());
        assert_eq!(d.labels.len(), 1); // "first declared here"
    }

    #[test]
    fn counter_resolves_clean() {
        let (_, errors) = resolve_src(
            "module m;\n\
             struct unsigned(Logic[]);\n\
             #[top]\n\
             entity Counter<W: integer> {\n\
               clk: Bit in,\n\
               rst: Logic in,\n\
               count: unsigned[W] out,\n\
             }\n\
             impl<W: integer> Counter<W> {\n\
               let value: unsigned[W] = 0;\n\
               if clk.rising() {\n\
                 value = value + 1;\n\
               }\n\
               count = value;\n\
             }\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn unknown_type_is_reported() {
        let (_, errors) = resolve_src("module m;\nentity E { y: Bogus out, }\n");
        assert_eq!(errors, 1);
    }

    #[test]
    fn duplicate_item_is_reported() {
        let (_, errors) = resolve_src("module m;\nstruct P { a: Bit }\nstruct P { b: Bit }\n");
        assert_eq!(errors, 1);
    }

    #[test]
    fn enum_variant_paths() {
        // Good variant resolves; bad variant errors.
        let (_, errors) = resolve_src(
            "module m;\nenum State { Idle, Run }\nentity M {}\nimpl M {\n  next = State::Idle;\n}\n",
        );
        assert_eq!(errors, 0);

        let (_, errors) = resolve_src(
            "module m;\nenum State { Idle, Run }\nentity M {}\nimpl M {\n  next = State::Bogus;\n}\n",
        );
        assert_eq!(errors, 1);
    }

    #[test]
    fn impl_on_undeclared_target_is_reported() {
        let (_, errors) = resolve_src("module m;\nimpl Nope {\n  x = 1;\n}\n");
        assert_eq!(errors, 1);
    }

    #[test]
    fn undeclared_attribute_is_reported_but_declared_is_ok() {
        let (_, errors) = resolve_src("module m;\n#[bogus]\nentity E { y: Bit out, }\n");
        assert_eq!(errors, 1);

        let (_, errors) = resolve_src(
            "module m;\nattr fast: Bool for entity;\n#[fast]\nentity E { y: Bit out, }\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn use_sites_are_recorded() {
        let (r, _) = resolve_src(
            "module m;\nenum State { Idle }\nentity M {}\nimpl M {\n  s = State::Idle;\n}\n",
        );
        // There is exactly one enum and one variant; the variant use-site maps
        // to a DefId whose kind is EnumVariant.
        let variant_uses = r
            .uses
            .values()
            .filter(|id| r.kind_of(**id) == Some(DefKind::EnumVariant))
            .count();
        assert_eq!(variant_uses, 1);
    }
}
