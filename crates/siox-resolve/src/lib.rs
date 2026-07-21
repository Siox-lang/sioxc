//! Name resolution and module system for siox Phase 1 (spec Stage 3).
//!
//! Resolves identifiers to declarations: top-level item names, `using`
//! imports/aliases, `::` path resolution, associated items (`State::Idle`),
//! impl/instance type names, and attribute names. Each declaration gets a
//! stable [`DefId`]; every resolved use-site is recorded by span so later
//! stages (types, elaboration) can look up what a name refers to.
//!
//! Acceptance (spec Stage 3):
//! - unknown names reported ([`siox_diag::codes::UNKNOWN_NAME`])
//! - duplicate items reported ([`siox_diag::codes::DUPLICATE_ITEM`])
//! - attribute usage fails if the attribute was not declared/imported
//! - associated paths like `State::Idle` resolve correctly
//!
//! Phase-1 scope notes (deliberate simplifications, to be tightened later):
//! - The kernel base types (`integer`, `real`) are seeded as builtins, plus —
//!   as a shim until operator overloading — the std type names the checker/IR
//!   still special-case (`Bit`, `uint`, ...) and the `std::attrs` attributes.
//! - Type references, enum-variant paths, and attribute names are resolved
//!   strictly (an unknown one is an error). Plain value identifiers (signals,
//!   ports, locals) are resolved best-effort and never produce a false
//!   "unknown name" — full value/port/field scoping lands with type checking.
//! - All modules share one global namespace; cross-module visibility is not
//!   yet enforced.

use std::collections::HashMap;

use siox_diag::{codes, Diagnostic, DiagnosticSink, Span};
use siox_syntax::ast::*;
use siox_syntax::Module;

/// The Rust-style operator traits (spec 3.25): `a + b` dispatches to `Add`,
/// `and` to `And`, unary `not` to `Not`, and one `Ord` (`cmp -> Ordering`)
/// impl derives all six comparisons. Seeded as builtins so `impl Add for T`
/// needs no import. Non-core infix operators dispatch through std's `custom`
/// trait and are keyed by the symbol in its first template argument.
pub const OPERATORS: &[&str] = &[
    "Add", "Sub", "Mul", "Div", "Shl", "Shr", "And", "Or", "Not", "Ord",
];

/// Stable id for a resolved declaration. Later stages key off this instead of
/// raw names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DefId(pub u32);

/// What kind of thing a [`DefId`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefKind {
    /// Primitive type or seeded attribute (`Bit`, `uint`, `top`, ...).
    Builtin,
    Struct,
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
    for m in modules {
        for item in &m.items {
            r.collect_item(item);
        }
    }
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
    let std_files: std::collections::HashSet<siox_diag::FileId> = modules
        .iter()
        .filter(|m| m.path.segments.first().map(|s| s.text.as_str()) == Some("std"))
        .map(|m| m.span.file)
        .collect();
    r.lint_unused_imports(&std_files);
    r.out
}

