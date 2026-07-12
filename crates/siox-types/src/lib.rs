//! Type system and kind checking for siox Phase 1 (spec Stage 4).
//!
//! Checks primitive digital types (`Bit`, `Logic`, `Bool`), integer widths
//! (`uint[N]`, `int[N]`), structs, enums, arrays/vectors, entity types,
//! directional views and bus modes, function/method signatures, trait bounds,
//! attribute value typing, and pattern typing.
//!
//! Key Phase 1 rules to enforce:
//! - system attributes `::event`/`::old` exist on every digital value
//!   (spec 3.9), and range attributes `::width/::range/::high/::low/::left/
//!   ::right/::direction` on range-like values (spec 3.23)
//! - `::ddt` is rejected as Phase-2 analogue syntax (spec Stage 4)
//! - no implicit broad conversions (spec 3.17): `uint[8]` !-> `uint[16]`
//! - cannot write to `in` ports inside an entity (spec 3.18 / code E-P004)
//! - `Logic` is not a bare condition without comparison (spec 3.16)

use std::collections::{HashMap, HashSet};

use siox_diag::{codes, Diagnostic, DiagnosticSink, Span};
use siox_resolve::{DefKind, Resolved};
use siox_syntax::ast::*;
use siox_syntax::Module;

/// A checked, interned type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    Bit,
    Logic,
    Bool,
    /// The kernel base type `integer` — an unbounded mathematical number, NOT
    /// a bit collection. Coerces to/from bit vectors, supports arithmetic and
    /// comparison, but has NO per-bit boolean operators (unlike uint/int).
    Integer,
    /// The kernel base type `real` (f64 in simulation).
    Real,
    /// The kernel base type `Char`: a non-numeric character symbol.
    /// Equality is intrinsic; numbers only exist via std encoding tables.
    Char,
    /// A packed bit vector — every `#[vector]` family (uint/int and user
    /// types). Width `0` means "not yet known" (parametric `F[W]`) or the
    /// unbounded kernel `integer`. The compiler has NO notion of `uint`/`int`
    /// by name; those are std names for the unsigned/signed cases.
    Vector { width: u32, signed: bool },
    /// Named struct / enum / entity, keyed by its definition.
    Named(siox_resolve::DefId),
    /// `T[range]` array/vector of a digital element type.
    Array { elem: Box<Ty>, len: u32 },
    /// Placeholder for an as-yet-unresolved/error type.
    Error,
}

impl Ty {
    /// Whether `::event` / `::old` apply (spec 3.9). True for all digital and
    /// discrete values, structs of digital fields, arrays, and enums.
    pub fn is_digital(&self) -> bool {
        // TODO(stage-4): recurse into Named structs to confirm all-digital.
        !matches!(self, Ty::Error)
    }
}

/// Outcome of type checking: a type for every expression/signal, ready for the
/// elaborator and IR lowering.
#[derive(Default)]
pub struct Typed {
    // TODO(stage-4): expr -> Ty map, signal/port types, method resolution.
}

/// Type-check resolved modules.
///
/// Incremental Stage-4 checker. It builds a light type-inference core (resolve
/// type annotations to [`Ty`], a per-impl symbol table, and `type_of` for
/// expressions) and enforces the digital rules that do not need elaboration:
/// - **Phase-2 guard** (spec Stage 4): `::ddt` -> [`codes::PHASE2_SYNTAX`].
/// - **Write to input port** (spec 3.18): bare `in` port on an assignment LHS
///   -> [`codes::WRITE_TO_INPUT_PORT`].
/// - **`Logic` as a bare condition** (spec 3.16): a condition of type `Logic`
///   that is not an explicit comparison -> [`codes::TYPE_MISMATCH`].
/// - **Attribute target** (spec 3.5): an attribute applied to a target its
///   declaration does not allow -> [`codes::INVALID_ATTR_TARGET`].
///
/// Deferred to elaboration, where the needed information exists: width-level
/// conversions (`uint[8]` !-> `uint[16]`) and method-call resolution.
pub fn check(modules: &[Module], resolved: &Resolved, sink: &mut DiagnosticSink) -> Typed {
    let mut checker = Checker::new(sink, resolved);
    checker.collect(modules);
    for m in modules {
        for item in &m.items {
            checker.check_item(item);
        }
    }
    Typed::default()
}

/// Analogue (Phase-2) system attributes that must error rather than be silently
/// accepted in Phase 1 (spec Stage 4). The full analogue set is a Phase-2
/// concern; `::ddt` is kept here only as the guard the spec calls out.
const PHASE2_ATTRS: &[&str] = &["ddt"];

/// A port as seen by the checker: its name, resolved type, and direction.
struct PortInfo {
    name: String,
    ty: Ty,
    dir: Option<Direction>,
}

/// The value type an attribute declaration expects (spec 3.5).
#[derive(Clone, Copy, PartialEq, Eq)]
enum AttrValueTy {
    Bool,
    Str,
    Other,
}

struct Checker<'a> {
    sink: &'a mut DiagnosticSink,
    resolved: &'a Resolved,
    /// Entity name -> its ports.
    entities: HashMap<String, Vec<PortInfo>>,
    /// Attribute name -> the target keywords it may be applied to.
    attr_targets: HashMap<String, Vec<String>>,
    /// Attribute name -> the value type it expects.
    attr_value_kinds: HashMap<String, AttrValueTy>,
    /// Trait name -> set of type (head) names that implement it.
    trait_impls: HashMap<String, HashSet<String>>,
    /// Enum name -> its EFFECTIVE variant names (inherited + own).
    enum_variants: HashMap<String, Vec<String>>,
    /// Enum name -> only its own declared variants (pre-inheritance).
    own_variants: HashMap<String, Vec<String>>,
    /// Enum name -> the head name after `:` (a base enum or numeric repr).
    enum_bases: HashMap<String, String>,
    /// Struct name -> (derivation base, own field names) for inheritance.
    structs: HashMap<String, (Option<Type>, Vec<String>)>,
    /// Struct name -> signedness, for structs carrying `#[vector]` (an
    /// array-derived numeric family). Membership is the attribute, not shape.
    vector_families: HashMap<String, bool>,
    /// Generic module fns: name -> (type params with bounds, value params).
    /// Bounds are checked at each call (spec: generic bounds).
    generic_fns: HashMap<String, (Vec<Param>, Vec<(String, Type)>)>,
    /// Literal suffix -> the type names defining it via `impl Suffix for T`
    /// (more than one is an ambiguity error at the use site).
    suffix_types: HashMap<String, Vec<String>>,
    /// `using X = T;` aliases, resolved through when typing.
    aliases: HashMap<String, Type>,
}

