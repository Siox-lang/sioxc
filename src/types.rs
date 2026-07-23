//! Type system and kind checking for siox Phase 1 (spec Stage 4).
//!
//! Checks primitive digital types (`Bit`, `Logic`, `Bool`), integer widths
//! (`uint[N]`, `int[N]`), structs, enums, arrays/vectors, entity types,
//! directional views and bus modes, function/method signatures, trait bounds,
//! attribute value typing, and pattern typing.
//!
//! Key Phase 1 rules to enforce:
//! - system attributes `::event`/`::old` exist on every digital value
//!   (spec 3.9), and range attributes `::length/::range/::high/::low/::left/
//!   ::right/::direction` on range-like values (spec 3.23)
//! - `::ddt` is rejected as Phase-2 analogue syntax (spec Stage 4)
//! - no implicit broad conversions (spec 3.17): `uint[8]` !-> `uint[16]`
//! - cannot write to `in` ports inside an entity (spec 3.18 / code E-P004)
//! - `Logic` is not a bare condition without comparison (spec 3.16)

use std::collections::{HashMap, HashSet};

use crate::diag::{codes, Diagnostic, DiagnosticSink, Span};
use crate::resolve::{DefKind, Resolved};
use crate::syntax::ast::*;
use crate::syntax::Module;

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
    /// unbounded kernel `integer`. `family` is the declared family name when
    /// one is known (`uint`, `int`, a user `struct Byte : uint[8]`), carried
    /// purely so diagnostics name the real type; it is `None` for anonymous
    /// vectors (bit-string literals, concatenations) that have no family.
    ///
    /// The compiler still has NO *semantic* notion of `uint`/`int`: every
    /// family shares one operator surface (keyed `uint` in [`Self::ty_head`]),
    /// and width comparison ignores `family` — `family` never gates a check,
    /// it only labels.
    Vector { family: Option<String>, width: u32 },
    /// Named struct / enum / entity, keyed by its definition.
    Named(crate::resolve::DefId),
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
    /// `(struct, mode)` when this is a bus-mode port (`out Stream::Source`).
    mode: Option<(String, String)>,
}