/// The head identifier of a type expression (`uint[8]` -> `uint`), for
/// derivation-base lookup.
fn type_head(t: &Type) -> Option<&str> {
    match t {
        Type::Path(p) => p.segments.first().map(|s| s.text.as_str()),
        Type::Generic { base, .. } | Type::Indexed { base, .. } => type_head(base),
        Type::Mode { inner, .. } => type_head(inner),
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
        }
    }

    fn seed_builtins(&mut self) {
        // The kernel's base types are `integer` and `real` (unconstrained,
        // VHDL-style); everything else is designed to live in `std/`:
        // Bit/Logic/Bool are enums in std/logic.siox, `Boolean` a trait
        // in std/ops.siox, and uint[N]/int[N] are derived Logic vectors that
        // accept `integer` on assignment. The rest of this list is the shim:
        // names the checker/IR still special-case until operator overloading
        // lets their semantics move to std as source.
        for name in [
            "integer", "real", "Char", "Bit", "Logic", "Bool", "string",
            "range",
        ]
        {
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
            let mut guard = 0;
            while let Some(base) = self.enum_derives.get(&cur).cloned() {
                let Some(&bid) = self.enum_ids.get(&base) else { break };
                if let Some(m) = self.enum_variants.get(&bid) {
                    inherited.push(m.clone());
                }
                cur = base;
                guard += 1;
                if guard > 64 {
                    break;
                }
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
                    self.declare(&name.text, DefKind::TypeAlias, false, name.span);
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
            }
            Item::Enum(e) => {
                let id = self.declare(&e.name.text, DefKind::Enum, e.is_pub, e.name.span);
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
            }
            Item::Trait(t) => {
                self.declare(&t.name.text, DefKind::Trait, t.is_pub, t.name.span);
            }
            Item::AttrDecl(a) => {
                let id =
                    self.add_def(a.name.text.clone(), DefKind::Attr, a.is_pub, Some(a.name.span), None);
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
    fn lint_unused_imports(&mut self, std_files: &std::collections::HashSet<siox_diag::FileId>) {
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

    /// Bind each `using base::{names}` name to the declaration another loaded
    /// module (or a builtin) provides. Runs after all modules are collected;
    /// an import that matches nothing is a hard error.
    fn resolve_imports(&mut self, item: &Item) {
        let Item::Using(u) = item else { return };
        let UsingKind::Import { base, names } = &u.kind else { return };
        for n in names {
            let found = self.globals.get(&n.text).or_else(|| self.attrs.get(&n.text)).copied();
            match found {
                Some(id) => {
                    self.out.uses.insert(n.span, id);
                    self.import_sites.push((n.span, id));
                }
                None => {
                    let base_str =
                        base.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("::");
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
        self.out.defs.push(DefInfo { name, kind, is_pub, span, parent });
        id
    }

    // --- resolution (uses) --------------------------------------------------

    fn resolve_item(&mut self, item: &Item) {
        match item {
            Item::Using(_) => {}
            Item::Fn(f) => {
                // A generic fn's type params (`<T: Ord>`) scope over its
                // signature, so `a: T` resolves.
                self.enter();
                self.bind_params(&f.generics);
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
                self.bind_params(&s.params);
                for f in &s.fields {
                    self.resolve_type(&f.ty);
                }
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
                self.bind_params(&e.params);
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
                self.bind_params(&t.params);
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
        self.bind_params(&im.params);
        // `impl Reg<T>` declares the type parameter `T` for the body (like
        // Rust's `impl<T> Reg<T>`): a bare single-name generic argument on the
        // target that isn't already a known type is a type parameter.
        if let Type::Generic { args, .. } = &im.target {
            for a in args {
                if let GenericArg::Positional(Expr::Path(p)) = a {
                    if p.segments.len() == 1 && self.lookup(&p.segments[0].text).is_none() {
                        let name = p.segments[0].text.clone();
                        let id = self.add_def(name.clone(), DefKind::Param, false, Some(p.segments[0].span), None);
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
        for it in &im.items {
            self.resolve_impl_item(it);
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
            Stmt::For { var, range, body, .. } => {
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
            // Qualified `std::attrs::top` — accept when the leaf is known.
            self.out.uses.insert(a.name.span, id);
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
            Type::Mode { inner, .. } => self.resolve_type(inner),
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
            // Qualified path: lenient while cross-module `std::*` is absent.
            let last = p.segments.last().unwrap().text.clone();
            if let Some(id) = self.globals.get(&last).copied() {
                self.out.uses.insert(p.span, id);
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
            | Expr::LogicLit { .. }
            | Expr::StrLit { .. }
            | Expr::Bool { .. } => {}
            Expr::Path(p) => self.resolve_value_path(p),
            Expr::IfExpr { cond, then, els, .. } => {
                self.resolve_expr(cond);
                self.resolve_expr(then);
                self.resolve_expr(els);
            }
            Expr::Match { scrutinee, arms, .. } => {
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
                    match self.variant(id, &var) {
                        Some(vid) => {
                            self.out.uses.insert(p.span, vid);
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
            }
        } else if let Some(name) = p.segments.first() {
            if let Some(id) = self.lookup(&name.text) {
                self.out.uses.insert(p.span, id);
            }
        }
    }

    // --- scopes & lookup ----------------------------------------------------

    fn enter(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn exit(&mut self) {
        self.scopes.pop();
    }

    fn bind_params(&mut self, params: &Params) {
        for p in &params.params {
            let id =
                self.add_def(p.name.text.clone(), DefKind::Param, false, Some(p.name.span), None);
            self.bind(&p.name.text, id);
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
        let candidates = self.scopes.iter().flat_map(|s| s.keys()).chain(self.globals.keys());
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
        self.enum_variants.get(&enum_id).and_then(|m| m.get(name)).copied()
    }

    fn error(&mut self, code: &'static str, span: Span, msg: String) {
        self.sink.emit(Diagnostic::error(msg).with_code(code).at(span));
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
    use siox_diag::FileId;

    fn resolve_src(src: &str) -> (Resolved, usize) {
        let mut sink = DiagnosticSink::new();
        let module = siox_syntax::parse_module(FileId(0), src, &mut sink);
        assert_eq!(sink.error_count(), 0, "source failed to parse:\n{src}");
        let resolved = resolve(std::slice::from_ref(&module), &mut sink);
        (resolved, sink.error_count())
    }

    /// Resolve and return the raw diagnostics, for inspecting help/labels.
    fn diagnostics(src: &str) -> DiagnosticSink {
        let mut sink = DiagnosticSink::new();
        let module = siox_syntax::parse_module(FileId(0), src, &mut sink);
        resolve(std::slice::from_ref(&module), &mut sink);
        sink
    }

    #[test]
    fn unused_import_lint() {
        // A provider module (`std::lib`) and a user module that imports two of
        // its names but only uses one. The unused one warns; the used one and
        // the std module's own items do not.
        let mut sink = DiagnosticSink::new();
        let provider = siox_syntax::parse_module(
            FileId(0),
            "module std::lib;\npub enum Used { A, B }\npub enum Dead { C }\n",
            &mut sink,
        );
        let user = siox_syntax::parse_module(
            FileId(1),
            "module m;\nusing std::lib::{Used, Dead};\nentity E { in a: Used; }\n",
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
        assert!(unused[0].contains("Dead"), "flags Dead, not Used: {unused:?}");
    }

    #[test]
    fn operator_traits_resolve_and_reject_unknown_operators() {
        // A core operator trait and its impl resolve cleanly.
        let (_, errs) = resolve_src(
            "module m;\nstruct V { a: Bit }\nimpl Add for V {\n  fn add(self, rhs: V) -> V {\n    return self;\n  }\n}\n",
        );
        assert_eq!(errs, 0);

        // Quoted operator traits were removed with the Rust-style pivot.
        let sink = diagnostics("module m;\npub trait \"+\" {\n  fn apply(self) -> Self;\n}\n");
        assert!(
            sink.diagnostics().iter().any(|d| d.message.contains("quoted operator traits")),
            "expected the removal error"
        );
    }

    #[test]
    fn unknown_type_suggests_a_close_name() {
        let sink = diagnostics("module m;\nstruct Packet { a: Bit }\nentity E { out y: Packe; }\n");
        let d = sink.diagnostics().iter().find(|d| d.code == Some(codes::UNKNOWN_NAME)).unwrap();
        assert_eq!(d.help.as_deref(), Some("did you mean `Packet`?"));
    }

    #[test]
    fn levenshtein_basics() {
        assert_eq!(levenshtein("Packe", "Packet"), 1);
        assert_eq!(levenshtein("uint", "unit"), 2);
        assert_eq!(levenshtein("abc", "abc"), 0);
    }

    #[test]
    fn duplicate_item_points_to_the_first() {
        let sink = diagnostics("module m;\nstruct P { a: Bit }\nstruct P { b: Bit }\n");
        let d = sink.diagnostics().iter().find(|d| d.code == Some(codes::DUPLICATE_ITEM)).unwrap();
        assert!(d.help.is_some());
        assert_eq!(d.labels.len(), 1); // "first declared here"
    }

    #[test]
    fn counter_resolves_clean() {
        let (_, errors) = resolve_src(
            "module m;\n\
             using std::logic::{Bit, Logic};\n\
             struct uint : Logic[];\n\
             #[top]\n\
             entity Counter<W: integer> {\n\
               in clk: Bit;\n\
               in rst: Logic;\n\
               out count: uint[W];\n\
             }\n\
             impl Counter<W: integer> {\n\
               let value: uint[W] = 0;\n\
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
        let (_, errors) = resolve_src("module m;\nentity E { out y: Bogus; }\n");
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
        let (_, errors) = resolve_src("module m;\n#[bogus]\nentity E { out y: Bit; }\n");
        assert_eq!(errors, 1);

        let (_, errors) = resolve_src(
            "module m;\nattr fast: Bool for entity;\n#[fast]\nentity E { out y: Bit; }\n",
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