impl<'a> Checker<'a> {
    fn new(sink: &'a mut DiagnosticSink, resolved: &'a Resolved) -> Self {
        // Seed the std::attrs targets so the standard attributes validate while
        // `std/` is still empty (mirrors the builtins seeded in siox-resolve).
        let mut attr_targets = HashMap::new();
        for (name, targets) in [
            ("top", &["entity"][..]),
            ("test", &["entity"]),
            ("keep", &["let", "port"]),
            ("library", &["entity"]),
            ("name", &["entity"]),
            ("signed", &["struct"]),
        ] {
            attr_targets.insert(name.to_string(), targets.iter().map(|s| s.to_string()).collect());
        }
        let mut attr_value_kinds = HashMap::new();
        for (name, ty) in [
            ("top", AttrValueTy::Bool),
            ("test", AttrValueTy::Bool),
            ("keep", AttrValueTy::Bool),
            ("library", AttrValueTy::Str),
            ("name", AttrValueTy::Str),
        ] {
            attr_value_kinds.insert(name.to_string(), ty);
        }
        // Mirror of std::ops' `Boolean` impls: `Bit` and `Bool` can be used
        // directly as conditions (spec 3.16); truth is 1-bit, '1' = true.
        // `Logic` is omitted, so it still requires an explicit comparison.
        // ponytail: hardcoded shim — replace with real trait-impl lookup when
        // trait resolution lands, so user `impl Boolean for T` works from source.
        let mut trait_impls: HashMap<String, HashSet<String>> = HashMap::new();
        trait_impls.insert(
            "Boolean".to_string(),
            ["Bit", "Bool"].iter().map(|s| s.to_string()).collect(),
        );
        Checker {
            sink,
            resolved,
            entities: HashMap::new(),
            attr_targets,
            attr_value_kinds,
            trait_impls,
            enum_variants: HashMap::new(),
            own_variants: HashMap::new(),
            enum_bases: HashMap::new(),
            structs: HashMap::new(),
            vector_families: HashMap::new(),
            generic_fns: HashMap::new(),
            suffix_types: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    /// First pass: record entity port types and declared attribute targets.
    fn collect(&mut self, modules: &[Module]) {
        // Two passes: gather type declarations (structs, enums, aliases,
        // attrs, impls) first, so entity-port typing below can already see
        // e.g. `struct uint : Logic[]` regardless of module/item order.
        for m in modules {
            for item in &m.items {
                if matches!(item, Item::Entity(_)) {
                    continue;
                }
                self.collect_decl(item);
            }
        }
        // Signedness (impl Signed for T) BEFORE entity ports are typed, so an
        // `int` port and an `int[16](..)` conversion agree.
        if let Some(signed) = self.trait_impls.get("Signed").cloned() {
            for (name, is_signed) in self.vector_families.iter_mut() {
                *is_signed = signed.contains(name);
            }
        }
        for m in modules {
            for item in &m.items {
                match item {
                    Item::Entity(e) => {
                        let ports = e
                            .ports
                            .iter()
                            .map(|p| PortInfo {
                                name: p.name.text.clone(),
                                ty: self.ast_ty(&p.ty),
                                dir: p.dir,
                            })
                            .collect();
                        self.entities.insert(e.name.text.clone(), ports);
                    }
                    _ => {}
                }
            }
        }
        // (inherited enum variants are expanded in collect_decl's tail)
        self.expand_inherited_variants();
    }

    /// Collect one non-entity type declaration.
    fn collect_decl(&mut self, item: &Item) {
        match item {
            Item::AttrDecl(a) => {
                let targets = a.targets.iter().map(|t| t.text.clone()).collect();
                self.attr_targets.insert(a.name.text.clone(), targets);
                let kind = match type_head_name(&a.ty) {
                    Some("Bool") => AttrValueTy::Bool,
                    Some("string") | Some("str") => AttrValueTy::Str,
                    _ => AttrValueTy::Other,
                };
                self.attr_value_kinds.insert(a.name.text.clone(), kind);
            }
            Item::Impl(im) => {
                // Record `impl Trait for Type` so trait-driven checks (e.g.
                // conditions) can ask "does T implement Trait?".
                if let Some(tr) = &im.trait_ {
                    let trait_name = tr.segments.last().map(|s| s.text.clone());
                    let target = type_head_name(&im.target).map(|s| s.to_string());
                    if let (Some(t), Some(ty)) = (trait_name, target) {
                        // `impl Suffix for T`: each fn's name is a literal
                        // suffix producing a T (spec 3.24).
                        if t == "Suffix" {
                            for it in &im.items {
                                if let ImplItem::Fn(f) = it {
                                    self.suffix_types
                                        .entry(f.name.text.clone())
                                        .or_default()
                                        .push(ty.clone());
                                }
                            }
                        }
                        self.trait_impls.entry(t).or_default().insert(ty);
                    }
                }
            }
            Item::Enum(e) => {
                let vars: Vec<String> =
                    e.variants.iter().map(|v| v.name.text.clone()).collect();
                self.own_variants.insert(e.name.text.clone(), vars.clone());
                self.enum_variants.insert(e.name.text.clone(), vars);
                if let Some(t) = &e.repr {
                    if let Some(h) = type_head_name(t) {
                        self.enum_bases.insert(e.name.text.clone(), h.to_string());
                    }
                }
            }
            Item::Fn(f) if !f.generics.params.is_empty() => {
                let vps = f
                    .params
                    .iter()
                    .filter(|p| !p.is_self)
                    .filter_map(|p| Some((p.name.as_ref()?.text.clone(), p.ty.clone()?)))
                    .collect();
                self.generic_fns
                    .insert(f.name.text.clone(), (f.generics.params.clone(), vps));
            }
            Item::Struct(st) => {
                let fields = st.fields.iter().map(|f| f.name.text.clone()).collect();
                self.structs.insert(st.name.text.clone(), (st.base.clone(), fields));
                // A bodyless struct over an array of bit scalars is a bit
                // vector by shape (`struct uint : Logic[]`); signedness comes
                // from `impl Signed for T`, applied in a post-pass.
                let is_vec = st.fields.is_empty()
                    && matches!(
                        st.base.as_ref().and_then(|b| match b {
                            Type::Indexed { base, .. } => type_head_name(base),
                            _ => None,
                        }),
                        Some("Logic" | "Bit" | "ULogic" | "Clock")
                    );
                if is_vec {
                    // Signedness (impl Signed for T) is applied in a post-pass,
                    // since the impl may be collected after the struct.
                    self.vector_families.insert(st.name.text.clone(), false);
                }
            }
            Item::Using(u) => {
                if let UsingKind::Alias { name, ty } = &u.kind {
                    self.aliases.insert(name.text.clone(), ty.clone());
                }
            }
            _ => {}
        }
    }

    /// Nominal enum derivation: prepend base variants (spec derived types).
    /// A base that isn't a known enum is a numeric repr — ignore it.
    fn expand_inherited_variants(&mut self) {
        let names: Vec<String> = self.enum_variants.keys().cloned().collect();
        for name in &names {
            let mut chain = Vec::new();
            let mut cur = name.clone();
            let mut prefix: Vec<String> = Vec::new();
            while let Some(base) = self.enum_bases.get(&cur).cloned() {
                if !self.enum_variants.contains_key(&base) || chain.contains(&base) {
                    break; // numeric repr, or cycle
                }
                chain.push(base.clone());
                cur = base;
            }
            for anc in chain.iter().rev() {
                if let Some(vs) = self.own_variants.get(anc) {
                    prefix.extend(vs.iter().cloned());
                }
            }
            if !prefix.is_empty() {
                let own = self.enum_variants.get(name).cloned().unwrap_or_default();
                prefix.extend(own);
                self.enum_variants.insert(name.clone(), prefix);
            }
        }
    }

    /// Derived-struct validation (spec: nominal derivation): a field-adding
    /// body requires a struct-shaped base (arrays reject fields — the
    /// index/field access models would collide), and no field may collide
    /// with an inherited one.
    fn check_struct(&mut self, st: &StructDecl) {
        for a in &st.attrs {
            let name = a.name.segments.last().map(|s| s.text.as_str()).unwrap_or("");
            if !self.attr_targets.contains_key(name) {
                self.error(codes::UNKNOWN_NAME, a.name.span, format!("unknown attribute `{name}`"));
                continue;
            }
            self.check_attr_target(a, "struct", Some(st.name.text.as_str()));
        }
        let Some(base) = &st.base else { return };
        // Array-shaped base + fields is rejected (after alias resolution).
        if !st.fields.is_empty() && self.is_array_base(base) {
            self.error(
                codes::TYPE_MISMATCH,
                st.name.span,
                "cannot add fields when deriving from an array type; use the \
                 bodyless form `struct B : A;` or explicit composition"
                    .to_string(),
            );
            return;
        }
        // Field-name collisions with the inherited base fields.
        let inherited = self.base_struct_fields(base);
        for f in &st.fields {
            if inherited.iter().any(|n| n == &f.name.text) {
                self.error(
                    codes::DUPLICATE_ITEM,
                    f.name.span,
                    format!("field `{}` already exists in the base struct", f.name.text),
                );
            }
        }
    }

    /// The (transitive) field names of a struct-shaped base type.
    fn base_struct_fields(&self, ty: &Type) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(head) = type_head_name(ty) {
            if let Some((base, own)) = self.structs.get(head) {
                if let Some(b) = base {
                    out.extend(self.base_struct_fields(b));
                }
                out.extend(own.iter().cloned());
            }
        }
        out
    }

    /// If `name` is an array-derived Logic family (`struct F : Logic[]` /
    /// `: Bit[]`, bodyless), returns its signedness — a nominal numeric vector
    /// (spec: derived types §5). `impl Signed for F` marks it signed. This is
    /// how uint/int and future fixed-point families are recognized without
    /// hardcoding their names.
    fn logic_vector_family(&self, name: &str) -> Option<bool> {
        // Membership is the `#[vector]` attribute (spec 3.5) — the compiler no
        // longer infers it from the `: Logic[]` shape.
        self.vector_families.get(name).copied()
    }

    /// Whether a base type resolves (through aliases) to an array shape.
    fn is_array_base(&self, ty: &Type) -> bool {
        match ty {
            Type::Indexed { .. } => true,
            Type::Path(p) if p.segments.len() == 1 => {
                match self.aliases.get(&p.segments[0].text) {
                    Some(t) => self.is_array_base(t),
                    None => false,
                }
            }
            _ => false,
        }
    }

    fn check_item(&mut self, item: &Item) {
        let sym = HashMap::new();
        let sym = &sym;
        match item {
            Item::Const(c) => self.check_expr(&c.value, sym),
            Item::Enum(e) => {
                for v in &e.variants {
                    if let Some(val) = &v.value {
                        self.check_expr(val, sym);
                    }
                }
            }
            Item::Entity(e) => {
                for a in &e.attrs {
                    self.check_attr_target(a, "entity", Some(e.name.text.as_str()));
                    self.check_attr_value(a);
                    if let Some(v) = &a.value {
                        self.check_expr(v, sym);
                    }
                }
            }
            Item::Impl(im) => self.check_impl(im),
            Item::Trait(t) => {
                for f in &t.items {
                    if let Some(b) = &f.body {
                        self.check_block(b);
                    }
                }
            }
            Item::Fn(f) => {
                // A generic fn's body is verified at each call (it inlines),
                // where the concrete types are known; checking it abstractly
                // (operators on the opaque `T`) would wrongly reject it.
                if f.generics.params.is_empty() {
                    if let Some(b) = &f.body {
                        self.check_block(b);
                    }
                }
            }
            Item::Struct(st) => self.check_struct(st),
            Item::Using(_) | Item::AttrDecl(_) | Item::ExternBlock { .. } => {}
        }
    }

    /// Spec 3.5: an attribute may only be applied to a target its declaration
    /// allows. Targets are item kinds (`entity`, `let`, `port`) or **type
    /// names** — `pub attr external_clock: Bool for Pll;` is valid only on
    /// the `Pll` entity or on declarations/instances of `Pll` (per-instance
    /// vendor metadata, preserved for external tools). Unknown attribute
    /// names on entities are reported by name resolution.
    fn check_attr_target(&mut self, a: &Attr, kind: &str, type_name: Option<&str>) {
        let name = a.name.segments.last().map(|s| s.text.as_str()).unwrap_or("");
        let verdict = self.attr_targets.get(name).map(|targets| {
            let ok = targets
                .iter()
                .any(|t| t == kind || Some(t.as_str()) == type_name);
            (ok, targets.join(", "))
        });
        if let Some((false, allowed)) = verdict {
            self.error(
                codes::INVALID_ATTR_TARGET,
                a.name.span,
                format!("attribute `{name}` cannot be applied to this {kind} (allowed: {allowed})"),
            );
        }
    }

    fn check_impl(&mut self, im: &ImplDecl) {
        let (in_ports, sym) = self.impl_env(im);
        for item in &im.items {
            match item {
                ImplItem::Const(c) => self.check_expr(&c.value, &sym),
                ImplItem::Let(l) => {
                    // Per-instance attributes: valid for `let` targets or when
                    // a named target matches the declaration's type (the
                    // instance's entity, or the annotated type head).
                    let type_name: Option<String> = match &l.value {
                        Some(Expr::Construct { ty: Some(t), .. }) => {
                            type_head_name(t).map(str::to_string)
                        }
                        _ => l.ty.as_ref().and_then(type_head_name).map(str::to_string),
                    };
                    for a in &l.attrs {
                        let name =
                            a.name.segments.last().map(|s| s.text.as_str()).unwrap_or("");
                        if !self.attr_targets.contains_key(name) {
                            self.error(
                                codes::UNKNOWN_NAME,
                                a.name.span,
                                format!("unknown attribute `{name}`"),
                            );
                            continue;
                        }
                        self.check_attr_target(a, "let", type_name.as_deref());
                        self.check_attr_value(a);
                    }
                    if let Some(v) = &l.value {
                        self.check_init(l.ty.as_ref(), v, &sym);
                        self.check_expr(v, &sym);
                    }
                }
                ImplItem::Fn(f) => {
                    if let Some(b) = &f.body {
                        self.check_block(b);
                    }
                }
                ImplItem::ModeField { .. } => {}
                ImplItem::Stmt(s) => self.check_stmt(s, &in_ports, &sym),
            }
        }
    }

    /// Build the value environment for an impl body: the `in` ports (for the
    /// write check) and a name -> type table (ports + impl-level lets/consts).
    fn impl_env(&self, im: &ImplDecl) -> (HashSet<String>, HashMap<String, Ty>) {
        let mut in_ports = HashSet::new();
        let mut sym = HashMap::new();
        if im.trait_.is_none() {
            if let Some(ports) = type_head_name(&im.target).and_then(|n| self.entities.get(n)) {
                for p in ports {
                    sym.insert(p.name.clone(), p.ty.clone());
                    if p.dir == Some(Direction::In) {
                        in_ports.insert(p.name.clone());
                    }
                }
            }
        }
        for it in &im.items {
            match it {
                ImplItem::Let(l) => {
                    let ty = l.ty.as_ref().map(|t| self.ast_ty(t)).unwrap_or(Ty::Error);
                    sym.insert(l.name.text.clone(), ty);
                }
                ImplItem::Const(c) => {
                    sym.insert(c.name.text.clone(), self.ast_ty(&c.ty));
                }
                _ => {}
            }
        }
        (in_ports, sym)
    }

    fn check_block(&mut self, b: &Block) {
        let (in_ports, sym) = (HashSet::new(), HashMap::new());
        for s in &b.stmts {
            self.check_stmt(s, &in_ports, &sym);
        }
    }

    fn check_stmt(&mut self, s: &Stmt, in_ports: &HashSet<String>, sym: &HashMap<String, Ty>) {
        match s {
            Stmt::Let(l) => {
                if let Some(v) = &l.value {
                    self.check_init(l.ty.as_ref(), v, sym);
                    self.check_expr(v, sym);
                }
            }
            Stmt::Assign { target, value, .. } => {
                self.check_write_target(target, in_ports);
                self.check_assignment(target, value, sym);
                self.check_expr(target, sym);
                self.check_expr(value, sym);
            }
            Stmt::If(i) => self.check_if(i, in_ports, sym),
            Stmt::Match(m) => {
                self.check_match_exhaustive(m, sym);
                self.check_unreachable_arms(m);
                self.check_expr(&m.scrutinee, sym);
                for arm in &m.arms {
                    for s in &arm.body.stmts {
                        self.check_stmt(s, in_ports, sym);
                    }
                }
            }
            Stmt::For { range, body, .. } => {
                self.check_expr(range, sym);
                for s in &body.stmts {
                    self.check_stmt(s, in_ports, sym);
                }
            }
            Stmt::Expr(e) => self.check_expr(e, sym),
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.check_expr(v, sym);
                }
            }
        }
    }

    fn check_if(&mut self, i: &IfStmt, in_ports: &HashSet<String>, sym: &HashMap<String, Ty>) {
        self.check_condition(&i.cond, sym);
        self.check_expr(&i.cond, sym);
        for s in &i.then.stmts {
            self.check_stmt(s, in_ports, sym);
        }
        match i.else_.as_deref() {
            Some(ElseBranch::Block(b)) => {
                for s in &b.stmts {
                    self.check_stmt(s, in_ports, sym);
                }
            }
            Some(ElseBranch::If(inner)) => self.check_if(inner, in_ports, sym),
            None => {}
        }
    }

    /// A condition's type must implement `Boolean` (spec 3.16, generalized).
    /// `Bit`/`Bool` have built-in impls; user types opt in with `impl Boolean
    /// for T`; `Logic` has none, so it still requires an explicit comparison.
    /// An unknown (`Error`) condition type is skipped to avoid false positives.
    fn check_condition(&mut self, cond: &Expr, sym: &HashMap<String, Ty>) {
        let ty = self.type_of(cond, sym);
        let Some(name) = self.type_kind_name(&ty) else { return };
        if !self.implements_boolean(&name) {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(cond),
                format!(
                    "`{name}` cannot be used directly as a condition; \
                     compare it explicitly (e.g. `== '1'`) or `impl Boolean for {name}`"
                ),
            );
        }
    }

    /// Warn (spec Stage 10) when a `match` on an enum omits variants and has no
    /// `_` wildcard.
    fn check_match_exhaustive(&mut self, m: &MatchStmt, sym: &HashMap<String, Ty>) {
        let Ty::Named(id) = self.type_of(&m.scrutinee, sym) else { return };
        let Some(enum_name) = self.resolved.def(id).map(|d| d.name.clone()) else { return };
        let Some(variants) = self.enum_variants.get(&enum_name).cloned() else { return };

        if m.arms.iter().any(|a| matches!(a.pattern, Pattern::Wildcard)) {
            return;
        }
        let covered: HashSet<&str> = m
            .arms
            .iter()
            .filter_map(|a| match &a.pattern {
                Pattern::Path(p) if p.segments.len() >= 2 => Some(p.segments[1].text.as_str()),
                _ => None,
            })
            .collect();
        let missing: Vec<String> =
            variants.into_iter().filter(|v| !covered.contains(v.as_str())).collect();
        if !missing.is_empty() {
            let names = missing.iter().map(|v| format!("`{v}`")).collect::<Vec<_>>().join(", ");
            self.sink.emit(
                Diagnostic::warning(format!("non-exhaustive match on `{enum_name}`: missing {names}"))
                    .with_code(codes::NON_EXHAUSTIVE_MATCH)
                    .at(m.span)
                    .help("add the missing arms, or a `_` wildcard"),
            );
        }
    }

    /// Warn (spec Stage 10) on arms that can never match: anything after a `_`
    /// wildcard, or a variant already covered by an earlier arm.
    fn check_unreachable_arms(&mut self, m: &MatchStmt) {
        let mut after_wildcard = false;
        let mut seen: HashSet<String> = HashSet::new();
        for arm in &m.arms {
            let reason = if after_wildcard {
                Some("a previous `_` already matches everything".to_string())
            } else {
                match &arm.pattern {
                    Pattern::Wildcard => {
                        after_wildcard = true;
                        None
                    }
                    Pattern::Path(p) if p.segments.len() >= 2 => {
                        let var = p.segments[1].text.clone();
                        (!seen.insert(var.clone()))
                            .then(|| format!("`{var}` is already matched by an earlier arm"))
                    }
                    _ => None,
                }
            };
            if let Some(reason) = reason {
                self.sink.emit(
                    Diagnostic::warning(format!("unreachable match arm: {reason}"))
                        .with_code(codes::UNREACHABLE_MATCH_ARM)
                        .at(arm.span),
                );
            }
        }
    }

    fn implements_boolean(&self, name: &str) -> bool {
        self.trait_impls.get("Boolean").is_some_and(|set| set.contains(name))
    }

    /// The name a type is keyed by in the trait-impl table (`uint[8]` and
    /// `uint` share `uint`). `Error`/array types have no name.
    fn type_kind_name(&self, t: &Ty) -> Option<String> {
        match t {
            Ty::Bit => Some("Bit".to_string()),
            Ty::Logic => Some("Logic".to_string()),
            Ty::Bool => Some("Bool".to_string()),
            Ty::Integer => Some("integer".to_string()),
            Ty::Vector { signed: false, .. } => Some("uint".to_string()),
            Ty::Vector { signed: true, .. } => Some("int".to_string()),
            Ty::Real => Some("real".to_string()),
            Ty::Char => Some("Char".to_string()),
            Ty::Named(id) => self.resolved.def(*id).map(|d| d.name.clone()),
            Ty::Array { .. } | Ty::Error => None,
        }
    }

    /// Spec 3.18: flag `port = ...` where `port` is a bare `in` port. Field /
    /// index writes (`bus.ready = ...`) are left for fuller direction analysis.
    fn check_write_target(&mut self, target: &Expr, in_ports: &HashSet<String>) {
        if let Expr::Path(p) = target {
            if p.segments.len() == 1 && in_ports.contains(&p.segments[0].text) {
                self.sink.emit(
                    Diagnostic::error(format!(
                        "cannot assign to input port `{}`",
                        p.segments[0].text
                    ))
                    .with_code(codes::WRITE_TO_INPUT_PORT)
                    .at(p.span)
                    .help("input ports are read-only inside the entity; drive it from the instantiating scope"),
                );
            }
        }
    }

    /// Spec 3.5: an attribute's value must match the type its declaration gives.
    fn check_attr_value(&mut self, a: &Attr) {
        let Some(value) = &a.value else { return };
        let name = a.name.segments.last().map(|s| s.text.as_str()).unwrap_or("");
        let expected = self.attr_value_kinds.get(name).copied();
        let ok = match expected {
            Some(AttrValueTy::Bool) => matches!(value, Expr::Bool { .. }),
            Some(AttrValueTy::Str) => matches!(value, Expr::StrLit { .. }),
            // Unknown attribute (reported by resolve) or an `Other`-typed one.
            _ => true,
        };
        if !ok {
            let want = match expected {
                Some(AttrValueTy::Bool) => "a Bool",
                Some(AttrValueTy::Str) => "a string",
                _ => "a different",
            };
            self.error(
                codes::INVALID_ATTR_VALUE_TYPE,
                expr_span(value),
                format!("attribute `{name}` expects {want} value"),
            );
        }
    }

    /// Spec 3.17: a `let name: T = e` initializer must be assignable to `T`.
    fn check_init(&mut self, decl_ty: Option<&Type>, value: &Expr, sym: &HashMap<String, Ty>) {
        let Some(t) = decl_ty else { return };
        self.check_value_range(t, value);
        let lhs = self.ast_ty(t);
        if !matches!(lhs, Ty::Error) && !self.assignable(&lhs, value, sym) {
            let rhs = self.type_of(value, sym);
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(value),
                format!(
                    "cannot initialize {} with {} without an explicit conversion",
                    ty_name(&lhs),
                    ty_name(&rhs)
                ),
            );
        }
    }

    /// Spec 3.17: the right-hand side of `target = value` must be assignable to
    /// the target's type. Only fires when the target type is known.
    fn check_assignment(&mut self, target: &Expr, value: &Expr, sym: &HashMap<String, Ty>) {
        let lhs = self.type_of(target, sym);
        if !matches!(lhs, Ty::Error) && !self.assignable(&lhs, value, sym) {
            let rhs = self.type_of(value, sym);
            self.sink.emit(
                Diagnostic::error(format!(
                    "cannot assign {} to {} without an explicit conversion",
                    ty_name(&rhs),
                    ty_name(&lhs)
                ))
                .with_code(codes::TYPE_MISMATCH)
                .at(expr_span(value))
                .help(format!("wrap it in a conversion, e.g. `{}(...)`", ty_name(&lhs))),
            );
        }
    }

    /// Whether `value` may be assigned to a target of type `lhs` without an
    /// explicit conversion. Integer and logic *literals* are polymorphic; an
    /// `Error` type on either side suppresses the check.
    /// Whether `id` is an enum declaring the character variant `ch`.
    fn enum_has_char_variant(&self, id: siox_resolve::DefId, ch: char) -> bool {
        let Some(d) = self.resolved.def(id) else { return false };
        self.enum_variants.get(&d.name).is_some_and(|vars| {
            vars.iter().any(|v| v.trim_matches('\'') == ch.to_string())
        })
    }

    /// Enforce a generic fn's trait bounds at the call site (spec: generic
    /// bounds). Each type parameter is inferred from the value argument whose
    /// declared type names it; a bound `T: Tr` requires the inferred type to
    /// satisfy `Tr`. Fns inline, so the call *is* the monomorphization —
    /// checking here gives an early, clear error instead of a post-inline one.
    fn check_generic_bounds(&mut self, callee: &Expr, args: &[Expr], sym: &HashMap<String, Ty>) {
        let Expr::Path(p) = callee else { return };
        if p.segments.len() != 1 {
            return;
        }
        let Some((generics, vparams)) = self.generic_fns.get(&p.segments[0].text).cloned() else {
            return;
        };
        for gp in &generics {
            let Some(bound) = &gp.bound else { continue };
            let Some(trait_name) = type_head_name(bound) else { continue };
            // Infer the type param from the first value param named after it.
            let inferred = vparams.iter().position(|(_, t)| type_head_name(t) == Some(&gp.name.text))
                .and_then(|i| args.get(i))
                .map(|a| self.type_of(a, sym));
            let Some(ty) = inferred else { continue };
            if !self.satisfies(&ty, trait_name) {
                let name = ty_name(&ty);
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(callee),
                    format!(
                        "`{name}` does not satisfy the bound `{}: {trait_name}`",
                        gp.name.text
                    ),
                );
            }
        }
    }

    /// Whether `ty` satisfies trait bound `trait_name`. A named struct/enum
    /// must have an explicit `impl Tr for it`; kernel scalars and vectors are
    /// assumed to carry the built-in capabilities (arithmetic, comparison), so
    /// they are accepted leniently — this catches a custom type missing the
    /// impl without false-flagging uint/int/etc.
    fn satisfies(&self, ty: &Ty, trait_name: &str) -> bool {
        match self.type_kind_name(ty) {
            Some(kind) => {
                if self.trait_impls.get(trait_name).is_some_and(|s| s.contains(&kind)) {
                    return true;
                }
                // A named (struct/enum) type without the impl fails; a kernel
                // scalar / vector is accepted (built-in capability).
                !matches!(ty, Ty::Named(_))
            }
            None => true,
        }
    }

    /// Compile-time fit check for conversion expressions with constant
    /// arguments: the value must be representable in the target container.
    fn check_conversion_fit(&mut self, callee: &Expr, args: &[Expr], site: &Expr) {
        // Target family + width from the conversion callee shape.
        let (signed, width) = match callee {
            Expr::Index { base, index, .. } => {
                let head = match base.as_ref() {
                    Expr::Path(p) if p.segments.len() == 1 => p.segments[0].text.as_str(),
                    _ => return,
                };
                let Some(w) = signed_lit(index) else { return };
                match self.vector_families.get(head) {
                    Some(&signed) => (signed, w),
                    None => return,
                }
            }
            Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == "resize" => {
                let Some(w) = args.get(1).and_then(signed_lit) else { return };
                (false, w) // resize width bound; the family is the argument's
            }
            _ => return,
        };
        if !(1..=64).contains(&width) {
            return;
        }
        fn const_fold(e: &Expr) -> Option<i64> {
            match e {
                Expr::Binary { op, lhs, rhs, .. } => {
                    let (a, b) = (const_fold(lhs)?, const_fold(rhs)?);
                    Some(match op {
                        BinOp::Add => a + b,
                        BinOp::Sub => a - b,
                        BinOp::Mul => a * b,
                        BinOp::Div if b != 0 => a / b,
                        _ => return None,
                    })
                }
                _ => signed_lit(e),
            }
        }
        let Some(v) = args.first().and_then(const_fold) else { return };
        let fits = if signed {
            let half = 1i64 << (width - 1);
            (-half..half).contains(&v)
        } else {
            v >= 0 && (width == 64 || v < (1i64 << width))
        };
        if !fits {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(site),
                format!("`{v}` does not fit in `{}[{width}]`", if signed { "int" } else { "uint" }),
            );
        }
    }

    fn assignable(&self, lhs: &Ty, value: &Expr, sym: &HashMap<String, Ty>) -> bool {
        match value {
            // A numeric literal also initialises `real` (`.re = 10` is 10.0).
            Expr::Int { .. } => matches!(lhs, Ty::Vector { .. } | Ty::Integer | Ty::Real | Ty::Error),
            Expr::LogicLit { ch, .. } => {
                // A character literal reads through its context type (spec:
                // type kernel): builtin scalars, `Char`, or a user enum with
                // a matching character variant (e.g. ULogic's 'Z').
                if let Ty::Named(id) = lhs {
                    return self.enum_has_char_variant(*id, *ch);
                }
                matches!(lhs, Ty::Bit | Ty::Logic | Ty::Char | Ty::Error)
            }
            _ => compatible(lhs, &self.type_of(value, sym)),
        }
    }

    /// Walk an expression for the Phase-2 `::ddt` guard (the only expression-
    /// local check so far).
    fn check_expr(&mut self, e: &Expr, sym: &HashMap<String, Ty>) {
        match e {
            Expr::SysAttr { base, attr, span } => {
                if PHASE2_ATTRS.contains(&attr.text.as_str()) {
                    self.error(
                        codes::PHASE2_SYNTAX,
                        *span,
                        format!("`::{}` is Phase-2 analogue syntax, not available in Phase 1", attr.text),
                    );
                }
                self.check_expr(base, sym);
            }
            Expr::Field { base, .. } => self.check_expr(base, sym),
            Expr::Index { base, index, .. } => {
                self.check_expr(base, sym);
                self.check_expr(index, sym);
            }
            Expr::Range { lo, hi, .. } => {
                self.check_expr(lo, sym);
                self.check_expr(hi, sym);
            }
            Expr::Unary { op, rhs, span } => {
                self.check_expr(rhs, sym);
                // `not` is per-bit boolean — bit-derived / Boolean operands only.
                if matches!(op, UnOp::Not) {
                    let t = self.type_of(rhs, sym);
                    if matches!(t, Ty::Real | Ty::Char) {
                        self.error(
                            codes::TYPE_MISMATCH,
                            *span,
                            format!("`not` is a per-bit operator; `{}` is not a bit-derived type", ty_name(&t)),
                        );
                    }
                }
            }
            Expr::IfExpr { cond, then, els, .. } => {
                // Same condition rule as statement `if` (must be Boolean).
                self.check_condition(cond, sym);
                self.check_expr(cond, sym);
                self.check_expr(then, sym);
                self.check_expr(els, sym);
            }
            Expr::Binary { op, lhs, rhs, span } => {
                self.check_expr(lhs, sym);
                self.check_expr(rhs, sym);
                // A character literal's identity comes from its counterpart's
                // type (spec: type kernel); a numeric counterpart cannot read
                // one — conversion goes through an encoding table.
                for (lit, other) in [(lhs, rhs), (rhs, lhs)] {
                    if matches!(lit.as_ref(), Expr::LogicLit { .. })
                        && matches!(
                            self.type_of(other, sym),
                            Ty::Vector { .. } | Ty::Integer | Ty::Real
                        )
                    {
                        self.error(
                            codes::TYPE_MISMATCH,
                            *span,
                            "a character literal has no numeric identity; convert it                              through an encoding table (std::text)"
                                .to_string(),
                        );
                    }
                }
                let op_str = siox_syntax::pretty::bin_op(*op);
                // The boolean operators (`and`/`or`/`xor`/...) are "boolean,
                // per bit": on a bit array they act element-wise and return
                // the same array, on `Bool` they are plain boolean. They are
                // only meaningful on Boolean and bit-derived types — never on
                // `real` or `Char`.
                if matches!(op_str, "and" | "or" | "xor" | "nand" | "nor" | "xnor") {
                    for operand in [lhs, rhs] {
                        let t = self.type_of(operand, sym);
                        // A literal is a bit-mask that coerces to the other
                        // operand's width (`b and 31`); a non-literal number
                        // (`integer`/`real`) or a `Char` is not bit-derived.
                        let is_lit = matches!(
                            operand.as_ref(),
                            Expr::Int { .. } | Expr::SuffixLit { .. } | Expr::BitStrLit { .. }
                        );
                        let bad = matches!(t, Ty::Real | Ty::Char)
                            || (matches!(t, Ty::Integer) && !is_lit);
                        if bad {
                            self.error(
                                codes::TYPE_MISMATCH,
                                *span,
                                format!(
                                    "`{op_str}` needs bit-derived operands (Bit/Logic/Bool/uint/int); `{}` is a number",
                                    ty_name(&t)
                                ),
                            );
                            break;
                        }
                    }
                }
                // A user struct/enum operand needs an operator-trait impl
                // (spec 3.25); intrinsic numerics keep built-in semantics.
                // `==`/`!=` on enums stay built-in (discriminant compare).
                if !matches!(op_str, "==" | "!=") {
                    if let Some(name) = self.named_operand_name(lhs, sym) {
                        let has_op = |tr: &str| {
                            self.trait_impls.get(tr).is_some_and(|set| set.contains(&name))
                        };
                        // The Rust-style trait for this operator; one `Ord`
                        // (cmp -> Ordering) impl derives every comparison.
                        let tr = siox_syntax::ast::op_trait_name(op_str).unwrap_or(op_str);
                        let is_cmp = matches!(op_str, "<" | "<=" | ">" | ">=");
                        let has = has_op(tr) || (is_cmp && has_op("Ord"));
                        if !has {
                            let want = if is_cmp { "Ord" } else { tr };
                            self.error(
                                codes::TYPE_MISMATCH,
                                *span,
                                format!("`{op_str}` needs an `impl {want} for {name}`"),
                            );
                        }
                    }
                }
            }
            Expr::Call { callee, args, .. } => {
                self.check_expr(callee, sym);
                for a in args {
                    self.check_expr(a, sym);
                }
                // A constant conversion argument must FIT the target
                // (spec 3.17/3.26): `uint[4](300)` is a compile-time error,
                // like `let b: Byte = 300`. Dynamic values get simulation
                // range checks later (with the S3 reporting machinery).
                self.check_conversion_fit(callee, args, e);
                self.check_generic_bounds(callee, args, sym);
            }
            Expr::Construct { args, .. } => {
                for c in args {
                    if let Some(v) = &c.value {
                        self.check_expr(v, sym);
                    }
                }
            }
            Expr::Concat { parts, .. } => {
                for p in parts {
                    self.check_expr(p, sym);
                }
            }
            Expr::SuffixLit { suffix, span, .. } => {
                match self.suffix_types.get(&suffix.text).map(|v| v.as_slice()) {
                    Some([_]) => {} // one `impl Suffix` fn defines it
                    Some(tys) => {
                        let list = tys
                            .iter()
                            .map(|t| format!("{t}::{}", suffix.text))
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.error(
                            codes::UNKNOWN_NAME,
                            *span,
                            format!("literal suffix `{}` is ambiguous: {list}", suffix.text),
                        );
                    }
                    // No Suffix impl in scope: the fixed fs/Hz table backs
                    // bare files (spec 3.24).
                    None => {
                        if suffix_scale(&suffix.text).is_none() {
                            self.error(
                                codes::UNKNOWN_NAME,
                                *span,
                                format!("unknown literal suffix `{}`", suffix.text),
                            );
                        }
                    }
                }
            }
            Expr::BitStrLit { base, digits, span } => {
                let radix = if *base == 'x' { 16 } else { 2 };
                if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        format!("invalid {} bit-string literal `{base}\"{digits}\"`",
                            if radix == 16 { "hex" } else { "binary" }),
                    );
                }
            }
            Expr::Int { .. }
            | Expr::LogicLit { .. }
            | Expr::StrLit { .. }
            | Expr::Bool { .. }
            | Expr::Path(_) => {}
        }
    }

    // --- type inference core ------------------------------------------------

    /// Best-effort type of an expression given the in-scope value table. Unknown
    /// or unsupported cases yield [`Ty::Error`], which suppresses dependent
    /// checks rather than producing a false positive.
    fn type_of(&self, e: &Expr, sym: &HashMap<String, Ty>) -> Ty {
        match e {
            Expr::Int { .. } => Ty::Integer,
            // `if c { a } else { b }` takes its branches' type (the then arm;
            // branch-mismatch diagnostics ride on assignment compatibility).
            Expr::IfExpr { then, .. } => self.type_of(then, sym),
            // A suffix defined by `impl Suffix for T` types the literal as T;
            // the fixed fs/Hz table backs bare files as integer.
            Expr::SuffixLit { suffix, .. } => {
                if let Some([ty]) = self.suffix_types.get(&suffix.text).map(|v| v.as_slice()) {
                    return self
                        .resolved
                        .defs()
                        .iter()
                        .position(|d| {
                            d.name == *ty
                                && matches!(d.kind, DefKind::Struct | DefKind::Enum)
                        })
                        .map(|i| Ty::Named(siox_resolve::DefId(i as u32)))
                        .unwrap_or(Ty::Error);
                }
                if suffix_scale(&suffix.text).is_some() { Ty::Integer } else { Ty::Error }
            }
            Expr::BitStrLit { base, digits, .. } => {
                Ty::Vector { width: digits.len() as u32 * if *base == 'x' { 4 } else { 1 }, signed: false }
            }
            Expr::LogicLit { .. } => Ty::Logic,
            Expr::Bool { .. } => Ty::Bool,
            Expr::StrLit { .. } => Ty::Error,
            Expr::Path(p) => {
                if p.segments.len() == 1 {
                    sym.get(&p.segments[0].text).cloned().unwrap_or(Ty::Error)
                } else {
                    // `Enum::Variant` has the enum's type, not the variant's.
                    match self.resolved.resolved(p.span).and_then(|id| self.resolved.def(id)) {
                        Some(d) if d.kind == DefKind::EnumVariant => {
                            d.parent.map(Ty::Named).unwrap_or(Ty::Error)
                        }
                        _ => self.resolved.resolved(p.span).map(Ty::Named).unwrap_or(Ty::Error),
                    }
                }
            }
            Expr::SysAttr { base, attr, .. } => match attr.text.as_str() {
                "event" | "rising" | "falling" | "edge" => Ty::Bool,
                "old" => self.type_of(base, sym),
                "width" | "high" | "low" | "left" | "right" => Ty::Integer,
                _ => Ty::Error,
            },
            Expr::Binary { op, lhs, rhs, .. } => {
                if is_comparison(*op) {
                    return Ty::Bool;
                }
                let lhs_ty = self.type_of(lhs, sym);
                // An integer literal joins the other operand's numeric type
                // (`100 / r` with r: int[8] is an int[8], via the std
                // `impl Div<int> for integer`).
                if matches!(lhs_ty, Ty::Integer) {
                    if let r @ (Ty::Vector { signed: true, .. } | Ty::Vector { signed: false, .. }) = self.type_of(rhs, sym) {
                        return r;
                    }
                }
                // A mixed-operand operator impl (`10 + 5i`) yields the
                // impl-owning operand's type.
                if !matches!(lhs_ty, Ty::Named(_)) {
                    if let Ty::Named(id) = self.type_of(rhs, sym) {
                        let has_impl = self
                            .resolved
                            .def(id)
                            .map(|d| &d.name)
                            .is_some_and(|name| {
                                let op_str = siox_syntax::pretty::bin_op(*op);
                                let tr = siox_syntax::ast::op_trait_name(op_str)
                                    .unwrap_or(op_str);
                                self.trait_impls
                                    .get(tr)
                                    .is_some_and(|set| set.contains(name))
                            });
                        if has_impl {
                            return Ty::Named(id);
                        }
                    }
                }
                lhs_ty
            }
            Expr::Unary { rhs, .. } => self.type_of(rhs, sym),
            // A name-less struct literal (`ty: None`) takes its type from the
            // assignment target, which `type_of` does not see here.
            Expr::Construct { ty, .. } => ty.as_ref().map(|t| self.ast_ty(t)).unwrap_or(Ty::Error),
            // A concatenation is an unsigned bit vector of unknown width.
            Expr::Concat { .. } => Ty::Vector { width: 0, signed: false },
            // Conversion expressions type as their target (spec 3.17):
            // `uint[16](x)`, `int[8](x)`, `integer(x)`, `resize(x, n)`.
            Expr::Call { callee, args, .. } => match callee.as_ref() {
                Expr::Index { base, index, .. } => {
                    let head = match base.as_ref() {
                        Expr::Path(p) if p.segments.len() == 1 => p.segments[0].text.as_str(),
                        _ => "",
                    };
                    let w = signed_lit(index).unwrap_or(0).max(0) as u32;
                    match self.vector_families.get(head) {
                        Some(&signed) => Ty::Vector { width: w, signed },
                        None => Ty::Error,
                    }
                }
                Expr::Path(p) if p.segments.len() == 1 => match p.segments[0].text.as_str() {
                    // A named struct/enum: a `From` conversion, typed as the
                    // target (fn calls and kernel conversions fall through).
                    name
                        if name != "integer"
                            && name != "resize"
                            && match self.path_ty(p) {
                                Ty::Named(id) => self
                                    .resolved
                                    .def(id)
                                    .is_some_and(|d| {
                                        matches!(d.kind, DefKind::Struct | DefKind::Enum)
                                    }),
                                _ => false,
                            } =>
                    {
                        self.path_ty(p)
                    }
                    "integer" => Ty::Integer,
                    "Char" => Ty::Char,
                    // resize keeps the argument's family at the new width.
                    "resize" => {
                        let w = args.get(1).and_then(signed_lit).unwrap_or(0).max(0) as u32;
                        match args.first().map(|a| self.type_of(a, sym)) {
                            Some(Ty::Vector { signed: true, .. }) => Ty::Vector { width: w, signed: true },
                            Some(Ty::Vector { signed: false, .. }) => Ty::Vector { width: w, signed: false },
                            _ => Ty::Vector { width: w, signed: false },
                        }
                    }
                    _ => Ty::Error,
                },
                _ => Ty::Error,
            },
            Expr::Field { .. } | Expr::Index { .. } | Expr::Range { .. } => {
                Ty::Error
            }
        }
    }

    /// The declared name of an operand's type when it is a user struct/enum
    /// (the types operator-trait impls target). `None` for intrinsics and
    /// unknowns, which keep built-in operator semantics.
    fn named_operand_name(&self, e: &Expr, sym: &HashMap<String, Ty>) -> Option<String> {
        match self.type_of(e, sym) {
            Ty::Named(id) => {
                let d = self.resolved.def(id)?;
                matches!(d.kind, DefKind::Struct | DefKind::Enum).then(|| d.name.clone())
            }
            _ => None,
        }
    }

    /// A constant initializer must lie inside a value-range-constrained
    /// numeric type (`let b: integer<0..255> = 300;` is an error). Literal
    /// bounds only; named ranges and dynamic values are runtime checks later.
    fn check_value_range(&mut self, decl_ty: &Type, value: &Expr) {
        // Resolve one alias hop (`using Byte = integer<0..255>`).
        let resolved;
        let t = match decl_ty {
            Type::Path(p) if p.segments.len() == 1 => {
                match self.aliases.get(&p.segments[0].text) {
                    Some(a) => {
                        resolved = a.clone();
                        &resolved
                    }
                    None => decl_ty,
                }
            }
            _ => decl_ty,
        };
        let Type::Generic { base, args, .. } = t else { return };
        let Type::Path(p) = base.as_ref() else { return };
        if p.segments.last().map(|s| s.text.as_str()) != Some("integer") {
            return;
        }
        let [GenericArg::Positional(Expr::Range { lo, hi, .. })] = args.as_slice() else {
            return;
        };
        let (Some(a), Some(b)) = (signed_lit(lo), signed_lit(hi)) else { return };
        let (min, max) = (a.min(b), a.max(b));
        if let Some(v) = signed_lit(value) {
            if v < min || v > max {
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(value),
                    format!("value {v} is outside the range {min}..{max}"),
                );
            }
        }
    }

    /// Resolve a type annotation to a [`Ty`]. Parametric widths (`uint[W]`)
    /// become `Vector { width: 0, .. }` until elaboration fills them in.
    fn ast_ty(&self, t: &Type) -> Ty {
        match t {
            Type::Path(p) => self.path_ty(p),
            Type::Indexed { base, index, .. } => {
                // Unconstrained (`Char[]`): width 0 = "set at use".
                let width = index.as_deref().map(width_of).unwrap_or(0);
                match self.ast_ty(base) {
                    Ty::Vector { signed, .. } => Ty::Vector { width, signed },
                    other => Ty::Array { elem: Box::new(other), len: width },
                }
            }
            Type::Generic { base, .. } => self.ast_ty(base),
            Type::Mode { inner, .. } => self.ast_ty(inner),
        }
    }

    fn path_ty(&self, p: &Path) -> Ty {
        if p.segments.len() == 1 {
            match p.segments[0].text.as_str() {
                "Bit" => Ty::Bit,
                "Logic" => Ty::Logic,
                "Bool" => Ty::Bool,
                // A clock is a single-bit signal; treat it as Bit for checking
                // (clock-as-condition correctness is a separate, later concern).
                "Clock" => Ty::Bit,
                // `integer` is the kernel word; `uint`/`int` are no longer
                // names here — they resolve as array-derived Logic families
                // (`struct uint : Logic[]` in std::bits) via the arm below.
                "integer" => Ty::Integer,
                "real" => Ty::Real,
                "Char" => Ty::Char,
                // Elaboration-time range constants (`const BYTE: range`);
                // opaque to value checking.
                "range" => Ty::Error,
                name => match self.aliases.get(name) {
                    Some(t) => self.ast_ty(&t.clone()),
                    None => match self.logic_vector_family(name) {
                        // An array-derived Logic family behaves as a numeric
                        // vector: width applies via `F[N]` (ast_ty's Indexed).
                        Some(true) => Ty::Vector { width: 0, signed: true },
                        Some(false) => Ty::Vector { width: 0, signed: false },
                        None => {
                            self.resolved.resolved(p.span).map(Ty::Named).unwrap_or(Ty::Error)
                        }
                    },
                },
            }
        } else {
            self.resolved.resolved(p.span).map(Ty::Named).unwrap_or(Ty::Error)
        }
    }

    fn error(&mut self, code: &'static str, span: Span, msg: String) {
        self.sink.emit(Diagnostic::error(msg).with_code(code).at(span));
    }
}

