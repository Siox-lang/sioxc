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
//! - Lookup is module-aware: loaded modules do not leak declarations into one
//!   another, imports bind the exact named module, and qualified paths select
//!   that module. Free functions and module constants retain that identity
//!   through type checking, constant evaluation, and lowering;
//!   declaration categories whose later semantic tables are still leaf-keyed
//!   remain crate-unique for now. Several source files may belong to the same
//!   module and therefore share its private declarations.

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
    /// Compiler-provided type, operator hook, or attribute (`integer`,
    /// `Operator`, `top`, ...).
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
    /// Declaring module path. `None` only for compiler builtins.
    pub module: Option<String>,
    pub kind: DefKind,
    pub is_pub: bool,
    /// Declaration site, or `None` for builtins.
    pub span: Option<Span>,
    /// Owning definition, e.g. the enum a variant belongs to.
    pub parent: Option<DefId>,
}

#[derive(Clone)]
struct ImportSite {
    span: Span,
    id: DefId,
    accessible: bool,
}

/// The result of resolving a set of modules: the definition table plus a map
/// from every resolved name-use site (keyed by its span) to its [`DefId`].
#[derive(Default)]
pub struct Resolved {
    defs: Vec<DefInfo>,
    uses: HashMap<Span, DefId>,
    declarations: HashMap<Span, DefId>,
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

    /// The definition declared at an AST declaration span. Unlike
    /// [`Self::resolved`], this addresses the declaration itself rather than a
    /// reference to it, so downstream phases never need to rediscover an item
    /// by its leaf spelling.
    pub fn declared(&self, span: Span) -> Option<DefId> {
        self.declarations.get(&span).copied()
    }

    /// Stable namespaced spelling for a definition. Diagnostics generally use
    /// the shorter [`DefInfo::name`]; semantic registries use this key until
    /// they can store [`DefId`] directly.
    pub fn qualified_name(&self, id: DefId) -> Option<String> {
        let definition = self.def(id)?;
        Some(match &definition.module {
            Some(module) => format!("{module}::{}", definition.name),
            None => definition.name.clone(),
        })
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
    r.file_modules = modules
        .iter()
        .map(|m| {
            let path = m
                .path
                .segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join("::");
            (m.span.file, path)
        })
        .collect();
    for m in modules {
        r.set_current_module(m);
        for item in &m.items {
            r.collect_item(item);
        }
    }
    r.check_declaration_cycles(modules);
    r.inherit_enum_variants();
    // Resolve direct imports first, then public re-export chains. Collection
    // has already seen every declaration, so source order is irrelevant; the
    // bounded fixed point is only for `module facade; pub using base::{T}`.
    for _ in 0..=modules.len() {
        let mut progress = false;
        for m in modules {
            r.set_current_module(m);
            for item in &m.items {
                progress |= r.resolve_imports(item, false);
            }
        }
        if !progress {
            break;
        }
    }
    for m in modules {
        r.set_current_module(m);
        for item in &m.items {
            r.resolve_imports(item, true);
        }
    }
    for m in modules {
        r.set_current_module(m);
        for item in &m.items {
            r.resolve_item(item);
        }
    }
    r.check_public_interfaces(modules);
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

fn path_text(path: &Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join("::")
}

fn impl_member(item: &ImplItem) -> Option<(&String, Span, &'static str)> {
    Some(match item {
        ImplItem::Let(declaration) => (&declaration.name.text, declaration.name.span, "state"),
        ImplItem::Const(constant) => (&constant.name.text, constant.name.span, "constant"),
        ImplItem::Fn(function) => (&function.name.text, function.name.span, "method"),
        ImplItem::ModeField { name, .. } => (&name.text, name.span, "mode field"),
        ImplItem::Stmt(_) => return None,
    })
}

/// Semantic owner of an inherent impl. Applied views are overloaded by their
/// backing type, so both declarations participate in the identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ImplOwner {
    nominal: DefId,
    backing: Option<DefId>,
}

struct Resolver<'a> {
    sink: &'a mut DiagnosticSink,
    out: Resolved,
    /// Module-level + builtin type/value namespace.
    globals: HashMap<String, DefId>,
    /// Compiler-provided names remain available even when std declares the
    /// source-level trait/type with the same spelling.
    builtins: HashMap<String, DefId>,
    /// Exact `(module path, leaf)` ownership for imports and qualified paths.
    module_defs: HashMap<(String, String), DefId>,
    module_attrs: HashMap<(String, String), DefId>,
    /// Names explicitly imported into each source module. Re-exports retain
    /// their public flag here instead of mutating the target declaration.
    module_imports: HashMap<(String, String), (DefId, bool, Span)>,
    /// Attribute namespace (kept separate; attrs share no names with types).
    attrs: HashMap<String, DefId>,
    builtin_attrs: HashMap<String, DefId>,
    /// Enum `DefId` -> (variant name -> variant `DefId`).
    enum_variants: HashMap<DefId, HashMap<String, DefId>>,
    /// Enum name -> its `DefId`, and enum name -> base head name (derivation).
    enum_ids: HashMap<String, DefId>,
    enum_derives: HashMap<String, String>,
    /// Lexical scopes for params/locals, innermost last.
    scopes: Vec<HashMap<String, DefId>>,
    /// `using` import sites `(name span, imported DefId)`, for the unused-import
    /// lint after all references are resolved.
    import_sites: Vec<ImportSite>,
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
    /// Named members contributed by every inherent impl of one semantic owner.
    /// Split impl blocks share a scope, so duplicates must be rejected before
    /// type checking/lowering can silently overwrite a registry entry.
    inherent_members: HashMap<(ImplOwner, String), (Span, &'static str)>,
    /// The `module` path of every source that was actually loaded. A `using`
    /// naming a path absent from this set imports from a file the compiler
    /// never read, which is a different mistake from importing a name the
    /// module does not have — and used to be reported as the latter.
    loaded_modules: HashSet<String>,
    /// Source file -> declared module path. Privacy belongs to the module,
    /// never to the physical file that happened to contain a declaration.
    file_modules: HashMap<crate::diag::FileId, String>,
    current_module: Option<String>,
}

impl<'a> Resolver<'a> {
    fn new(sink: &'a mut DiagnosticSink) -> Self {
        Resolver {
            sink,
            out: Resolved::default(),
            globals: HashMap::new(),
            builtins: HashMap::new(),
            module_defs: HashMap::new(),
            module_attrs: HashMap::new(),
            module_imports: HashMap::new(),
            attrs: HashMap::new(),
            builtin_attrs: HashMap::new(),
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
            inherent_members: HashMap::new(),
            loaded_modules: HashSet::new(),
            file_modules: HashMap::new(),
            current_module: None,
        }
    }