/// The value type an attribute declaration expects (spec 3.5).
#[derive(Clone, Copy, PartialEq, Eq)]
enum AttrValueTy {
    Bool,
    Str,
    Integer,
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
    /// (operator trait, implementing type) -> (input type, output type).
    /// Multiple entries are overloads selected by the right operand.
    operator_sigs: HashMap<(String, String), Vec<(Option<String>, Option<String>)>>,
    operator_precedence: HashMap<String, (u8, Span)>,
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
    vector_families: HashSet<String>,
    /// Generic module fns: name -> (type params with bounds, value params).
    /// Bounds are checked at each call (spec: generic bounds).
    generic_fns: HashMap<String, (Vec<Param>, Vec<(String, Type)>)>,
    /// Literal suffix -> the type names defining it via `impl Suffix for T`
    /// (more than one is an ambiguity error at the use site).
    suffix_types: HashMap<String, Vec<String>>,
    /// `using X = T;` aliases, resolved through when typing.
    aliases: HashMap<String, Type>,
    /// (type head, method name) -> the method's declared return type, for
    /// typing method calls `recv.method(args)` (spec 3.20). Covers both
    /// inherent (`impl T`) and trait (`impl Tr for T`) impl methods.
    methods: HashMap<(String, String), Option<Type>>,
    /// Bus-mode per-field directions `(struct, mode) -> {field -> dir}` from
    /// `impl <dir> Struct::Mode { in a; out b; }` (spec 3.19), so a write to an
    /// `in` bus leaf can be rejected.
    mode_dirs: HashMap<(String, String), HashMap<String, Direction>>,
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
            ("precedence", &["impl"]),
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
            ("precedence", AttrValueTy::Integer),
        ] {
            attr_value_kinds.insert(name.to_string(), ty);
        }
        // Mirror of std::ops' `Boolean` impls: `Bit` and `Bool` can be used
        // directly as conditions (spec 3.16); a condition's truth is a `Bool`
        // (`true`/`false`), not an integer code.
        // `Logic` is omitted, so it still requires an explicit comparison.
        // ponytail: hardcoded shim — replace with real trait-impl lookup when
        // trait resolution lands, so user `impl Boolean for T` works from source.
        let mut trait_impls: HashMap<String, HashSet<String>> = HashMap::new();
        trait_impls.insert(
            "Boolean".to_string(),
            ["Bit", "Bool"].iter().map(|s| s.to_string()).collect(),
        );
        trait_impls.insert(
            "Not".to_string(),
            ["Bit", "Bool", "Logic"].iter().map(|s| s.to_string()).collect(),
        );
        Checker {
            sink,
            resolved,
            entities: HashMap::new(),
            attr_targets,
            attr_value_kinds,
            trait_impls,
            operator_sigs: HashMap::new(),
            operator_precedence: HashMap::new(),
            enum_variants: HashMap::new(),
            own_variants: HashMap::new(),
            enum_bases: HashMap::new(),
            structs: HashMap::new(),
            vector_families: HashSet::new(),
            generic_fns: HashMap::new(),
            suffix_types: HashMap::new(),
            aliases: HashMap::new(),
            methods: HashMap::new(),
            mode_dirs: HashMap::new(),
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
        // A field-less struct deriving from another vector family is itself one
        // (`struct Byte : uint[8]`); resolve that transitively before typing
        // ports, so such a type is treated as a numeric vector.
        self.resolve_transitive_vector_families();
        for m in modules {
            for item in &m.items {
                if let Item::Entity(e) = item {
                    let ports = e
                        .ports
                        .iter()
                        .map(|p| PortInfo {
                            name: p.name.text.clone(),
                            ty: self.ast_ty(&p.ty),
                            dir: p.dir,
                            mode: mode_key(&p.ty),
                        })
                        .collect();
                    self.entities.insert(e.name.text.clone(), ports);
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
                    Some("integer") => AttrValueTy::Integer,
                    _ => AttrValueTy::Other,
                };
                self.attr_value_kinds.insert(a.name.text.clone(), kind);
            }
            Item::Impl(im) => {
                // A bus-mode impl (`impl out Stream::Source { in a; out b; }`,
                // spec 3.19) records each field's direction.
                if im.mode_dir.is_some() {
                    if let Type::Mode { inner, .. } = &im.target {
                        if let Type::Path(p) = inner.as_ref() {
                            if p.segments.len() >= 2 {
                                let key = (p.segments[0].text.clone(), p.segments[1].text.clone());
                                let map = self.mode_dirs.entry(key).or_default();
                                for it in &im.items {
                                    if let ImplItem::ModeField { dir, name, .. } = it {
                                        map.insert(name.text.clone(), *dir);
                                    }
                                }
                            }
                        }
                    }
                }
                // Record every impl method by (type head, name) with its
                // declared return type, so `recv.method(args)` types (spec 3.20).
                if let Some(ty) = type_head_name(&im.target) {
                    for it in &im.items {
                        if let ImplItem::Fn(f) = it {
                            self.methods
                                .insert((ty.to_string(), f.name.text.clone()), f.ret.clone());
                        }
                    }
                }
                // Record `impl Trait for Type` so trait-driven checks (e.g.
                // conditions) can ask "does T implement Trait?".
                if let Some(tr) = &im.trait_ {
                    let trait_name = tr.segments.last().map(|s| s.text.clone());
                    let target = type_head_name(&im.target).map(|s| s.to_string());
                    if let (Some(mut t), Some(ty)) = (trait_name, target) {
                        let custom = if t == "custom" {
                            im.trait_args.first().and_then(|a| match a {
                                GenericArg::Positional(Expr::StrLit { text, .. }) => {
                                    Some(text.clone())
                                }
                                _ => None,
                            })
                        } else {
                            None
                        };
                        if let Some(symbol) = &custom {
                            t = symbol.clone();
                            let precedence = im.attrs.iter().find_map(|a| {
                                (a.name.segments.last().is_some_and(|n| n.text == "precedence"))
                                    .then_some(a)
                                    .and_then(|a| a.value.as_ref())
                                    .and_then(|v| match v {
                                        Expr::Int { text, span } => {
                                            text.parse::<u8>().ok().map(|p| (p, *span))
                                        }
                                        _ => None,
                                    })
                            });
                            match precedence {
                                Some((value, span)) => {
                                    if let Some((previous, previous_span)) =
                                        self.operator_precedence.get(symbol).copied()
                                    {
                                        if previous != value {
                                            self.sink.emit(
                                                Diagnostic::error(format!(
                                                    "custom operator `{symbol}` has precedence {value}, but another implementation uses {previous}"
                                                ))
                                                .with_code(codes::TYPE_MISMATCH)
                                                .at(span)
                                                .label(previous_span, "previous precedence declared here"),
                                            );
                                        }
                                    } else {
                                        self.operator_precedence
                                            .insert(symbol.clone(), (value, span));
                                    }
                                }
                                None => self.error(
                                    codes::TYPE_MISMATCH,
                                    im.span,
                                    format!(
                                        "custom operator `{symbol}` requires `#[precedence = N]`"
                                    ),
                                ),
                            }
                        }
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
                        if crate::resolve::OPERATORS.contains(&t.as_str()) || custom.is_some() {
                            let offset = usize::from(custom.is_some());
                            let arg_name = |index: usize| {
                                im.trait_args.get(index + offset).and_then(|a| match a {
                                    GenericArg::Positional(Expr::Path(p)) => {
                                        p.segments.last().map(|s| s.text.clone())
                                    }
                                    _ => None,
                                })
                            };
                            let input = (t != "Not").then(|| arg_name(0)).flatten().or_else(|| {
                                if t == "Not" {
                                    return None;
                                }
                                im.items.iter().find_map(|item| match item {
                                    ImplItem::Fn(f) => f
                                        .params
                                        .iter()
                                        .find(|p| !p.is_self)
                                        .and_then(|p| p.ty.as_ref())
                                        .and_then(type_head_name)
                                        .map(str::to_string),
                                    _ => None,
                                })
                            });
                            let output = (if t == "Not" { arg_name(0) } else { arg_name(1) }).or_else(|| {
                                im.items.iter().find_map(|item| match item {
                                    ImplItem::Fn(f) => f
                                        .ret
                                        .as_ref()
                                        .and_then(type_head_name)
                                        .map(str::to_string),
                                    _ => None,
                                })
                            });
                            self.operator_sigs
                                .entry((t.clone(), ty.clone()))
                                .or_default()
                                .push((input, output));
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
                        Some("Logic" | "Bit" | "ULogic")
                    );
                if is_vec {
                    self.vector_families.insert(st.name.text.clone());
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

    /// Whether `name` is a bit-vector family (`struct F : Logic[]`),
    /// recognized by shape. There is no signedness — that lives in the
    /// family's operator impls.
    fn is_vector_family(&self, name: &str) -> bool {
        self.vector_families.contains(name)
    }

    /// Fixpoint: a field-less struct whose base array element is a bit scalar
    /// or an already-known vector family is itself a vector family, so
    /// `struct Byte : uint[8]` inherits uint's numeric nature.
    fn resolve_transitive_vector_families(&mut self) {
        loop {
            let mut changed = false;
            let names: Vec<String> = self.structs.keys().cloned().collect();
            for name in names {
                if self.vector_families.contains(&name) {
                    continue;
                }
                let Some((base, fields)) = self.structs.get(&name) else { continue };
                if !fields.is_empty() {
                    continue;
                }
                let elem: Option<String> = match base {
                    Some(Type::Indexed { base, .. }) => type_head_name(base).map(str::to_string),
                    Some(Type::Path(p)) => p.segments.last().map(|s| s.text.clone()),
                    _ => None,
                };
                let is_vec = matches!(elem.as_deref(), Some("Logic" | "Bit" | "ULogic"))
                    || elem.as_deref().is_some_and(|h| self.vector_families.contains(h));
                if is_vec {
                    self.vector_families.insert(name);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
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
            Item::Const(c) => {
                self.check_const_not_entity(c);
                self.check_expr(&c.value, sym);
            }
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
        let (dirs, sym) = self.impl_env(im);
        for a in &im.attrs {
            self.check_attr_target(a, "impl", type_head_name(&im.target));
            self.check_attr_value(a);
            if let Some(v) = &a.value {
                self.check_expr(v, &sym);
            }
        }
        for item in &im.items {
            match item {
                ImplItem::Const(c) => {
                    self.check_const_not_entity(c);
                    self.check_expr(&c.value, &sym);
                }
                ImplItem::Let(l) => {
                    self.require_let_annotation(l);
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
                ImplItem::Stmt(s) => self.check_stmt(s, &dirs, &sym),
            }
        }
    }

    /// Build the value environment for an impl body: the `in` ports (for the
    /// write check) and a name -> type table (ports + impl-level lets/consts).
    fn impl_env(&self, im: &ImplDecl) -> (PortDirs, HashMap<String, Ty>) {
        let mut illegal = HashSet::new();
        let mut plain_in_roots = HashSet::new();
        let mut sym = HashMap::new();
        if im.trait_.is_none() {
            if let Some(ports) = type_head_name(&im.target).and_then(|n| self.entities.get(n)) {
                for p in ports {
                    sym.insert(p.name.clone(), p.ty.clone());
                    if p.dir == Some(Direction::In) {
                        illegal.insert(p.name.clone());
                        // A *plain* (non-bus-mode) `in` port has no writable
                        // parts: driving a field/index of it is illegal too.
                        if p.mode.is_none() {
                            plain_in_roots.insert(p.name.clone());
                        }
                    }
                    // A bus-mode port contributes each `in` leaf (`bus.ready`),
                    // so driving it inside the entity is rejected (spec 3.19).
                    if let Some(dirs) = p.mode.clone().and_then(|k| self.mode_dirs.get(&k)) {
                        for (field, dir) in dirs {
                            if *dir == Direction::In {
                                illegal.insert(format!("{}.{field}", p.name));
                            }
                        }
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
        (PortDirs { illegal, plain_in_roots }, sym)
    }

    fn check_block(&mut self, b: &Block) {
        let (dirs, sym) = (PortDirs { illegal: HashSet::new(), plain_in_roots: HashSet::new() }, HashMap::new());
        for s in &b.stmts {
            self.check_stmt(s, &dirs, &sym);
        }
    }

    fn check_stmt(&mut self, s: &Stmt, dirs: &PortDirs, sym: &HashMap<String, Ty>) {
        match s {
            Stmt::Let(l) => {
                self.require_let_annotation(l);
                if let Some(v) = &l.value {
                    self.check_init(l.ty.as_ref(), v, sym);
                    self.check_expr(v, sym);
                }
            }
            Stmt::Assign { target, value, .. } => {
                self.check_write_target(target, dirs);
                self.check_assignment(target, value, sym);
                self.check_expr(target, sym);
                self.check_expr(value, sym);
            }
            Stmt::If(i) => self.check_if(i, dirs, sym),
            Stmt::Match(m) => {
                self.check_match_exhaustive(m, sym);
                self.check_unreachable_arms(m);
                self.check_expr(&m.scrutinee, sym);
                for arm in &m.arms {
                    for s in &arm.body.stmts {
                        self.check_stmt(s, dirs, sym);
                    }
                }
            }
            Stmt::For { range, body, .. } => {
                self.check_expr(range, sym);
                for s in &body.stmts {
                    self.check_stmt(s, dirs, sym);
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

    fn check_if(&mut self, i: &IfStmt, dirs: &PortDirs, sym: &HashMap<String, Ty>) {
        self.check_condition(&i.cond, sym);
        self.check_expr(&i.cond, sym);
        for s in &i.then.stmts {
            self.check_stmt(s, dirs, sym);
        }
        match i.else_.as_deref() {
            Some(ElseBranch::Block(b)) => {
                for s in &b.stmts {
                    self.check_stmt(s, dirs, sym);
                }
            }
            Some(ElseBranch::If(inner)) => self.check_if(inner, dirs, sym),
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

        // Collect the covered variant names, flattening or-patterns; a wildcard
        // (bare or inside an `|`) makes the match exhaustive.
        let mut covered: HashSet<String> = HashSet::new();
        for a in &m.arms {
            let (vars, wild) = pattern_covers(&a.pattern);
            if wild {
                return;
            }
            covered.extend(vars);
        }
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
            Ty::Vector { .. } => Some("uint".to_string()),
            Ty::Real => Some("real".to_string()),
            Ty::Char => Some("Char".to_string()),
            Ty::Named(id) => self.resolved.def(*id).map(|d| d.name.clone()),
            Ty::Array { .. } | Ty::Error => None,
        }
    }

    /// Spec 3.18: flag a write to an `in` port. Three shapes are illegal: the
    /// bare port (`a = ..`), an `in` bus-mode leaf (`bus.ready = ..`), and any
    /// field/index of a plain (non-bus) `in` port (`a[3] = ..`, `p.f = ..`).
    fn check_write_target(&mut self, target: &Expr, dirs: &PortDirs) {
        // The exact name for the bare / bus-leaf case.
        let exact = match target {
            Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
            Expr::Field { .. } => path_string(target),
            _ => None,
        };
        // The root name for a field/index write into a plain `in` port.
        let root = match target {
            Expr::Field { .. } | Expr::Index { .. } => target_root_name(target),
            _ => None,
        };
        let bad = exact.as_deref().filter(|n| dirs.illegal.contains(*n)).map(str::to_string).or_else(
            || root.filter(|r| dirs.plain_in_roots.contains(r)),
        );
        if let Some(name) = bad {
            self.sink.emit(
                Diagnostic::error(format!("cannot assign to input port `{name}`"))
                    .with_code(codes::WRITE_TO_INPUT_PORT)
                    .at(expr_span(target))
                    .help("input ports are read-only inside the entity; drive it from the instantiating scope"),
            );
        }
    }

    /// Spec 3.5: an attribute's value must match the type its declaration gives.
    fn check_attr_value(&mut self, a: &Attr) {
        let Some(value) = &a.value else { return };
        let name = a.name.segments.last().map(|s| s.text.as_str()).unwrap_or("");
        let expected = self.attr_value_kinds.get(name).copied();
        let ok = match expected {
            Some(AttrValueTy::Bool) => {
                matches!(value, Expr::Path(p) if p.segments.len() == 2 && p.segments[0].text == "Bool")
            }
            Some(AttrValueTy::Str) => matches!(value, Expr::StrLit { .. }),
            Some(AttrValueTy::Integer) => matches!(value, Expr::Int { .. }),
            // Unknown attribute (reported by resolve) or an `Other`-typed one.
            _ => true,
        };
        if !ok {
            let want = match expected {
                Some(AttrValueTy::Bool) => "a Bool",
                Some(AttrValueTy::Str) => "a string",
                Some(AttrValueTy::Integer) => "an integer",
                _ => "a different",
            };
            self.error(
                codes::INVALID_ATTR_VALUE_TYPE,
                expr_span(value),
                format!("attribute `{name}` expects {want} value"),
            );
        }
    }

    /// Phase 1 is type-strict: every `let` binding declares its type
    /// (`let x: T [= e]`), never inferring it from the value. A bare
    /// `let x = e` is rejected — including the old instance form
    /// `let dut = Sub { .. }`, which is now `let dut: Sub = { .. }`.
    fn require_let_annotation(&mut self, l: &LetDecl) {
        if l.ty.is_some() {
            return;
        }
        let mut diag = Diagnostic::error(format!(
            "`let {}` needs a type annotation",
            l.name.text
        ))
        .with_code(codes::MISSING_TYPE_ANNOTATION)
        .at(l.span);
        // Point at the clean form for the common instance case.
        if let Some(Expr::Construct { ty: Some(t), .. }) = &l.value {
            if let Some(head) = type_head_name(t) {
                diag = diag.help(format!("write `let {}: {} = {{ .. }};`", l.name.text, head));
            }
        } else {
            diag = diag.help(format!("write `let {}: <type> = ...;`", l.name.text));
        }
        self.sink.emit(diag);
    }

    /// An entity is a hardware instance, not a compile-time value, so it may be
    /// declared with `let` but never `const` (`const dut: Counter = ..`).
    fn check_const_not_entity(&mut self, c: &ConstDecl) {
        let Some(head) = type_head_name(&c.ty) else { return };
        // Use the resolved definition so a generic parameter that shadows an
        // entity name isn't misjudged.
        let is_entity = type_head_span(&c.ty)
            .and_then(|s| self.resolved.resolved(s))
            .and_then(|id| self.resolved.def(id))
            .map(|d| d.kind == DefKind::Entity)
            .unwrap_or_else(|| self.entities.contains_key(head));
        if is_entity {
            self.error(
                codes::CONST_ENTITY_INSTANCE,
                c.span,
                format!("`{head}` is an entity instance, not a constant — declare it with `let`"),
            );
        }
    }

    /// Spec 3.17: a `let name: T = e` initializer must be assignable to `T`.
    fn check_init(&mut self, decl_ty: Option<&Type>, value: &Expr, sym: &HashMap<String, Ty>) {
        let Some(t) = decl_ty else { return };
        self.check_value_range(t, value);
        let lhs = self.ast_ty(t);
        // `let x: Named = { .. }` is a construction (instance/struct literal),
        // not a data assignment: a positional/empty block lexes as a concat,
        // and a dotted one as a name-less construct. Either way it is checked
        // structurally by elaboration, not by initializer compatibility.
        if matches!(lhs, Ty::Named(_)) && matches!(value, Expr::Construct { .. } | Expr::Concat { .. })
        {
            return;
        }
        if !matches!(lhs, Ty::Error) && !self.assignable(&lhs, value, sym) {
            let rhs = self.type_of(value, sym);
            let mut diag = Diagnostic::error(format!(
                "cannot initialize {} with {} without an explicit conversion",
                ty_name(&lhs),
                ty_name(&rhs)
            ))
            .with_code(codes::TYPE_MISMATCH)
            .at(expr_span(value));
            if let Some(h) = strlit_help(&lhs, value) {
                diag = diag.help(h);
            }
            self.sink.emit(diag);
        }
    }

    /// Spec 3.17: the right-hand side of `target = value` must be assignable to
    /// the target's type. Only fires when the target type is known.
    fn check_assignment(&mut self, target: &Expr, value: &Expr, sym: &HashMap<String, Ty>) {
        let lhs = self.type_of(target, sym);
        if !matches!(lhs, Ty::Error) && !self.assignable(&lhs, value, sym) {
            let rhs = self.type_of(value, sym);
            let help = strlit_help(&lhs, value).unwrap_or_else(|| {
                format!("wrap it in a conversion, e.g. `{}(...)`", ty_name(&lhs))
            });
            self.sink.emit(
                Diagnostic::error(format!(
                    "cannot assign {} to {} without an explicit conversion",
                    ty_name(&rhs),
                    ty_name(&lhs)
                ))
                .with_code(codes::TYPE_MISMATCH)
                .at(expr_span(value))
                .help(help),
            );
        }
    }

    /// Whether `value` may be assigned to a target of type `lhs` without an
    /// explicit conversion. Integer and logic *literals* are polymorphic; an
    /// `Error` type on either side suppresses the check.
    /// Whether `id` is an enum declaring the character variant `ch`.
    fn enum_has_char_variant(&self, id: crate::resolve::DefId, ch: char) -> bool {
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
        let width = match callee {
            Expr::Index { base, index, .. } => {
                let head = match base.as_ref() {
                    Expr::Path(p) if p.segments.len() == 1 => p.segments[0].text.as_str(),
                    _ => return,
                };
                if !self.vector_families.contains(head) {
                    return;
                }
                match signed_lit(index) {
                    Some(w) => w,
                    None => return,
                }
            }
            Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == "resize" => {
                match args.get(1).and_then(signed_lit) {
                    Some(w) => w,
                    None => return,
                }
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
        // No signedness: a literal fits an N-bit vector if it lands in the
        // union of the unsigned (0..2^N) and signed (-2^(N-1)..) ranges.
        let hi = if width == 64 { i64::MAX } else { (1i64 << width) - 1 };
        let lo = -(1i64 << (width - 1));
        let fits = v >= lo && v <= hi;
        if !fits {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(site),
                format!("`{v}` does not fit in a {width}-bit vector"),
            );
        }
    }

    fn assignable(&self, lhs: &Ty, value: &Expr, sym: &HashMap<String, Ty>) -> bool {
        match value {
            // A numeric literal also initialises `real` (`.re = 10` is 10.0).
            Expr::Int { .. } => matches!(lhs, Ty::Vector { .. } | Ty::Integer | Ty::Real | Ty::Error),
            Expr::CharLit { ch, .. } => {
                // A character literal reads through its context type (spec:
                // type kernel): builtin scalars, `Char`, or a user enum with
                // a matching character variant (e.g. ULogic's 'Z').
                if let Ty::Named(id) = lhs {
                    return self.enum_has_char_variant(*id, *ch);
                }
                matches!(lhs, Ty::Bit | Ty::Logic | Ty::Char | Ty::Error)
            }
            // An if-expression is assignable if both branches are — so char
            // literals in the branches read through the target type
            // (`b: Bit = if c { '1' } else { '0' }`).
            Expr::IfExpr { then, els, .. } => {
                self.assignable(lhs, then, sym) && self.assignable(lhs, els, sym)
            }
            // `[a, b, c]` fills an array target: length must match and every
            // element must be assignable to the element type (element literals
            // read through it, as in an initialiser).
            Expr::Array { elems, .. } => match lhs {
                Ty::Array { elem, len } => {
                    elems.len() as u32 == *len && elems.iter().all(|e| self.assignable(elem, e, sym))
                }
                Ty::Error => true,
                _ => false,
            },
            // A string is a sequence of characters: assigned to a `Logic`-vector
            // it fills each element with the matching `std_ulogic` (like `b"…"`),
            // and assigned to an array of a char-enum each character is a variant
            // — a string of logic values *is* a logic array, no prefix needed.
            Expr::StrLit { text, .. } => {
                let n = text.chars().count() as u32;
                match lhs {
                    Ty::Vector { width, .. } => {
                        (*width == 0 || n == *width)
                            && text.chars().all(|c| "01ZXUWLH-".contains(c))
                    }
                    // A char-enum array (`Color[3] = "rgb"`): each char a variant.
                    Ty::Array { elem, len } if matches!(elem.as_ref(), Ty::Named(_)) => {
                        let Ty::Named(id) = elem.as_ref() else { unreachable!() };
                        (*len == 0 || n == *len)
                            && text.chars().all(|c| self.enum_has_char_variant(*id, c))
                    }
                    // `Char[]` (a `string`) and everything else keep the existing
                    // structural check.
                    _ => compatible(lhs, &self.type_of(value, sym)),
                }
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
                // The edge helpers are ordinary trait methods now, so the
                // compiler no longer treats them as attributes: an unknown
                // system attribute is just that.
                if matches!(attr.text.as_str(), "rising" | "falling" | "edge") {
                    self.error(
                        codes::UNKNOWN_NAME,
                        *span,
                        format!("unknown system attribute `::{}`", attr.text),
                    );
                }
                self.check_expr(base, sym);
            }
            Expr::Match { scrutinee, arms, .. } => {
                self.check_expr(scrutinee, sym);
                for arm in arms {
                    if let Some(v) = arm.value_expr() {
                        self.check_expr(v, sym);
                    }
                }
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
                    if let Some(owner) = self.ty_head(&t) {
                        let intrinsic_vector = matches!(t, Ty::Vector { .. });
                        if !intrinsic_vector
                            && !self
                                .trait_impls
                                .get("Not")
                                .is_some_and(|types| types.contains(&owner))
                        {
                            self.error(
                                codes::TYPE_MISMATCH,
                                *span,
                                format!("`not` needs an `impl Not<Output> for {owner}`"),
                            );
                        }
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
                    if matches!(lit.as_ref(), Expr::CharLit { .. })
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
                let op_str = crate::syntax::pretty::bin_op(op);
                if let BinOp::Custom { symbol, .. } = op {
                    let lhs_ty = self.type_of(lhs, sym);
                    let rhs_ty = self.type_of(rhs, sym);
                    let matching = self
                        .ty_head(&lhs_ty)
                        .zip(self.ty_head(&rhs_ty))
                        .is_some_and(|(owner, input)| {
                            self.operator_sigs
                                .get(&(symbol.clone(), owner))
                                .is_some_and(|sigs| {
                                    sigs.iter().any(|(declared, _)| {
                                        declared.as_deref() == Some(input.as_str())
                                            || declared.as_deref() == Some("Self")
                                    })
                                })
                        });
                    if !matching {
                        self.error(
                            codes::TYPE_MISMATCH,
                            *span,
                            format!(
                                "custom operator `{symbol}` has no implementation for these operand types"
                            ),
                        );
                    }
                }
                // The core boolean operators (`and`/`or`) are "boolean,
                // per bit": on a bit array they act element-wise and return
                // the same array, on `Bool` they are plain boolean. They are
                // only meaningful on Boolean and bit-derived types — never on
                // `real` or `Char`.
                if matches!(op_str, "and" | "or") {
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
                // Comparing an enum-valued operand (`Bit`/`Logic`/`Bool` or a
                // user `enum`) to a bare integer literal is almost always a
                // mistake: its values are written as char/variant literals
                // (`'1'`, `Idle`), and an integer silently compares the raw
                // discriminant (`b == 1` instead of `b == '1'`). Numeric
                // vectors (`uint`/`int`) legitimately compare to integers, so
                // they are excluded. (W-P008)
                if matches!(op_str, "==" | "!=") {
                    for (lit, other) in [(lhs, rhs), (rhs, lhs)] {
                        let is_int_lit =
                            matches!(lit.as_ref(), Expr::Int { text, .. } if !text.contains('.'));
                        if is_int_lit {
                            if let Some(name) = self.enum_operand_name(&self.type_of(other, sym)) {
                                let hint = match name.as_str() {
                                    "Bit" | "Logic" => "compare against a value literal, e.g. `== '1'`",
                                    "Bool" => "compare against `true`/`false`, or use the value directly",
                                    _ => "compare against a variant, e.g. `== Idle`",
                                };
                                self.warn(
                                    codes::SUSPICIOUS_LOGIC_COMPARE,
                                    *span,
                                    format!("comparing `{name}` to an integer literal"),
                                    hint,
                                );
                            }
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
                        let tr = crate::syntax::ast::op_trait_name(op_str).unwrap_or(op_str);
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
            Expr::Array { elems, .. } => {
                for e in elems {
                    self.check_expr(e, sym);
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
                // A binary bit string admits the full `std_ulogic` alphabet
                // (`0 1 Z X U W L H -`), not just `0`/`1`, so metavalues can be
                // written (`b"01X0"`); hex stays 2-value.
                let ok = !digits.is_empty()
                    && if *base == 'x' {
                        digits.chars().all(|c| c.is_ascii_hexdigit())
                    } else {
                        digits.chars().all(|c| "01ZXUWLH-".contains(c))
                    };
                if !ok {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        format!("invalid {} bit-string literal `{base}\"{digits}\"`",
                            if *base == 'x' { "hex" } else { "binary" }),
                    );
                }
            }
            Expr::Int { .. }
            | Expr::CharLit { .. }
            | Expr::StrLit { .. }
            | Expr::Path(_) => {}
        }
    }

    // --- type inference core ------------------------------------------------

    /// Best-effort type of an expression given the in-scope value table. Unknown
    /// or unsupported cases yield [`Ty::Error`], which suppresses dependent
    /// checks rather than producing a false positive.
    fn type_of(&self, e: &Expr, sym: &HashMap<String, Ty>) -> Ty {
        match e {
            // A numeric literal is `integer`, or `real` when it has a point.
            Expr::Int { text, .. } if text.contains('.') => Ty::Real,
            Expr::Int { .. } => Ty::Integer,
            // `if c { a } else { b }` takes its branches' type (the then arm;
            // branch-mismatch diagnostics ride on assignment compatibility).
            Expr::IfExpr { then, .. } => self.type_of(then, sym),
            // A match-expression takes its arms' common type (the first arm).
            Expr::Match { arms, .. } => arms
                .iter()
                .find_map(|a| a.value_expr())
                .map(|v| self.type_of(v, sym))
                .unwrap_or(Ty::Error),
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
                        .map(|i| Ty::Named(crate::resolve::DefId(i as u32)))
                        .unwrap_or(Ty::Error);
                }
                if suffix_scale(&suffix.text).is_some() { Ty::Integer } else { Ty::Error }
            }
            Expr::BitStrLit { base, digits, .. } => {
                Ty::Vector {
                    family: None,
                    width: digits.len() as u32 * if *base == 'x' { 4 } else { 1 },
                }
            }
            // A char literal defaults to `Char`; an annotation/target
            // overrides it (Bit/Logic/enum) via `assignable`.
            Expr::CharLit { .. } => Ty::Char,
            // A string literal is `string` = `Char[N]`.
            Expr::StrLit { text, .. } => {
                Ty::Array { elem: Box::new(Ty::Char), len: text.chars().count() as u32 }
            }
            Expr::Path(p) => {
                if p.segments.len() == 1 {
                    sym.get(&p.segments[0].text).cloned().unwrap_or(Ty::Error)
                } else {
                    // `Enum::Variant` has the enum's type, not the variant's.
                    // `Bool`'s variants (`true`/`false`, desugared to
                    // `Bool::true`) keep the primitive `Ty::Bool` so conditions
                    // and attrs that expect it are unaffected.
                    match self.resolved.resolved(p.span).and_then(|id| self.resolved.def(id)) {
                        Some(d) if d.kind == DefKind::EnumVariant => match d.parent {
                            Some(pid)
                                if self.resolved.def(pid).map(|p| p.name == "Bool").unwrap_or(false) =>
                            {
                                Ty::Bool
                            }
                            Some(pid) => Ty::Named(pid),
                            None => Ty::Error,
                        },
                        _ => self.named_ty(p.span),
                    }
                }
            }
            Expr::SysAttr { base, attr, .. } => match attr.text.as_str() {
                // `::event` is Bool; the edge helpers are `ClockLike` methods now.
                "event" => Ty::Bool,
                "old" => self.type_of(base, sym),
                "length" | "high" | "low" | "left" | "right" => Ty::Integer,
                "ascending" => Ty::Bool,
                _ => Ty::Error,
            },
            Expr::Binary { op, lhs, rhs, .. } => {
                if is_comparison(op) {
                    return Ty::Bool;
                }
                let lhs_ty = self.type_of(lhs, sym);
                let rhs_ty = self.type_of(rhs, sym);
                let op_str = crate::syntax::pretty::bin_op(op);
                let tr = crate::syntax::ast::op_trait_name(op_str).unwrap_or(op_str);
                if let (Some(owner), Some(input)) =
                    (self.ty_head(&lhs_ty), self.ty_head(&rhs_ty))
                {
                    if let Some((_, Some(output))) = self
                        .operator_sigs
                        .get(&(tr.to_string(), owner.clone()))
                        .and_then(|sigs| {
                            sigs.iter().find(|(declared, _)| {
                                declared.as_deref() == Some(input.as_str())
                                    || declared.as_deref() == Some("Self")
                            })
                        })
                    {
                        if output == "Self" || output == &owner {
                            return lhs_ty;
                        }
                        if output == &input {
                            return rhs_ty;
                        }
                        return self.ty_from_head(output);
                    }
                }
                if matches!(op, BinOp::Custom { .. }) {
                    return Ty::Error;
                }
                // An integer literal joins the other operand's numeric type
                // (`100 / r` with r: int[8] is an int[8], via the std
                // `impl Div<int> for integer`).
                if matches!(lhs_ty, Ty::Integer) {
                    if let r @ Ty::Vector { .. } = self.type_of(rhs, sym) {
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
                                let op_str = crate::syntax::pretty::bin_op(op);
                                let tr = crate::syntax::ast::op_trait_name(op_str)
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
            Expr::Unary { op: UnOp::Not, rhs, .. } => {
                let rhs_ty = self.type_of(rhs, sym);
                if let Some(owner) = self.ty_head(&rhs_ty) {
                    if let Some((_, Some(output))) = self
                        .operator_sigs
                        .get(&("Not".to_string(), owner.clone()))
                        .and_then(|sigs| sigs.first())
                    {
                        if output == "Self" || output == &owner {
                            return rhs_ty;
                        }
                        return self.ty_from_head(output);
                    }
                }
                rhs_ty
            }
            Expr::Unary { rhs, .. } => self.type_of(rhs, sym),
            // A name-less struct literal (`ty: None`) takes its type from the
            // assignment target, which `type_of` does not see here.
            Expr::Construct { ty, .. } => ty.as_ref().map(|t| self.ast_ty(t)).unwrap_or(Ty::Error),
            // A concatenation is an unsigned bit vector of unknown width.
            Expr::Concat { .. } => Ty::Vector { family: None, width: 0 },
            // An array literal: element type from the first element, length
            // from the count.
            Expr::Array { elems, .. } => {
                let elem = elems.first().map(|e| self.type_of(e, sym)).unwrap_or(Ty::Error);
                Ty::Array { elem: Box::new(elem), len: elems.len() as u32 }
            }
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
                        Some(_) => Ty::Vector { family: Some(head.to_string()), width: w },
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
                        let family = match args.first().map(|a| self.type_of(a, sym)) {
                            Some(Ty::Vector { family, .. }) => family,
                            _ => None,
                        };
                        Ty::Vector { family, width: w }
                    }
                    _ => Ty::Error,
                },
                // A method call `recv.method(args)` types as the method's
                // declared return type (spec 3.20); the receiver's type head
                // selects the impl. An unknown method or a `self`-only method
                // (no return) is opaque (`Error` suppresses further checks).
                Expr::Field { base, field, .. } => {
                    let recv = self.type_of(base, sym);
                    match self
                        .ty_head(&recv)
                        .and_then(|h| self.methods.get(&(h, field.text.clone())))
                    {
                        Some(Some(ret)) => self.ast_ty(&ret.clone()),
                        _ => Ty::Error,
                    }
                }
                _ => Ty::Error,
            },
            Expr::Field { .. } | Expr::Index { .. } | Expr::Range { .. } => {
                Ty::Error
            }
        }
    }

    /// The type-head name used to key impl methods: a named type's def name,
    /// a base type's spelling, or `uint` for a bit vector.
    fn ty_head(&self, t: &Ty) -> Option<String> {
        Some(match t {
            Ty::Named(id) => self.resolved.def(*id)?.name.clone(),
            Ty::Bit => "Bit".to_string(),
            Ty::Logic => "Logic".to_string(),
            Ty::Bool => "Bool".to_string(),
            Ty::Char => "Char".to_string(),
            Ty::Real => "real".to_string(),
            Ty::Integer => "integer".to_string(),
            Ty::Vector { .. } => "uint".to_string(),
            _ => return None,
        })
    }

    fn ty_from_head(&self, name: &str) -> Ty {
        match name {
            "Bit" => Ty::Bit,
            "Logic" => Ty::Logic,
            "Bool" => Ty::Bool,
            "Char" => Ty::Char,
            "integer" => Ty::Integer,
            "real" => Ty::Real,
            name if self.is_vector_family(name) => {
                Ty::Vector { family: Some(name.to_string()), width: 0 }
            }
            name => self
                .resolved
                .defs()
                .iter()
                .position(|d| d.name == name)
                .map(|i| Ty::Named(crate::resolve::DefId(i as u32)))
                .unwrap_or(Ty::Error),
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
                    // The *first* index on a vector family sets its width
                    // (`uint[8]`). A *second* index makes an array of those
                    // vectors (`uint[8][4]` = 4 elements, each 8 wide).
                    Ty::Vector { family, width: 0 } => Ty::Vector { family, width },
                    v @ Ty::Vector { .. } => Ty::Array { elem: Box::new(v), len: width },
                    other => Ty::Array { elem: Box::new(other), len: width },
                }
            }
            Type::Generic { base, .. } => self.ast_ty(base),
            Type::Mode { inner, .. } => self.ast_ty(inner),
        }
    }

    /// A resolved type-name span as a `Ty`. A **type parameter** (`T` in a
    /// generic entity/struct/impl) is opaque, so it types as `Error` — it
    /// suppresses the assignment/type checks that can't be meaningful until the
    /// parameter is bound at elaboration.
    fn named_ty(&self, span: Span) -> Ty {
        match self.resolved.resolved(span) {
            Some(id) if self.resolved.def(id).map(|d| d.kind) == Some(DefKind::Param) => Ty::Error,
            Some(id) => Ty::Named(id),
            None => Ty::Error,
        }
    }

    fn path_ty(&self, p: &Path) -> Ty {
        if p.segments.len() == 1 {
            match p.segments[0].text.as_str() {
                "Bit" => Ty::Bit,
                "Logic" => Ty::Logic,
                "Bool" => Ty::Bool,
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
                    // A bit-vector family (`struct F : Logic[]`): width applies
                    // via `F[N]` (ast_ty's Indexed).
                    None if self.is_vector_family(name) => {
                        Ty::Vector { family: Some(name.to_string()), width: 0 }
                    }
                    None => self.named_ty(p.span),
                },
            }
        } else {
            self.named_ty(p.span)
        }
    }

    fn error(&mut self, code: &'static str, span: Span, msg: String) {
        self.sink.emit(Diagnostic::error(msg).with_code(code).at(span));
    }

    fn warn(&mut self, code: &'static str, span: Span, msg: String, help: &str) {
        self.sink.emit(Diagnostic::warning(msg).with_code(code).at(span).help(help.to_string()));
    }

    /// The enum name if `t` is a symbolic enum value (`Bit`/`Logic`/`Bool` or a
    /// user `enum`) — the types whose values are written as char/variant
    /// literals, not numbers. `None` for numerics (`uint`/`int`/`integer`/
    /// `real`), `Char`, and non-enums.
    fn enum_operand_name(&self, t: &Ty) -> Option<String> {
        match t {
            Ty::Bit => Some("Bit".into()),
            Ty::Logic => Some("Logic".into()),
            Ty::Bool => Some("Bool".into()),
            Ty::Named(id) => {
                let d = self.resolved.def(*id)?;
                matches!(d.kind, DefKind::Enum).then(|| d.name.clone())
            }
            _ => None,
        }
    }
}

/// The base name of a type (`Counter<W>` -> `Counter`, `out S::Source` -> `S`).
/// A pattern's covered enum-variant names and whether it contains a wildcard,
/// flattening or-patterns (`A | B` covers both; `A | _` is a wildcard).
fn pattern_covers(p: &Pattern) -> (Vec<String>, bool) {
    match p {
        Pattern::Wildcard => (Vec::new(), true),
        Pattern::Path(pp) if pp.segments.len() >= 2 => (vec![pp.segments[1].text.clone()], false),
        Pattern::Or { alts, .. } => {
            let mut vars = Vec::new();
            let mut wild = false;
            for a in alts {
                let (v, w) = pattern_covers(a);
                vars.extend(v);
                wild |= w;
            }
            (vars, wild)
        }
        _ => (Vec::new(), false),
    }
}

/// The span of a type's head name segment (for resolving its definition).
fn type_head_span(ty: &Type) -> Option<Span> {
    match ty {
        Type::Path(p) => p.segments.first().map(|s| s.span),
        Type::Generic { base, .. } | Type::Indexed { base, .. } => type_head_span(base),
        Type::Mode { inner, .. } => type_head_span(inner),
    }
}

fn type_head_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Path(p) => p.segments.first().map(|s| s.text.as_str()),
        Type::Generic { base, .. } | Type::Indexed { base, .. } => type_head_name(base),
        Type::Mode { inner, .. } => type_head_name(inner),
    }
}

/// A dotted path string for a write target: `Expr::Path` or a `Field` chain
/// (`bus.ready` -> "bus.ready").
fn path_string(e: &Expr) -> Option<String> {
    match e {
        Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        Expr::Field { base, field, .. } => Some(format!("{}.{}", path_string(base)?, field.text)),
        _ => None,
    }
}

/// The leftmost identifier of a field/index access chain (`bus.ready` -> `bus`,
/// `a[3]` -> `a`, `p.f.g` -> `p`), for the plain-input-port write check.
fn target_root_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        Expr::Field { base, .. } | Expr::Index { base, .. } => target_root_name(base),
        _ => None,
    }
}

/// Port-direction facts for the write-to-input check within one impl.
struct PortDirs {
    /// Names whose write is illegal exactly: a bare `in` port, or an `in`
    /// bus-mode leaf (`bus.ready`).
    illegal: HashSet<String>,
    /// Plain (non-bus-mode) `in` ports — writing *any* field/index of one is
    /// illegal too (it has no writable parts).
    plain_in_roots: HashSet<String>,
}

/// The `(struct, mode)` of a bus-mode type (`out Stream::Source` ->
/// `("Stream", "Source")`), for looking up per-leaf directions.
fn mode_key(ty: &Type) -> Option<(String, String)> {
    if let Type::Mode { inner, mode, .. } = ty {
        // Generic form `Stream<..>::Source`.
        if let Some(m) = mode {
            return Some((type_head_name(inner)?.to_string(), m.text.clone()));
        }
        // Plain form `Stream::Source` (two-segment inner path).
        if let Type::Path(p) = inner.as_ref() {
            if p.segments.len() >= 2 {
                return Some((p.segments[0].text.clone(), p.segments[1].text.clone()));
            }
        }
    }
    None
}

/// Width of a bracketed type index when it is a literal (`uint[8]` -> 8);
/// otherwise `0`, meaning "parametric / not yet known".
fn width_of(index: &Expr) -> u32 {
    match index {
        Expr::Int { text, .. } => text.parse().unwrap_or(0),
        _ => 0,
    }
}

fn is_comparison(op: &BinOp) -> bool {
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
        // Width-only: `family` never gates compatibility (`uint[8]` and
        // `int[8]` are interchangeable — signedness lives in operator impls).
        (Vector { width: a, .. }, Vector { width: b, .. }) => *a == 0 || *b == 0 || a == b,
        (Named(a), Named(b)) => a == b,
        // Whole-array copy: same element type, matching length (0 = unset).
        (Array { elem: ea, len: la }, Array { elem: eb, len: lb }) => {
            compatible(ea, eb) && (*la == 0 || *lb == 0 || la == lb)
        }
        _ => false,
    }
}

/// When a string literal (`"c"`) is used where a character, logic scalar, or
/// bit vector is expected, explain that `"..."` is a *string* (a `Char` array)
/// and point at the right form: `'c'` for a single value, `b"..."` for a bit
/// vector. Assigning a string to a `Char` array is fine, so no hint there.
fn strlit_help(lhs: &Ty, value: &Expr) -> Option<String> {
    let Expr::StrLit { text, .. } = value else { return None };
    match lhs {
        // A string *is* a Char array — that assignment is correct.
        Ty::Array { elem, .. } if matches!(**elem, Ty::Char) => None,
        Ty::Bit | Ty::Logic | Ty::Char | Ty::Bool | Ty::Named(_) => Some(if text.chars().count() == 1 {
            format!("`\"{text}\"` is a string; for a single {} value use a character literal `'{text}'`", ty_name(lhs))
        } else {
            format!("`\"{text}\"` is a string (a `Char` array); a {} is one character, written `'c'`", ty_name(lhs))
        }),
        Ty::Vector { .. } => Some(format!(
            "`\"{text}\"` is a string; for a bit vector use a bit-string literal `b\"{text}\"` (binary) or `x\"...\"` (hex)"
        )),
        // A logic/bit array: strings don't build one.
        Ty::Array { .. } => Some(format!(
            "`\"{text}\"` is a string (a `Char` array); build the array from element values, e.g. `{{'0', '1', ...}}`, or use a bit vector `b\"{text}\"`"
        )),
        _ => None,
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
        // Name the real family when one is known (`int[8]`, `Byte`), falling
        // back to `uint` for anonymous vectors (bit-string literals, concats).
        Ty::Vector { family, width: 0 } => family.clone().unwrap_or_else(|| "uint".to_string()),
        Ty::Vector { family, width: w } => {
            format!("{}[{w}]", family.as_deref().unwrap_or("uint"))
        }
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
        | Expr::CharLit { span, .. }
        | Expr::StrLit { span, .. }
        | Expr::Field { span, .. }
        | Expr::SysAttr { span, .. }
        | Expr::IfExpr { span, .. }
        | Expr::Match { span, .. }
        | Expr::Index { span, .. }
        | Expr::Range { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Binary { span, .. }
        | Expr::Call { span, .. }
        | Expr::Construct { span, .. }
        | Expr::Concat { span, .. }
        | Expr::Array { span, .. } => *span,
        Expr::Path(p) => p.span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::FileId;

    const VEC: &str = "\nstruct uint : Logic[];\nstruct int : Logic[];\n";

    fn check_src(src: &str) -> usize {
        let src = format!("{src}{VEC}");
        let src = src.as_str();
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), src, &mut sink);
        assert_eq!(sink.error_count(), 0, "source failed to parse:\n{src}");
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
        let parse_resolve_errors = sink.error_count();
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        sink.error_count() - parse_resolve_errors
    }

    fn diag_codes(src: &str) -> Vec<String> {
        let src = format!("{src}{VEC}");
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        sink.diagnostics().iter().map(|d| format!("{:?}", d.code)).collect()
    }

    #[test]
    fn suspicious_logic_compare_warns_on_integer_literal() {
        let warns = |src: &str| diag_codes(src).iter().any(|c| c.contains("W-P008"));
        // Bit / Logic / enum vs a bare integer literal → W-P008.
        assert!(
            warns("module m;\nentity E { in b: Bit; out y: Bit; }\nimpl E { y = if b == 1 { '1' } else { '0' }; }\n"),
            "Bit == 1 should warn"
        );
        assert!(
            warns("module m;\nenum State { Idle, Run }\nentity E { out y: Bit; }\nimpl E { let s: State; y = if s == 0 { '1' } else { '0' }; }\n"),
            "enum == 0 should warn"
        );
        // Numeric vector vs integer, and Bit vs a value literal → no warning.
        assert!(
            !warns("module m;\nentity E { in a: uint[8]; out y: Bit; }\nimpl E { y = if a == 5 { '1' } else { '0' }; }\n"),
            "uint == 5 must not warn"
        );
        assert!(
            !warns("module m;\nentity E { in b: Bit; out y: Bit; }\nimpl E { y = if b == '1' { '1' } else { '0' }; }\n"),
            "Bit == '1' must not warn"
        );
    }

    #[test]
    fn rejects_write_to_input_bus_leaf() {
        // Driving an `in` leaf of a bus-mode port (`bus.ready` in the Source
        // view) is a write to an input (spec 3.19) — a clear E-P004.
        let bad = check_src(
            "module m;\n\
             struct S { valid: Bit, ready: Bit, }\n\
             impl out S::Source { out valid; in ready; }\n\
             entity P { bus: out S::Source; }\n\
             impl P { bus.valid = '1'; bus.ready = '1'; }\n",
        );
        assert_eq!(bad, 1, "driving the `in` leaf bus.ready must error");

        // Driving only the `out` leaves is fine.
        let ok = check_src(
            "module m;\n\
             struct S { valid: Bit, ready: Bit, }\n\
             impl out S::Source { out valid; in ready; }\n\
             entity P { bus: out S::Source; out r: Bit; }\n\
             impl P { bus.valid = '1'; r = bus.ready; }\n",
        );
        assert_eq!(ok, 0, "driving out leaves + reading in leaves is fine");
    }

    #[test]
    fn method_return_type_propagates() {
        // A method returning `Logic` used directly as a condition must error
        // (Logic isn't Boolean), proving the return type flows into checks.
        let bad = "module m;\n\
            struct S { v: Logic, }\n\
            impl S { fn ready(self) -> Logic { return self.v; } }\n\
            entity E { out o: Logic; }\n\
            impl E { let s: S; if s.ready() { o = '1'; } }\n";
        assert_eq!(check_src(bad), 1, "Logic-returning method as a condition should error");

        // A `Bool`-returning method is a valid condition — no error.
        let good = "module m;\n\
            struct S { v: Logic, }\n\
            impl S { fn ready(self) -> Bool { return true; } }\n\
            entity E { out o: Logic; }\n\
            impl E { let s: S; if s.ready() { o = '1'; } }\n";
        assert_eq!(check_src(good), 0, "Bool-returning method as a condition should pass");
    }

    #[test]
    fn string_literal_gets_a_targeted_hint() {
        let sp = crate::diag::Span::new(FileId(0), 0..1);
        let s = |t: &str| Expr::StrLit { text: t.to_string(), span: sp };
        // A scalar Logic/Bit/Char points at the character literal.
        let h = strlit_help(&Ty::Logic, &s("0")).unwrap();
        assert!(h.contains("'0'"), "{h}");
        // A bit vector points at the bit-string literal.
        let h = strlit_help(&Ty::Vector { family: None, width: 4 }, &s("0101")).unwrap();
        assert!(h.contains("b\"0101\""), "{h}");
        // Assigning a string to a Char array is correct — no hint.
        let str_ty = Ty::Array { elem: Box::new(Ty::Char), len: 2 };
        assert!(strlit_help(&str_ty, &s("hi")).is_none());
    }

    #[test]
    fn vector_names_its_real_family() {
        // A known family displays by name; anonymous vectors fall back to uint.
        let int8 = Ty::Vector { family: Some("int".to_string()), width: 8 };
        assert_eq!(ty_name(&int8), "int[8]");
        let byte = Ty::Vector { family: Some("Byte".to_string()), width: 0 };
        assert_eq!(ty_name(&byte), "Byte");
        let anon = Ty::Vector { family: None, width: 4 };
        assert_eq!(ty_name(&anon), "uint[4]");
        // Width still ignores the family: uint[8] and int[8] stay compatible.
        assert!(compatible(&int8, &Ty::Vector { family: None, width: 8 }));
    }

    /// The number of warnings with a given code emitted while checking `src`.
    fn warnings(src: &str, code: &str) -> usize {
        let src = format!("{src}{VEC}");
        let src = src.as_str();
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), src, &mut sink);
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
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
            "module m;\nentity E { in clk: Bit; out q: Bit; }\nimpl E {\n  if clk.rising() {\n    q = clk::old;\n  }\n}\n",
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
    fn rejects_write_to_plain_input_field_or_index() {
        // A field/index of a *plain* `in` port is read-only too.
        let errors = check_src(
            "module m;\nstruct P { x: Bit }\nentity E { in a: Bit; in p: P; out y: Bit; }\n\
             impl E {\n  a = '1';\n  p.x = '1';\n  y = a;\n}\n",
        );
        assert_eq!(errors, 2, "bare `a` and field `p.x` are both rejected");
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
            "module m;\nentity E { out count: uint[8]; out q: Bit; out clk: Bit; }\nimpl E {\n  let value: uint[8] = 0;\n  count = value;\n  q = '1';\n  clk = '0';\n}\n",
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
                "module m;\nentity E { out y: uint[8]; }\nimpl E {\n  let t: integer = 10ns;\n  let f: integer = 100MHz;\n  y = x\"AB\";\n}\n"
            ),
            0
        );
        // An unknown suffix is an error.
        assert_eq!(
            check_src("module m;\nentity E { out y: Bit; }\nimpl E {\n  let c: integer = 5i;\n  y = '0';\n}\n"),
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
            "module m;\nenum State { Idle, Run }\nimpl Boolean for State {\n  fn as_bool(self) -> Bool {\n    match self {\n      State::Idle => return false,\n      _ => return true,\n    }\n  }\n}\nentity E { out y: Bit; }\nimpl E {\n  let state: State;\n  if state {\n    y = '1';\n  }\n}\n",
        );
        assert_eq!(with, 0);
    }

    #[test]
    fn char_literal_defaults_to_char_but_takes_annotated_type() {
        // Bare: '0' is a Char.  Annotated / if-expr context: it takes the
        // target type (Bit/Logic), including through an if-expression.
        assert_eq!(
            check_src("module m;\nentity E { out y: Bit; }\nimpl E { y = '0'; }\n"),
            0,
            "'0' assigns to a Bit output"
        );
        assert_eq!(
            check_src("module m;\nentity E { out y: Logic; }\nimpl E { y = '1'; }\n"),
            0,
            "'1' assigns to a Logic output"
        );
        assert_eq!(
            check_src("module m;\nentity E { in c: Bit; out y: Bit; }\nimpl E { y = if c { '1' } else { '0' }; }\n"),
            0,
            "char literals in if-expr branches read through the Bit target"
        );
    }

    #[test]
    fn literals_default_to_their_core_types() {
        let ty = |src: &str| {
            let mut sink = DiagnosticSink::new();
            let m = crate::syntax::parse_module(FileId(0), src, &mut sink);
            let r = crate::resolve::resolve(std::slice::from_ref(&m), &mut sink);
            let c = Checker::new(&mut sink, &r);
            c.type_of(&value_expr(&m), &HashMap::new())
        };
        // helper: the value in `impl E { y = <value>; }`
        fn value_expr(m: &crate::syntax::Module) -> Expr {
            for item in &m.items {
                if let Item::Impl(im) = item {
                    for it in &im.items {
                        if let ImplItem::Stmt(Stmt::Assign { value, .. }) = it {
                            return value.clone();
                        }
                    }
                }
            }
            panic!("no assignment");
        }
        assert!(matches!(ty("module m;\nimpl E { y = 42; }\n"), Ty::Integer));
        assert!(matches!(ty("module m;\nimpl E { y = 3.14; }\n"), Ty::Real));
        assert!(matches!(ty("module m;\nimpl E { y = '0'; }\n"), Ty::Char));
        assert!(matches!(ty("module m;\nimpl E { y = \"abc\"; }\n"), Ty::Array { .. }));
        // `true`/`false` desugar to `Bool::true`/`Bool::false`, so std's `Bool`
        // enum must be in scope for them to resolve and type as `Ty::Bool`.
        assert!(matches!(
            ty("module m;\nenum Bool { false, true }\nimpl E { y = true; }\n"),
            Ty::Bool
        ));
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
    fn logical_operator_template_controls_output_type() {
        let src = "module m;\n\
            enum Left { L }\n\
            enum Right { R }\n\
            enum Result { Yes }\n\
            impl And<Right, Result> for Left {\n\
              fn and(self, rhs: Right) -> Result { return Result::Yes; }\n\
            }\n\
            entity E { in a: Left; in b: Right; out y: Result; }\n\
            impl E { y = a and b; }\n";
        assert_eq!(check_src(src), 0, "And's Output parameter types the expression");
    }

    #[test]
    fn custom_operator_selects_input_and_output_templates() {
        let ok = "module m;\n\
            attr precedence: integer for impl;\n\
            trait custom<S, I, O> { fn apply(self, rhs: I) -> O; }\n\
            enum Left { L } enum Right { R } enum Result { Yes }\n\
            #[precedence = 45]\n\
            impl custom<\"merge\", Right, Result> for Left {\n\
              fn apply(self, rhs: Right) -> Result { return Result::Yes; }\n\
            }\n\
            entity E { in a: Left; in b: Right; out y: Result; }\n\
            impl E { y = a merge b; }\n";
        assert_eq!(check_src(ok), 0);

        let bad = ok.replace("in b: Right", "in b: Left");
        assert_eq!(check_src(&bad), 1, "the Input template participates in overload selection");
    }

    #[test]
    fn custom_operator_precedence_is_required_and_consistent() {
        let header = "module m;\nattr precedence: integer for impl;\n\
            trait custom<S, I, O> { fn apply(self, rhs: I) -> O; }\n\
            enum A { A0 } enum B { B0 }\n";
        let missing = format!(
            "{header}impl custom<\"join\", A, A> for A {{ fn apply(self, rhs: A) -> A {{ return self; }} }}\n"
        );
        assert_eq!(check_src(&missing), 1);

        let conflict = format!(
            "{header}\
             #[precedence = 40] impl custom<\"join\", A, A> for A {{ fn apply(self, rhs: A) -> A {{ return self; }} }}\n\
             #[precedence = 30] impl custom<\"join\", B, B> for B {{ fn apply(self, rhs: B) -> B {{ return self; }} }}\n"
        );
        assert_eq!(check_src(&conflict), 1);
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