/// The base name of a type (`Counter<W>` -> `Counter`, `out S::Source` -> `S`).
fn type_head_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Path(p) => p.segments.first().map(|s| s.text.as_str()),
        Type::Generic { base, .. } | Type::Indexed { base, .. } => type_head_name(base),
        Type::Mode { inner, .. } => type_head_name(inner),
    }
}

/// Width of a bracketed type index when it is a literal (`uint[8]` -> 8);
/// otherwise `0`, meaning "parametric / not yet known".
fn width_of(index: &Expr) -> u32 {
    match index {
        Expr::Int { text, .. } => text.parse().unwrap_or(0),
        _ => 0,
    }
}

fn is_comparison(op: BinOp) -> bool {
    matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
}

/// Whether a value of type `rhs` may be assigned to `lhs` with no conversion.
/// A width of `0` is "not yet known" (parametric) and assumed compatible — the
/// concrete width check happens after elaboration.
fn compatible(lhs: &Ty, rhs: &Ty) -> bool {
    use Ty::*;
    if matches!(lhs, Error) || matches!(rhs, Error) {
        return true;
    }
    match (lhs, rhs) {
        (Bit, Bit) | (Logic, Logic) | (Bool, Bool) | (Char, Char) | (Real, Real) => true,
        // `integer` is the number kernel; it coerces to/from any bit vector
        // (a uint[8] accepts `42`, and a vector's value is an integer).
        (Integer, Integer) => true,
        (Integer, Vector { .. }) | (Vector { .. }, Integer) => true,
        (Vector { width: a, signed: sa }, Vector { width: b, signed: sb }) => {
            sa == sb && (*a == 0 || *b == 0 || a == b)
        }
        (Named(a), Named(b)) => a == b,
        // Whole-array copy: same element type, matching length (0 = unset).
        (Array { elem: ea, len: la }, Array { elem: eb, len: lb }) => {
            compatible(ea, eb) && (*la == 0 || *lb == 0 || la == lb)
        }
        _ => false,
    }
}