    fn seed_builtins(&mut self) {
        // The numeric and character kernels are intrinsic. Digital scalar
        // enums and indexed families come from std declarations.
        for name in ["integer", "real", "Char", "string", "range"] {
            let id = self.add_def(name.to_string(), DefKind::Builtin, true, None, None);
            self.globals.insert(name.to_string(), id);
            self.builtins.insert(name.to_string(), id);
        }
        // Operator traits and the literal suffix/prefix hooks are compiler
        // mechanisms (spec 3.24/3.25): `impl Add for T` / `impl Suffix for T`
        // need no trait declaration or import.
        for name in OPERATORS.iter().copied().chain(["Suffix", "Prefix"]) {
            let id = self.add_def(name.to_string(), DefKind::Builtin, true, None, None);
            self.globals.insert(name.to_string(), id);
            self.builtins.insert(name.to_string(), id);
        }
        // std::attrs metadata attributes (spec 3.5).
        for name in ["top", "test", "keep", "library", "name", "precedence"] {
            let id = self.add_def(name.to_string(), DefKind::Builtin, true, None, None);
            self.attrs.insert(name.to_string(), id);
            self.builtin_attrs.insert(name.to_string(), id);
        }
    }

    fn set_current_module(&mut self, module: &Module) {
        self.current_module = Some(
            module
                .path
                .segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join("::"),
        );
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
                self.declare(&f.name.text, DefKind::Fn, f.is_pub, f.name.span);
            }
            Item::ExternBlock { fns, .. } => {
                for f in fns {
                    self.declare(&f.name.text, DefKind::Fn, f.is_pub, f.name.span);
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
                if let Some(module) = &self.current_module {
                    self.module_defs
                        .entry((module.clone(), v.name.text.clone()))
                        .or_insert(id);
                }
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
                self.register_attr(&a.name.text, id, a.name.span);
            }
            // Impls declare no top-level name.
            Item::Impl(_) => {}
        }
    }