fn ty_name(t: &Ty) -> String {
    match t {
        Ty::Bit => "Bit".to_string(),
        Ty::Logic => "Logic".to_string(),
        Ty::Bool => "Bool".to_string(),
        Ty::Real => "real".to_string(),
        Ty::Char => "Char".to_string(),
        Ty::Integer => "integer".to_string(),
        Ty::Vector { width: 0, signed: false } => "uint".to_string(),
        Ty::Vector { width: w, signed: false } => format!("uint[{w}]"),
        Ty::Vector { width: 0, signed: true } => "int".to_string(),
        Ty::Vector { width: w, signed: true } => format!("int[{w}]"),
        Ty::Named(_) => "a named type".to_string(),
        Ty::Array { .. } => "an array".to_string(),
        Ty::Error => "<unknown>".to_string(),
    }
}

/// The value of an integer literal, allowing a leading unary minus.
fn signed_lit(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int { text, .. } => text.parse::<i64>().ok(),
        Expr::Unary { op: UnOp::Neg, rhs, .. } => signed_lit(rhs).map(|v| -v),
        _ => None,
    }
}

fn expr_span(e: &Expr) -> Span {
    match e {
        Expr::Int { span, .. }
        | Expr::SuffixLit { span, .. }
        | Expr::BitStrLit { span, .. }
        | Expr::LogicLit { span, .. }
        | Expr::StrLit { span, .. }
        | Expr::Bool { span, .. }
        | Expr::Field { span, .. }
        | Expr::SysAttr { span, .. }
        | Expr::IfExpr { span, .. }
        | Expr::Index { span, .. }
        | Expr::Range { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Binary { span, .. }
        | Expr::Call { span, .. }
        | Expr::Construct { span, .. }
        | Expr::Concat { span, .. } => *span,
        Expr::Path(p) => p.span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use siox_diag::FileId;

    const VEC: &str = "\nstruct uint : Logic[];\nstruct int : Logic[];\n";

    fn check_src(src: &str) -> usize {
        let src = format!("{src}{VEC}");
        let src = src.as_str();
        let mut sink = DiagnosticSink::new();
        let module = siox_syntax::parse_module(FileId(0), src, &mut sink);
        assert_eq!(sink.error_count(), 0, "source failed to parse:\n{src}");
        let resolved = siox_resolve::resolve(std::slice::from_ref(&module), &mut sink);
        let parse_resolve_errors = sink.error_count();
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        sink.error_count() - parse_resolve_errors
    }

    /// The number of warnings with a given code emitted while checking `src`.
    fn warnings(src: &str, code: &str) -> usize {
        let src = format!("{src}{VEC}");
        let src = src.as_str();
        let mut sink = DiagnosticSink::new();
        let module = siox_syntax::parse_module(FileId(0), src, &mut sink);
        let resolved = siox_resolve::resolve(std::slice::from_ref(&module), &mut sink);
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        sink.diagnostics().iter().filter(|d| d.code == Some(code)).count()
    }

    #[test]
    fn unreachable_match_arms_warn() {
        let base = "module m;\nenum State { Idle, Run, Done }\nentity E { out y: Bit; }\nimpl E {\n  let s: State;\n  match s {\n    ARMS\n  }\n}\n";
        // An arm after `_` is unreachable.
        assert_eq!(
            warnings(
                &base.replace("ARMS", "_ => { y = '0'; } State::Idle => { y = '1'; }"),
                codes::UNREACHABLE_MATCH_ARM
            ),
            1
        );
        // A repeated variant is unreachable.
        assert_eq!(
            warnings(
                &base.replace(
                    "ARMS",
                    "State::Idle => { y = '0'; } State::Idle => { y = '1'; } _ => { y = '0'; }"
                ),
                codes::UNREACHABLE_MATCH_ARM
            ),
            1
        );
        // A normal, distinct set of arms is fine.
        assert_eq!(
            warnings(
                &base.replace("ARMS", "State::Idle => { y = '0'; } _ => { y = '1'; }"),
                codes::UNREACHABLE_MATCH_ARM
            ),
            0
        );
    }

    #[test]
    fn non_exhaustive_enum_match_warns() {
        let base = "module m;\nenum State { Idle, Run, Done }\nentity E { out y: Bit; }\nimpl E {\n  let s: State;\n  match s {\n    ARMS\n  }\n}\n";
        // Missing `Done` and no `_` -> one warning.
        assert_eq!(
            warnings(
                &base.replace("ARMS", "State::Idle => { y = '0'; } State::Run => { y = '1'; }"),
                codes::NON_EXHAUSTIVE_MATCH
            ),
            1
        );
        // A `_` wildcard is exhaustive.
        assert_eq!(
            warnings(
                &base.replace("ARMS", "State::Idle => { y = '0'; } _ => { y = '1'; }"),
                codes::NON_EXHAUSTIVE_MATCH
            ),
            0
        );
        // All variants covered is exhaustive.
        assert_eq!(
            warnings(
                &base.replace(
                    "ARMS",
                    "State::Idle => { y = '0'; } State::Run => { y = '1'; } State::Done => { y = '0'; }"
                ),
                codes::NON_EXHAUSTIVE_MATCH
            ),
            0
        );
    }

    #[test]
    fn rejects_phase2_ddt() {
        let errors = check_src("module m;\nentity E { out y: Bit; }\nimpl E {\n  y = x::ddt;\n}\n");
        assert_eq!(errors, 1);
    }

    #[test]
    fn accepts_digital_sysattrs() {
        let errors = check_src(
            "module m;\nentity E { in clk: Clock; out q: Bit; }\nimpl E {\n  if clk::rising {\n    q = clk::old;\n  }\n}\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn rejects_write_to_input_port() {
        let errors = check_src(
            "module m;\nentity E { in en: Bit; out y: Bit; }\nimpl E {\n  en = '1';\n  y = en;\n}\n",
        );
        assert_eq!(errors, 1);
    }

    #[test]
    fn writing_output_is_fine() {
        let errors = check_src(
            "module m;\nentity E { in en: Bit; out y: Bit; }\nimpl E {\n  y = en;\n}\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn bare_logic_condition_is_rejected() {
        let errors = check_src(
            "module m;\nentity E { in rst: Logic; out y: Bit; }\nimpl E {\n  if rst {\n    y = '0';\n  }\n}\n",
        );
        assert_eq!(errors, 1);
    }

    #[test]
    fn compared_logic_and_bit_conditions_are_fine() {
        // `rst == '1'` is a comparison (-> Bool); `en` is a Bit. Both valid.
        let errors = check_src(
            "module m;\nentity E { in rst: Logic; in en: Bit; out y: Bit; }\nimpl E {\n  if rst == '1' {\n    y = '0';\n  }\n  if en {\n    y = '1';\n  }\n}\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn attribute_on_wrong_target_is_rejected() {
        // `keep` is declared for `let, port`, not `entity`.
        let errors = check_src("module m;\n#[keep]\nentity E { out y: Bit; }\n");
        assert_eq!(errors, 1);
    }

    #[test]
    fn attribute_on_right_target_is_fine() {
        let errors = check_src("module m;\n#[top]\nentity E { out y: Bit; }\n");
        assert_eq!(errors, 0);
    }

    #[test]
    fn assigning_bool_to_a_bit_port_is_rejected() {
        let errors = check_src(
            "module m;\nentity E { in en: Bit; out y: Bit; }\nimpl E {\n  y = en == en;\n}\n",
        );
        // `en == en` is Bool; `y` is Bit.
        assert_eq!(errors, 1);
    }

    #[test]
    fn integer_and_logic_literals_are_polymorphic() {
        // int literal -> any uint; '1' -> Bit or Logic. No conversions needed.
        let errors = check_src(
            "module m;\nentity E { out count: uint[8]; out q: Bit; out clk: Logic; }\nimpl E {\n  let value: uint[8] = 0;\n  count = value;\n  q = '1';\n  clk = '0';\n}\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn enum_assignment_uses_the_enum_type() {
        let errors = check_src(
            "module m;\nenum State { Idle, Run }\nentity E { out s: State; }\nimpl E {\n  s = State::Idle;\n}\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn bad_initializer_type_is_rejected() {
        let errors = check_src(
            "module m;\nentity E { out y: Bit; }\nimpl E {\n  let flag: Bool = 5;\n  y = '0';\n}\n",
        );
        assert_eq!(errors, 1);
    }

    #[test]
    fn attribute_value_type_is_checked() {
        // `name` expects a string; giving it an int is an error.
        let bad = check_src("module m;\n#[name = 5]\nentity E { out y: Bit; }\n");
        assert_eq!(bad, 1);
        let good = check_src("module m;\n#[name = \"dut\"]\nentity E { out y: Bit; }\n");
        assert_eq!(good, 0);
    }

    #[test]
    fn operators_on_user_types_need_an_impl() {
        let base = "module m;\nstruct V { a: Bit }\nOPIMPL\nentity E { in p: V; in q: V; out y: Bit; }\nimpl E {\n  let r: V = p + q;\n  y = '0';\n}\n";
        // Without an impl, `+` on a struct is rejected.
        assert_eq!(check_src(&base.replace("OPIMPL\n", "")), 1);
        // With `impl Add for V`, it is accepted.
        assert_eq!(
            check_src(&base.replace(
                "OPIMPL",
                "impl Add for V {\n  fn add(self, rhs: V) -> V {\n    return self;\n  }\n}"
            )),
            0
        );
    }

    #[test]
    fn suffix_traits_define_and_disambiguate_literals() {
        let time = "struct Time { fs: uint[48] }\nimpl Suffix for Time {\n  fn s(v: integer) -> Time {\n    return Time { .fs = v };\n  }\n}\n";
        // A Suffix-impl fn defines the literal's type: Time = 5s init passes.
        assert_eq!(
            check_src(&format!(
                "module m;\n{time}entity E {{ out y: Bit; }}\nimpl E {{\n  let t: Time = 5s;\n  y = '0';\n}}\n"
            )),
            0
        );
        // Two types defining the same suffix is an ambiguity error (the
        // cascading init mismatch is separate).
        let score = "struct Score { p: uint[8] }\nimpl Suffix for Score {\n  fn s(v: integer) -> Score {\n    return Score { .p = v };\n  }\n}\n";
        let src = format!(
            "module m;\n{time}{score}entity E {{ out y: Bit; }}\nimpl E {{\n  let t: Time = 5s;\n  y = '0';\n}}\n"
        );
        assert_eq!(warnings(&src, codes::UNKNOWN_NAME), 1);
    }

    #[test]
    fn suffix_and_bitstring_literals_are_checked() {
        // Known unit suffixes and valid bit-strings pass.
        assert_eq!(
            check_src(
                "module m;\nentity E { out y: uint[8]; }\nimpl E {\n  let t = 10ns;\n  let f = 100MHz;\n  y = x\"AB\";\n}\n"
            ),
            0
        );
        // An unknown suffix is an error.
        assert_eq!(
            check_src("module m;\nentity E { out y: Bit; }\nimpl E {\n  let c = 5i;\n  y = '0';\n}\n"),
            1
        );
        // Bad digits for the base are an error.
        assert_eq!(
            check_src(
                "module m;\nentity E { out y: uint[5]; }\nimpl E {\n  y = b\"01021\";\n}\n"
            ),
            1
        );
    }

    #[test]
    fn user_type_opts_into_condition_via_boolean() {
        // Without an `impl Boolean for State`, `if state` is rejected.
        let without = check_src(
            "module m;\nenum State { Idle, Run }\nentity E { out y: Bit; }\nimpl E {\n  let state: State;\n  if state {\n    y = '1';\n  }\n}\n",
        );
        assert_eq!(without, 1);

        // With it, the enum becomes usable as a condition.
        let with = check_src(
            "module m;\nenum State { Idle, Run }\nimpl Boolean for State {\n  fn as_bool(self) -> integer {\n    match self {\n      State::Idle => return 0,\n      _ => return 1,\n    }\n  }\n}\nentity E { out y: Bit; }\nimpl E {\n  let state: State;\n  if state {\n    y = '1';\n  }\n}\n",
        );
        assert_eq!(with, 0);
    }

    #[test]
    fn boolean_ops_reject_non_bit_types() {
        // `and`/`or`/`not` are boolean-per-bit: bit-derived / Boolean only.
        assert_eq!(
            check_src("module m;\nentity E { in a: real; in b: real; out y: real; }\nimpl E { y = a and b; }\n"),
            1,
            "`and` on real is rejected"
        );
        assert_eq!(
            check_src("module m;\nentity E { in a: uint[8]; in b: uint[8]; out y: uint[8]; }\nimpl E { y = a and b; }\n"),
            0,
            "`and` on a bit array is fine (per-bit, returns the array)"
        );
        // integer is a number, not bits — no boolean operators on it.
        assert_eq!(
            check_src("module m;\nentity E { in a: integer; in b: integer; out y: integer; }\nimpl E { y = a and b; }\n"),
            1,
            "`and` on integer variables is rejected"
        );
        // ...but a literal mask coerces to the bit operand's width.
        assert_eq!(
            check_src("module m;\nentity E { in a: uint[8]; out y: uint[8]; }\nimpl E { y = a and 15; }\n"),
            0,
            "`b and 15` (literal mask) is fine"
        );
        // comparison results are Bool, so boolean ops chain them.
        assert_eq!(
            check_src("module m;\nentity E { in a: uint[8]; in b: uint[8]; out y: Bool; }\nimpl E { y = (a > b) and (a != b); }\n"),
            0,
            "boolean ops on comparison results are fine"
        );
    }

    #[test]
    fn derived_struct_field_collision_errors() {
        // A field re-declaring an inherited one is rejected.
        let n = check_src(
            "module m;\nstruct A { x: Bit }\nstruct B : A { x: Bit }\n",
        );
        assert_eq!(n, 1, "duplicate inherited field");
        // A fresh field name is fine.
        let ok = check_src(
            "module m;\nstruct A { x: Bit }\nstruct B : A { y: Bit }\n",
        );
        assert_eq!(ok, 0);
    }

    #[test]
    fn field_adding_over_array_base_errors() {
        // Deriving fields over an array-shaped base is rejected; the bodyless
        // form is allowed.
        let bad = check_src(
            "module m;\nstruct Foo : Bit[] { parity: Bit }\n",
        );
        assert_eq!(bad, 1, "fields over array base");
        let ok = check_src("module m;\nstruct Word : Bit[];\n");
        assert_eq!(ok, 0, "bodyless array-derived is fine");
    }
}