    /// Unused-import lint (W-P005): a `using base::{name}` whose imported
    /// declaration is never referenced elsewhere in the same module. The
    /// import's own
    /// name span is excluded so the binding doesn't count as a use of itself.
    /// Reject a `using` that imports a non-`pub` item from another module.
    fn lint_private_imports(
        &mut self,
        _std_files: &std::collections::HashSet<crate::diag::FileId>,
    ) {
        let sites = self.import_sites.clone();
        for site in sites {
            let imp_span = site.span;
            let id = site.id;
            let bad = self.out.def(id).map(|d| (d.name.clone(), !site.accessible));
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
        for site in sites {
            let imp_span = site.span;
            let id = site.id;
            if std_files.contains(&imp_span.file) {
                continue;
            }
            let used = self
                .out
                .uses
                .iter()
                .any(|(s, d)| *d == id && self.same_module(*s, imp_span) && *s != imp_span);
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
    /// module provides. Runs after all modules are collected; an import that
    /// matches nothing is a hard error.
    fn resolve_imports(&mut self, item: &Item, report: bool) -> bool {
        let Item::Using(u) = item else { return false };
        let UsingKind::Import { base, names } = &u.kind else {
            return false;
        };
        let base_str = base
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("::");
        let importing_module = self.current_module.clone().unwrap_or_default();
        let mut progress = false;
        for n in names {
            let found = self
                .module_defs
                .get(&(base_str.clone(), n.text.clone()))
                .copied()
                .map(|id| {
                    let public = self.out.def(id).is_some_and(|definition| definition.is_pub);
                    (id, public || (base_str == importing_module && !u.is_pub))
                })
                .or_else(|| {
                    self.module_attrs
                        .get(&(base_str.clone(), n.text.clone()))
                        .copied()
                        .map(|id| {
                            let public =
                                self.out.def(id).is_some_and(|definition| definition.is_pub);
                            (id, public || (base_str == importing_module && !u.is_pub))
                        })
                })
                .or_else(|| {
                    self.module_imports
                        .get(&(base_str.clone(), n.text.clone()))
                        .map(|(id, is_pub, _)| {
                            (*id, *is_pub || (base_str == importing_module && !u.is_pub))
                        })
                });
            match found {
                Some((id, accessible)) => {
                    let key = (importing_module.clone(), n.text.clone());
                    if let Some(existing) = self.module_defs.get(&key).copied() {
                        if report {
                            self.report_import_collision(&n.text, n.span, existing);
                        }
                        continue;
                    }
                    match self.module_imports.entry(key) {
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert((id, u.is_pub, n.span));
                            self.import_sites.push(ImportSite {
                                span: n.span,
                                id,
                                accessible,
                            });
                            self.out.uses.insert(n.span, id);
                            progress = true;
                        }
                        std::collections::hash_map::Entry::Occupied(mut entry) => {
                            if entry.get().0 == id {
                                // Several physical files may contribute to one
                                // declared module. Repeating the same binding
                                // is harmless; any public occurrence makes it
                                // a module re-export, independent of file order.
                                entry.get_mut().1 |= u.is_pub;
                            } else if report && entry.get().2 != n.span {
                                let existing = entry.get().0;
                                self.report_import_collision(&n.text, n.span, existing);
                            }
                        }
                    }
                }
                None if report => {
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
                None => {}
            }
        }
        progress
    }

    fn report_import_collision(&mut self, name: &str, span: Span, existing: DefId) {
        let mut diagnostic = Diagnostic::error(format!(
            "imported name `{name}` conflicts with an existing name in this module"
        ))
        .with_code(codes::DUPLICATE_ITEM)
        .at(span)
        .help("remove one import or rename the local declaration");
        if let Some(previous) = self
            .out
            .def(existing)
            .and_then(|definition| definition.span)
        {
            diagnostic = diagnostic.label(previous, format!("`{name}` first introduced here"));
        }
        self.sink.emit(diagnostic);
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
        self.register_global(name, id, span);
        id
    }

    fn register_global(&mut self, name: &str, id: DefId, span: Span) {
        let current_module = self.current_module.as_deref();
        if let Some(module) = &self.current_module {
            self.module_defs
                .entry((module.clone(), name.to_string()))
                .or_insert(id);
        }
        if let Some(prev) = self.globals.get(name).copied() {
            let separate_namespaced_values = matches!(
                (self.out.kind_of(prev), self.out.kind_of(id)),
                (Some(DefKind::Fn), Some(DefKind::Fn))
                    | (Some(DefKind::Const), Some(DefKind::Const))
            ) && self
                .out
                .def(prev)
                .and_then(|definition| definition.module.as_deref())
                != current_module;
            if self.out.kind_of(prev) != Some(DefKind::Builtin) && !separate_namespaced_values {
                let mut diag = Diagnostic::error(format!("duplicate item `{name}`"))
                    .with_code(codes::DUPLICATE_ITEM)
                    .at(span)
                    .help("rename or remove one of them");
                if let Some(prev_span) = self.out.def(prev).and_then(|d| d.span) {
                    diag = diag.label(prev_span, format!("`{name}` first declared here"));
                }
                self.sink.emit(diag);
                return; // keep the first declaration as the resolution target
            }
        }
        self.globals.entry(name.to_string()).or_insert(id);
    }

    fn register_attr(&mut self, name: &str, id: DefId, span: Span) {
        if let Some(previous) = self.attrs.get(name).copied() {
            if self.out.kind_of(previous) != Some(DefKind::Builtin) {
                let mut diagnostic = Diagnostic::error(format!("duplicate attribute `{name}`"))
                    .with_code(codes::DUPLICATE_ITEM)
                    .at(span)
                    .help("rename or remove one of them");
                if let Some(previous_span) = self
                    .out
                    .def(previous)
                    .and_then(|definition| definition.span)
                {
                    diagnostic =
                        diagnostic.label(previous_span, format!("`{name}` first declared here"));
                }
                self.sink.emit(diagnostic);
                return;
            }
        }
        self.attrs.insert(name.to_string(), id);
        if let Some(module) = &self.current_module {
            self.module_attrs
                .insert((module.clone(), name.to_string()), id);
        }
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
        if let Some(span) = span {
            self.out.declarations.insert(span, id);
        }
        self.out.defs.push(DefInfo {
            name,
            module: self.current_module.clone(),
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
            // A free function uses the same resolver as a method. The old
            // special case resolved only its signature and skipped the body,
            // so unknown names and unresolved local types inside every
            // module-level function passed Stage 3 silently.
            Item::Fn(f) => self.resolve_fn(f),
            // Foreign declarations still have ordinary SIOX types in their
            // signatures. Resolve those names so ABI validation and call-site
            // checking see nominal/aliased types instead of `Ty::Error`.
            Item::ExternBlock { fns, .. } => {
                for f in fns {
                    self.resolve_fn(f);
                }
            }
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
        if im.trait_.is_none() {
            self.check_inherent_impl_coherence(im);
        } else {
            // Different traits may deliberately use the same method name, but
            // one trait impl block cannot declare one member twice.
            let owner = im
                .trait_
                .as_ref()
                .map(path_text)
                .unwrap_or_else(|| "trait impl".to_string());
            self.check_duplicate_names(
                im.items
                    .iter()
                    .filter_map(impl_member)
                    .map(|(name, span, _)| (name, span)),
                "implementation member",
                &owner,
            );
        }
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
                GenericArg::PositionalType(ty) | GenericArg::NamedType { ty, .. } => {
                    self.resolve_type(ty);
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

    fn check_inherent_impl_coherence(&mut self, im: &ImplDecl) {
        let Some(owner) = self.impl_owner(&im.target) else {
            // Unknown targets already receive the ordinary resolution error.
            return;
        };
        let Some(definition) = self.out.def(owner.nominal).cloned() else {
            return;
        };
        let display = self
            .impl_owner_display(owner)
            .unwrap_or_else(|| definition.name.clone());
        if !matches!(
            definition.kind,
            DefKind::Struct | DefKind::View | DefKind::Enum | DefKind::Entity
        ) {
            self.sink.emit(
                Diagnostic::error(format!(
                    "cannot define an inherent impl for non-owned type `{display}`"
                ))
                .with_code(codes::IMPL_COHERENCE)
                .at(im.span)
                .help("define a nominal wrapper type, or implement a trait instead"),
            );
            return;
        }
        let defining_module = definition.module.as_deref();
        if defining_module != self.current_module.as_deref() {
            let module = defining_module.unwrap_or("<compiler>");
            let mut diagnostic = Diagnostic::error(format!(
                "inherent impl for `{display}` must be declared in its defining module `{module}`"
            ))
            .with_code(codes::IMPL_COHERENCE)
            .at(im.span)
            .help("move this impl beside the type declaration, or express the extension as a trait impl");
            if let Some(span) = definition.span {
                diagnostic = diagnostic.label(span, "type defined here");
            }
            self.sink.emit(diagnostic);
            return;
        }

        for (name, span, kind) in im.items.iter().filter_map(impl_member) {
            let key = (owner, name.clone());
            if let Some(&(previous, previous_kind)) = self.inherent_members.get(&key) {
                self.sink.emit(
                    Diagnostic::error(format!(
                        "duplicate inherent member `{name}` for `{display}`"
                    ))
                    .with_code(codes::DUPLICATE_ITEM)
                    .at(span)
                    .label(previous, format!("first declared here as {previous_kind}"))
                    .help("rename or remove one of the members"),
                );
            } else {
                self.inherent_members.insert(key, (span, kind));
            }
        }
    }

    fn impl_owner(&self, ty: &Type) -> Option<ImplOwner> {
        match ty {
            Type::Path(path) => Some(ImplOwner {
                nominal: self.out.resolved(path.span)?,
                backing: None,
            }),
            Type::Generic { base, .. } | Type::Indexed { base, .. } => self.impl_owner(base),
            Type::View { view, target, .. } => Some(ImplOwner {
                nominal: self.out.resolved(view.span)?,
                backing: Some(self.type_definition(target)?),
            }),
        }
    }

    fn type_definition(&self, ty: &Type) -> Option<DefId> {
        match ty {
            Type::Path(path) => self.out.resolved(path.span),
            Type::Generic { base, .. } | Type::Indexed { base, .. } => self.type_definition(base),
            Type::View { view, .. } => self.out.resolved(view.span),
        }
    }

    fn impl_owner_display(&self, owner: ImplOwner) -> Option<String> {
        let nominal = self.out.qualified_name(owner.nominal)?;
        match owner.backing {
            Some(backing) => Some(format!("{} {}", self.out.qualified_name(backing)?, nominal)),
            None => Some(nominal),
        }
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
        // Function parameters scope over both the signature and body. This is
        // needed here (rather than only in the module-level Item arm) for
        // generic methods and trait defaults as well.
        self.bind_params(&f.generics, true, None);
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
            if let Some(id) = self.lookup_attr(last) {
                self.out.uses.insert(a.name.span, id);
            } else {
                self.error(
                    codes::UNKNOWN_NAME,
                    a.name.span,
                    format!("unknown attribute `{last}` (declare it with `attr` before use)"),
                );
            }
        } else {
            let module = segs[..segs.len() - 1]
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join("::");
            if let Some(id) = self.lookup_module_export(&module, last, true) {
                self.record_qualified_use(a.name.span, id);
            } else {
                self.error(
                    codes::UNKNOWN_NAME,
                    a.name.span,
                    format!("unknown attribute `{}`", path_text(&a.name)),
                );
            }
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
                        GenericArg::PositionalType(ty) | GenericArg::NamedType { ty, .. } => {
                            self.resolve_type(ty)
                        }
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
                self.record_qualified_use(p.span, id);
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
            let module = p.segments[..p.segments.len() - 1]
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join("::");
            if let Some(id) = self.lookup_module_export(&module, &last, false) {
                self.record_qualified_use(p.span, id);
            } else {
                self.error(
                    codes::UNKNOWN_NAME,
                    p.span,
                    format!("unknown type `{}`", path_text(p)),
                );
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
            Expr::Call {
                callee,
                type_args,
                args,
                ..
            } => {
                self.resolve_expr(callee);
                for ty in type_args {
                    self.resolve_type(ty);
                }
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
                        self.record_qualified_use(p.segments[0].span, id);
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
                                self.record_qualified_use(p.segments[0].span, id);
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
                self.record_qualified_use(p.segments[0].span, id);
                return;
            }
            // A module-qualified declaration (`pkg::VALUE`). Modules are not
            // definitions themselves yet, so resolve the exported leaf.
            if let Some(last) = p.segments.last() {
                let module = p.segments[..p.segments.len() - 1]
                    .iter()
                    .map(|segment| segment.text.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                if let Some(id) = self.lookup_module_export(&module, &last.text, false) {
                    self.record_qualified_use(p.span, id);
                }
            }
        } else if let Some(name) = p.segments.first() {
            if let Some(id) = self.lookup(&name.text) {
                self.record_qualified_use(p.span, id);
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
            return;
        }
        // Each argument must BE one of the bound names. siox has no partial
        // implementations, so a concrete argument (`impl<W: integer>
        // Counter<8>`) says nothing the declaration does not — and it was
        // accepted, leaving `W` to mean the instance's value while the target
        // claimed 8.
        let bound: HashSet<&str> = im
            .params
            .params
            .iter()
            .map(|p| p.name.text.as_str())
            .collect();
        let Type::Generic { args, .. } = &im.target else {
            return;
        };
        for arg in args {
            let named = match arg {
                GenericArg::Positional(Expr::Path(path)) => match path.segments.as_slice() {
                    [seg] => bound.contains(seg.text.as_str()),
                    _ => false,
                },
                _ => false,
            };
            if !named {
                self.sink.emit(
                    Diagnostic::error(format!(
                        "`{owner}` must be applied to the parameters this `impl` binds"
                    ))
                    .with_code(codes::TYPE_MISMATCH)
                    .at(span)
                    .help(
                        "siox has no per-value implementations: write \
                         `impl<W: ..> Owner<W>`, not a concrete argument",
                    ),
                );
                return;
            }
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
        // A private import is diagnosed at the import itself. Keep resolving
        // uses through that binding so one mistake does not cascade onto every
        // later reference in the importing module.
        if self
            .import_sites
            .iter()
            .any(|site| site.id == id && self.same_module(site.span, use_span))
        {
            self.out.uses.insert(use_span, id);
            return;
        }
        let private = self.out.def(id).and_then(|d| {
            d.span
                .filter(|decl_span| !d.is_pub && !self.same_module(*decl_span, use_span))
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

    fn check_public_interfaces(&mut self, modules: &[Module]) {
        let structs: HashMap<DefId, &StructDecl> = modules
            .iter()
            .flat_map(|module| module.items.iter())
            .filter_map(|item| match item {
                Item::Struct(structure) => self
                    .out
                    .declared(structure.name.span)
                    .map(|id| (id, structure)),
                _ => None,
            })
            .collect();
        let aliases: HashMap<DefId, &Type> = modules
            .iter()
            .flat_map(|module| module.items.iter())
            .filter_map(|item| match item {
                Item::Using(Using {
                    kind: UsingKind::Alias { name, ty },
                    ..
                }) => self.out.declared(name.span).map(|id| (id, ty)),
                _ => None,
            })
            .collect();
        for module in modules {
            for item in &module.items {
                match item {
                    Item::Const(c) if c.is_pub => self.check_public_type(&c.ty, "public constant"),
                    Item::Fn(f) if f.is_pub => self.check_public_fn(f, "public function"),
                    Item::ExternBlock { fns, .. } => {
                        for f in fns.iter().filter(|f| f.is_pub) {
                            self.check_public_fn(f, "public extern function");
                        }
                    }
                    Item::Struct(s) if s.is_pub => {
                        self.check_public_params(&s.params, "public struct");
                        if let Some(base) = &s.base {
                            self.check_public_type(base, "public struct representation");
                        }
                        for field in s.fields.iter().filter(|field| field.is_pub) {
                            self.check_public_type(&field.ty, "public struct field");
                        }
                    }
                    Item::View(v) if v.is_pub => {
                        self.check_public_params(&v.params, "public view");
                        self.check_public_type(&v.target, "public view backing type");
                        for field in &v.fields {
                            if let Some(ty) = self.view_field_type(
                                &v.target,
                                &field.name.text,
                                &structs,
                                &aliases,
                            ) {
                                // A view deliberately exposes even a private
                                // backing field. Its type is consequently part
                                // of the exported structural interface and must
                                // be nameable independently of the field's raw
                                // representation visibility.
                                self.check_public_type(&ty, "public view field");
                            }
                        }
                    }
                    Item::Enum(e) if e.is_pub => {
                        if let Some(repr) = &e.repr {
                            self.check_public_type(repr, "public enum representation");
                        }
                    }
                    Item::Entity(e) if e.is_pub => {
                        self.check_public_params(&e.params, "public entity");
                        for port in &e.ports {
                            self.check_public_type(&port.ty, "public entity port");
                        }
                    }
                    Item::Trait(t) if t.is_pub => {
                        self.check_public_params(&t.params, "public trait");
                        for f in &t.items {
                            self.check_public_fn(f, "public trait method");
                        }
                    }
                    Item::AttrDecl(a) if a.is_pub => {
                        self.check_public_type(&a.ty, "public attribute")
                    }
                    Item::Using(u) if u.is_pub => {
                        if let UsingKind::Alias { ty, .. } = &u.kind {
                            self.check_public_type(ty, "public type alias");
                        }
                    }
                    Item::Impl(im)
                        if im.trait_.is_none() && self.public_impl_target(&im.target) =>
                    {
                        for f in im.items.iter().filter_map(|item| match item {
                            ImplItem::Fn(f) if f.is_pub => Some(f),
                            _ => None,
                        }) {
                            self.check_public_fn(f, "public inherent method");
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn view_field_type(
        &self,
        target: &Type,
        field: &str,
        structs: &HashMap<DefId, &StructDecl>,
        aliases: &HashMap<DefId, &Type>,
    ) -> Option<Type> {
        let mut current = self.struct_type_id(target, structs, aliases, &mut HashSet::new())?;
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current) {
                return None;
            }
            let structure = structs.get(&current)?;
            if let Some(found) = structure.fields.iter().find(|item| item.name.text == field) {
                return Some(found.ty.clone());
            }
            current = self.struct_type_id(
                structure.base.as_ref()?,
                structs,
                aliases,
                &mut HashSet::new(),
            )?;
        }
    }

    fn struct_type_id(
        &self,
        ty: &Type,
        structs: &HashMap<DefId, &StructDecl>,
        aliases: &HashMap<DefId, &Type>,
        seen_aliases: &mut HashSet<DefId>,
    ) -> Option<DefId> {
        let id = match ty {
            Type::Path(path) => self.out.resolved(path.span)?,
            Type::Generic { base, .. } | Type::Indexed { base, .. } => {
                return self.struct_type_id(base, structs, aliases, seen_aliases);
            }
            Type::View { target, .. } => {
                return self.struct_type_id(target, structs, aliases, seen_aliases);
            }
        };
        if structs.contains_key(&id) {
            return Some(id);
        }
        let alias = aliases.get(&id)?;
        seen_aliases
            .insert(id)
            .then(|| self.struct_type_id(alias, structs, aliases, seen_aliases))?
    }

    /// An inherent method is externally reachable only when its owning type
    /// is externally nameable. `pub fn` on a private struct is still useful as
    /// an API within the module, but it cannot leak a signature outside that
    /// module because callers cannot name the receiver type there.
    fn public_impl_target(&self, ty: &Type) -> bool {
        let path = match ty {
            Type::Path(path) => path,
            Type::Generic { base, .. } | Type::Indexed { base, .. } => {
                return self.public_impl_target(base);
            }
            Type::View { view, .. } => view,
        };
        self.out.resolved(path.span).is_some_and(|id| {
            self.out.def(id).is_some_and(|definition| {
                definition.is_pub
                    || matches!(
                        definition.kind,
                        DefKind::Builtin | DefKind::Param | DefKind::Local
                    )
            })
        })
    }

    fn check_public_fn(&mut self, function: &FnDecl, context: &str) {
        self.check_public_params(&function.generics, context);
        for parameter in &function.params {
            if let Some(ty) = &parameter.ty {
                self.check_public_type(ty, context);
            }
        }
        if let Some(ret) = &function.ret {
            self.check_public_type(ret, context);
        }
    }

    fn check_public_params(&mut self, params: &Params, context: &str) {
        for param in &params.params {
            if let Some(bound) = &param.bound {
                self.check_public_type(bound, context);
            }
        }
    }

    fn check_public_type(&mut self, ty: &Type, context: &str) {
        match ty {
            Type::Path(path) => self.check_public_path(path, context),
            Type::Indexed { base, .. } => self.check_public_type(base, context),
            Type::Generic { base, args, .. } => {
                self.check_public_type(base, context);
                for arg in args {
                    match arg {
                        GenericArg::PositionalType(ty) | GenericArg::NamedType { ty, .. } => {
                            self.check_public_type(ty, context)
                        }
                        GenericArg::Positional(_) | GenericArg::Named { .. } => {}
                    }
                }
            }
            Type::View { view, target, .. } => {
                self.check_public_path(view, context);
                self.check_public_type(target, context);
            }
        }
    }

    fn check_public_path(&mut self, path: &Path, context: &str) {
        let Some(id) = self.out.resolved(path.span) else {
            return;
        };
        let Some(definition) = self.out.def(id).cloned() else {
            return;
        };
        if definition.is_pub
            || matches!(
                definition.kind,
                DefKind::Builtin | DefKind::Param | DefKind::Local
            )
        {
            return;
        }
        let mut diagnostic = Diagnostic::error(format!(
            "{context} exposes private type `{}`",
            definition.name
        ))
        .with_code(codes::PRIVATE_INTERFACE)
        .at(path.span)
        .help("make the referenced type public, or keep this declaration private");
        if let Some(span) = definition.span {
            diagnostic = diagnostic.label(span, "private type declared here");
        }
        self.sink.emit(diagnostic);
    }

    fn same_module(&self, a: Span, b: Span) -> bool {
        match (
            self.file_modules.get(&a.file),
            self.file_modules.get(&b.file),
        ) {
            (Some(a), Some(b)) => a == b,
            _ => a.file == b.file,
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
        if let Some(module) = &self.current_module {
            if let Some(id) = self
                .module_defs
                .get(&(module.clone(), name.to_string()))
                .copied()
            {
                return Some(id);
            }
            if let Some((id, _, _)) = self.module_imports.get(&(module.clone(), name.to_string())) {
                return Some(*id);
            }
            if module != "std::prelude" {
                if let Some((id, true, _)) = self
                    .module_imports
                    .get(&("std::prelude".to_string(), name.to_string()))
                {
                    return Some(*id);
                }
            }
        }
        self.builtins.get(name).copied()
    }

    fn lookup_attr(&self, name: &str) -> Option<DefId> {
        if let Some(module) = &self.current_module {
            if let Some(id) = self.module_attrs.get(&(module.clone(), name.to_string())) {
                return Some(*id);
            }
            if let Some((id, _, _)) = self.module_imports.get(&(module.clone(), name.to_string())) {
                if self.out.kind_of(*id) == Some(DefKind::Attr) {
                    return Some(*id);
                }
            }
            if let Some((id, true, _)) = self
                .module_imports
                .get(&("std::prelude".to_string(), name.to_string()))
            {
                if self.out.kind_of(*id) == Some(DefKind::Attr) {
                    return Some(*id);
                }
            }
        }
        self.builtin_attrs.get(name).copied()
    }

    fn lookup_module_export(&self, module: &str, name: &str, attr: bool) -> Option<DefId> {
        let direct = if attr {
            self.module_attrs
                .get(&(module.to_string(), name.to_string()))
        } else {
            self.module_defs
                .get(&(module.to_string(), name.to_string()))
        };
        direct.copied().or_else(|| {
            self.module_imports
                .get(&(module.to_string(), name.to_string()))
                .and_then(|(id, is_pub, _)| is_pub.then_some(*id))
        })
    }

    /// The closest in-scope name to `name` (edit distance <= 2), for a
    /// "did you mean?" suggestion.
    fn suggest(&self, name: &str) -> Option<String> {
        let candidates = self
            .scopes
            .iter()
            .flat_map(|s| s.keys())
            .chain(self.current_module.iter().flat_map(|module| {
                self.module_defs
                    .keys()
                    .filter(move |(owner, _)| owner == module)
                    .map(|(_, name)| name)
                    .chain(
                        self.module_imports
                            .keys()
                            .filter(move |(owner, _)| owner == module)
                            .map(|(_, name)| name),
                    )
            }))
            .chain(self.builtins.keys());
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

    fn module_diagnostics(sources: &[(&str, FileId)]) -> DiagnosticSink {
        let mut sink = DiagnosticSink::new();
        let modules: Vec<Module> = sources
            .iter()
            .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
            .collect();
        resolve(&modules, &mut sink);
        sink
    }

    #[test]
    fn foreign_modules_cannot_add_inherent_impls() {
        let sink = module_diagnostics(&[
            (
                "module owner;\npub struct Device { pub value: integer }\n",
                FileId(0),
            ),
            (
                "module user;\nusing owner::{Device};\nimpl Device { pub fn read(self) -> integer { return self.value; } }\n",
                FileId(1),
            ),
        ]);
        let errors: Vec<_> = sink
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == Some(codes::IMPL_COHERENCE))
            .collect();
        assert_eq!(errors.len(), 1, "foreign impl must fail once: {errors:?}");
        assert!(errors[0].message.contains("defining module `owner`"));
        assert!(errors[0]
            .labels
            .iter()
            .any(|label| label.message == "type defined here"));
    }

    #[test]
    fn inherent_impl_ownership_is_the_module_not_the_source_file() {
        let sink = module_diagnostics(&[
            ("module owner;\npub struct Device(integer);\n", FileId(0)),
            (
                "module owner;\nimpl Device { pub fn read(self) -> integer { return 0; } }\n",
                FileId(1),
            ),
        ]);
        assert!(sink
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.code != Some(codes::IMPL_COHERENCE)));
    }

    #[test]
    fn compiler_types_and_aliases_do_not_gain_inherent_impls() {
        let sink = diagnostics(
            "module m;\nusing Count = integer;\n\
             impl integer { fn raw(self) -> integer { return self; } }\n\
             impl Count { fn alias(self) -> integer { return self; } }\n",
        );
        assert_eq!(
            sink.diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.code == Some(codes::IMPL_COHERENCE))
                .count(),
            2
        );
    }

    #[test]
    fn split_inherent_impls_share_one_member_namespace() {
        let sink = diagnostics(
            "module m;\nstruct Register(integer);\n\
             impl Register { fn read(self) -> integer { return 0; } }\n\
             impl Register { fn read(self) -> integer { return 1; } }\n",
        );
        let duplicates: Vec<_> = sink
            .diagnostics()
            .iter()
            .filter(|diagnostic| {
                diagnostic.code == Some(codes::DUPLICATE_ITEM)
                    && diagnostic.message.contains("inherent member `read`")
            })
            .collect();
        assert_eq!(duplicates.len(), 1, "duplicate method must fail once");
        assert!(duplicates[0]
            .labels
            .iter()
            .any(|label| label.message.contains("first declared here as method")));
    }

    #[test]
    fn different_inherent_member_kinds_cannot_shadow_across_blocks() {
        let sink = diagnostics(
            "module m;\nentity Device {}\n\
             impl Device { let state: integer = 0; }\n\
             impl Device { const state: integer = 1; }\n",
        );
        assert!(sink.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == Some(codes::DUPLICATE_ITEM)
                && diagnostic.message.contains("inherent member `state`")
        }));
    }

    #[test]
    fn applied_view_owners_include_the_backing_type() {
        let sink = diagnostics(
            "module m;\n\
             struct Stream { value: integer }\n\
             struct Queue { value: integer }\n\
             view Source for Stream { value out }\n\
             view Source for Queue { value out }\n\
             impl Stream Source { fn get(self) -> integer { return self.value; } }\n\
             impl Queue Source { fn get(self) -> integer { return self.value; } }\n",
        );
        assert!(sink.diagnostics().iter().all(|diagnostic| {
            diagnostic.code != Some(codes::IMPL_COHERENCE)
                && !(diagnostic.code == Some(codes::DUPLICATE_ITEM)
                    && diagnostic.message.contains("inherent member `get`"))
        }));
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
    fn private_items_are_shared_by_files_in_the_same_module() {
        let mut sink = DiagnosticSink::new();
        let declaration = crate::syntax::parse_module(
            FileId(0),
            "module a;\nstruct Secret { value: integer }\n",
            &mut sink,
        );
        let use_site = crate::syntax::parse_module(
            FileId(1),
            "module a;\nusing a::{Secret};\nfn keep(value: Secret) -> Secret { return value; }\n",
            &mut sink,
        );
        resolve(&[declaration, use_site], &mut sink);
        assert!(
            sink.diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.code != Some(codes::PRIVATE_IMPORT)),
            "a module's privacy boundary is not a physical source file: {:?}",
            sink.diagnostics()
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_public_signature_cannot_expose_a_private_type() {
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(
            FileId(0),
            "module a;\nstruct Secret { value: integer }\npub fn leak(value: Secret) -> Secret { return value; }\n",
            &mut sink,
        );
        resolve(&[module], &mut sink);
        let leaks = sink
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == Some(codes::PRIVATE_INTERFACE))
            .count();
        assert_eq!(leaks, 2, "both parameter and return type are unusable API");
    }

    #[test]
    fn a_public_view_cannot_project_a_private_field_type() {
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(
            FileId(0),
            "module bus;\n\
             struct Hidden(integer);\n\
             pub struct Lines { secret: Hidden }\n\
             pub view Source for Lines { secret out }\n",
            &mut sink,
        );
        resolve(&[module], &mut sink);
        let leaks: Vec<&Diagnostic> = sink
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == Some(codes::PRIVATE_INTERFACE))
            .collect();
        assert_eq!(
            leaks.len(),
            1,
            "the private field itself may be projected, but its type must be public: {:?}",
            leaks
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
        );
        assert!(leaks[0].message.contains("public view field"));
    }

    #[test]
    fn a_public_view_may_project_a_private_field_of_public_type() {
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(
            FileId(0),
            "module bus;\n\
             pub struct Payload(integer);\n\
             pub struct Lines { payload: Payload }\n\
             pub view Source for Lines { payload out }\n",
            &mut sink,
        );
        resolve(&[module], &mut sink);
        assert!(
            sink.diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.code != Some(codes::PRIVATE_INTERFACE)),
            "field privacy and projected type visibility are independent: {:?}",
            sink.diagnostics()
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_public_method_on_a_private_type_has_only_module_visibility() {
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(
            FileId(0),
            "module a;\nstruct Secret(integer);\nstruct Owner(integer);\nimpl Owner { pub fn keep(self, value: Secret) -> Secret { return value; } }\n",
            &mut sink,
        );
        resolve(&[module], &mut sink);
        assert!(
            sink.diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.code != Some(codes::PRIVATE_INTERFACE)),
            "a member cannot be more visible than its private owner: {:?}",
            sink.diagnostics()
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_import_only_binds_the_requested_modules_declaration() {
        let mut sink = DiagnosticSink::new();
        let a = crate::syntax::parse_module(
            FileId(0),
            "module a;\npub struct Actual { value: integer }\n",
            &mut sink,
        );
        let b = crate::syntax::parse_module(
            FileId(1),
            "module b;\npub struct Thing { value: integer }\n",
            &mut sink,
        );
        let user = crate::syntax::parse_module(
            FileId(2),
            "module user;\nusing a::{Thing};\nfn take(value: Thing) -> Thing { return value; }\n",
            &mut sink,
        );
        resolve(&[a, b, user], &mut sink);
        assert!(sink.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == Some(codes::UNRESOLVED_IMPORT)
                && diagnostic.message.contains("no `Thing` in `a`")
        }));
    }

    #[test]
    fn loaded_modules_do_not_leak_names_into_unqualified_scope() {
        let mut sink = DiagnosticSink::new();
        let library = crate::syntax::parse_module(
            FileId(0),
            "module library;\npub struct HiddenUnlessImported { value: integer }\n",
            &mut sink,
        );
        let user = crate::syntax::parse_module(
            FileId(1),
            "module user;\nfn take(value: HiddenUnlessImported) -> integer { return 0; }\n",
            &mut sink,
        );
        resolve(&[library, user], &mut sink);
        assert!(sink.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == Some(codes::UNKNOWN_NAME)
                && diagnostic.message.contains("HiddenUnlessImported")
        }));
    }

    #[test]
    fn qualified_paths_resolve_the_exact_module() {
        let mut sink = DiagnosticSink::new();
        let a = crate::syntax::parse_module(
            FileId(0),
            "module a;\npub struct Actual { value: integer }\n",
            &mut sink,
        );
        let b = crate::syntax::parse_module(
            FileId(1),
            "module b;\npub struct Thing { value: integer }\n",
            &mut sink,
        );
        let user = crate::syntax::parse_module(
            FileId(2),
            "module user;\nfn wrong(value: a::Thing) -> integer { return 0; }\nfn right(value: b::Thing) -> integer { return 1; }\n",
            &mut sink,
        );
        resolve(&[a, b, user], &mut sink);
        let wrong = sink
            .diagnostics()
            .iter()
            .filter(|diagnostic| {
                diagnostic.code == Some(codes::UNKNOWN_NAME)
                    && diagnostic.message.contains("a::Thing")
            })
            .count();
        assert_eq!(wrong, 1, "only the wrong qualified path is rejected");
    }

    #[test]
    fn equal_constant_leaves_keep_their_module_identity() {
        let mut sink = DiagnosticSink::new();
        let modules = [
            crate::syntax::parse_module(
                FileId(0),
                "module a; pub const VALUE: integer = 11;",
                &mut sink,
            ),
            crate::syntax::parse_module(
                FileId(1),
                "module b; pub const VALUE: integer = 22;",
                &mut sink,
            ),
            crate::syntax::parse_module(
                FileId(2),
                "module user; const SUM: integer = a::VALUE + b::VALUE;",
                &mut sink,
            ),
        ];
        let resolved = resolve(&modules, &mut sink);
        assert!(sink
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.code != Some(codes::DUPLICATE_ITEM)));
        let Item::Const(sum) = &modules[2].items[0] else {
            panic!("expected SUM constant");
        };
        let Expr::Binary { lhs, rhs, .. } = &sum.value else {
            panic!("expected binary initializer");
        };
        let ids = [lhs.as_ref(), rhs.as_ref()].map(|expression| {
            let Expr::Path(path) = expression else {
                panic!("expected constant path");
            };
            resolved.resolved(path.span).expect("resolved constant")
        });
        assert_ne!(ids[0], ids[1]);
        assert_eq!(resolved.qualified_name(ids[0]).as_deref(), Some("a::VALUE"));
        assert_eq!(resolved.qualified_name(ids[1]).as_deref(), Some("b::VALUE"));
    }

    #[test]
    fn public_imports_reexport_their_target() {
        let mut sink = DiagnosticSink::new();
        let base = crate::syntax::parse_module(
            FileId(0),
            "module base;\npub struct Thing { value: integer }\n",
            &mut sink,
        );
        let facade = crate::syntax::parse_module(
            FileId(1),
            "module facade;\npub using base::{Thing};\n",
            &mut sink,
        );
        let user = crate::syntax::parse_module(
            FileId(2),
            "module user;\nusing facade::{Thing};\nfn take(value: Thing) -> Thing { return value; }\n",
            &mut sink,
        );
        resolve(&[user, facade, base], &mut sink);
        assert!(
            sink.diagnostics().iter().all(|diagnostic| {
                diagnostic.code != Some(codes::UNRESOLVED_IMPORT)
                    && diagnostic.code != Some(codes::PRIVATE_IMPORT)
            }),
            "public re-export should resolve regardless of module order: {:?}",
            sink.diagnostics()
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_import_cannot_silently_shadow_a_local_declaration() {
        let mut sink = DiagnosticSink::new();
        let library = crate::syntax::parse_module(
            FileId(0),
            "module library;\npub struct Thing { value: integer }\n",
            &mut sink,
        );
        let user = crate::syntax::parse_module(
            FileId(1),
            "module user;\nstruct Thing { value: integer }\nusing library::{Thing};\n",
            &mut sink,
        );
        resolve(&[library, user], &mut sink);
        assert!(sink.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == Some(codes::DUPLICATE_ITEM)
                && diagnostic.message.contains("imported name `Thing`")
        }));
    }

    #[test]
    fn repeated_same_module_imports_of_one_target_are_not_ambiguous() {
        let mut sink = DiagnosticSink::new();
        let library = crate::syntax::parse_module(
            FileId(0),
            "module library;\npub struct Thing { value: integer }\n",
            &mut sink,
        );
        let first = crate::syntax::parse_module(
            FileId(1),
            "module user;\nusing library::{Thing};\nfn first(value: Thing) -> Thing { return value; }\n",
            &mut sink,
        );
        let second = crate::syntax::parse_module(
            FileId(2),
            "module user;\nusing library::{Thing};\nfn second(value: Thing) -> Thing { return value; }\n",
            &mut sink,
        );
        resolve(&[library, first, second], &mut sink);
        assert!(sink.diagnostics().iter().all(|diagnostic| {
            diagnostic.code != Some(codes::DUPLICATE_ITEM)
                && diagnostic.code != Some(codes::UNRESOLVED_IMPORT)
        }));
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

    #[test]
    fn free_function_bodies_are_resolved() {
        let (_, errs) = resolve_src(
            "module m;\nfn bad(value: Bit) -> Bit { let local: Missing = value; return value; }\n",
        );
        assert_eq!(errs, 1, "an unknown block-local type");

        let (_, errs) = resolve_src(
            "module m;\nfn id<T>(value: T) -> T { let local: T = value; return local; }\n",
        );
        assert_eq!(
            errs, 0,
            "function generics, parameters, and locals share the body scope"
        );
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
    fn a_qualified_attribute_uses_the_exact_module() {
        let mut sink = DiagnosticSink::new();
        let attrs = crate::syntax::parse_module(
            FileId(0),
            "module attrs;\npub attr known: integer for entity;\n",
            &mut sink,
        );
        let user = crate::syntax::parse_module(
            FileId(1),
            "module user;\n#[attrs::missing = 1]\nentity E {}\n",
            &mut sink,
        );
        resolve(&[attrs, user], &mut sink);
        assert!(sink.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == Some(codes::UNKNOWN_NAME)
                && diagnostic.message.contains("attrs::missing")
        }));
    }

    #[test]
    fn duplicate_attribute_declarations_are_reported() {
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(
            FileId(0),
            "module attrs;\nattr marker: integer for entity;\nattr marker: integer for struct;\n",
            &mut sink,
        );
        resolve(&[module], &mut sink);
        assert!(sink.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == Some(codes::DUPLICATE_ITEM)
                && diagnostic.message.contains("duplicate attribute `marker`")
        }));
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
