//! Type system and kind checking for siox Phase 1 (spec Stage 4).
//!
//! Checks std-defined digital types (`Bit`, `Logic`, `Bool`), indexed widths
//! (`unsigned[N]`, `signed[N]`), structs, enums, arrays, entity types,
//! directional views and bus modes, function/method signatures, trait bounds,
//! attribute value typing, and pattern typing.
//!
//! Key Phase 1 rules to enforce:
//! - system attributes `::event`/`::old` exist on every digital value
//!   (spec 3.9), and range attributes `::length/::range/::high/::low/::left/
//!   ::right/::direction` on range-like values (spec 3.23)
//! - `::ddt` is rejected as Phase-2 analogue syntax (spec Stage 4)
//! - no implicit broad conversions (spec 3.17): `unsigned[8]` !-> `unsigned[16]`
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
    /// The kernel base type `integer` — an unbounded mathematical number, NOT
    /// a bit collection. Coerces to/from bit vectors, supports arithmetic and
    /// comparison, but has NO per-bit boolean operators (unlike unsigned/signed).
    Integer,
    /// The kernel base type `real` (f64 in simulation).
    Real,
    /// The kernel character scalar. Unlike library enums, its value set is the
    /// full Unicode scalar range rather than a finite declaration.
    Char,
    /// Named struct / enum / entity, keyed by its definition.
    Named(crate::resolve::DefId),
    /// The single indexed collection representation. Plain arrays have no
    /// family; a library newtype over an unconstrained array (`unsigned`,
    /// `signed`, or a user equivalent) retains its nominal family for method
    /// and operator dispatch while using the same element/length shape.
    Array {
        elem: Box<Ty>,
        len: u32,
        family: Option<String>,
    },
    /// Placeholder for an as-yet-unresolved/error type.
    Error,
}

impl Ty {
    /// Concrete storage width known at Stage 4. Named types need declaration
    /// metadata and therefore return `None` here.
    pub fn bit_width(&self) -> Option<u32> {
        match self {
            Ty::Integer | Ty::Real => Some(64),
            Ty::Char => Some(32),
            Ty::Array { elem, len, family } => {
                if family.is_some() {
                    (*len != 0).then_some(*len)
                } else {
                    elem.bit_width()?.checked_mul(*len)
                }
            }
            Ty::Named(_) | Ty::Error => None,
        }
    }
}

/// Outcome of type checking: a type for every expression/signal, ready for the
/// elaborator and IR lowering.
#[derive(Clone, Default)]
pub struct Typed {
    expr_types: HashMap<Span, Ty>,
}

impl Typed {
    /// Checked type of the expression covering this exact source span.
    pub fn expr_type(&self, span: Span) -> Option<&Ty> {
        self.expr_types.get(&span)
    }

    pub fn expr_types(&self) -> &HashMap<Span, Ty> {
        &self.expr_types
    }
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
/// conversions (`unsigned[8]` !-> `unsigned[16]`) and method-call resolution.
pub fn check(modules: &[Module], resolved: &Resolved, sink: &mut DiagnosticSink) -> Typed {
    let mut checker = Checker::new(sink, resolved);
    checker.collect(modules);
    for m in modules {
        for item in &m.items {
            checker.check_item(item);
        }
    }
    checker.finish()
}

/// Analogue (Phase-2) system attributes that must error rather than be silently
/// accepted in Phase 1 (spec Stage 4). The full analogue set is a Phase-2
/// concern; `::ddt` is kept here only as the guard the spec calls out.
const PHASE2_ATTRS: &[&str] = &["ddt"];

/// Every system attribute the compiler implements (spec 3.9 / 3.23). Anything
/// else after a `'` is reported rather than lowered: an unrecognized one used
/// to pass every stage and become an `Unknown` in the IR, surfacing only as
/// "no engine can run this design" with nothing naming the attribute.
const SYS_ATTRS: &[&str] = &[
    "event",
    "old",
    "length",
    "high",
    "low",
    "left",
    "right",
    "ascending",
];

/// A port as seen by the checker: its name, resolved type, and direction.
struct PortInfo {
    name: String,
    ty: Ty,
    dir: Option<Direction>,
    /// Named directional view when this port is view-typed.
    view: Option<String>,
    /// Declared bounds of a ranged numeric (`integer<0..10>`); `Ty` does not
    /// carry them, and an out-of-range constant otherwise wrapped at store
    /// time so the runtime range assert could never see it.
    range: Option<(i64, i64)>,
}

/// The value type an attribute declaration expects (spec 3.5).
#[derive(Clone, Copy, PartialEq, Eq)]
enum AttrValueTy {
    Bool,
    Str,
    Integer,
    Other,
}

type OperatorSignatures = HashMap<(String, String), Vec<(Option<String>, Option<String>)>>;
type GenericFnSignature = (Vec<Param>, Vec<(String, Type)>);
type ImplEnvironment = (PortDirs, HashMap<String, Ty>, HashMap<String, (i64, i64)>);

struct Checker<'a> {
    /// Entities carrying `#[test]`: testbenches, where the stimulus
    /// primitives (`await`, `assert!`, `print!`, `warn!`) are meaningful.
    test_entities: HashSet<String>,
    /// Whether the statements being checked belong to a testbench impl.
    in_testbench: std::cell::Cell<bool>,
    /// Whether the statements being checked are a function body. `return`
    /// belongs to a function; in hardware statement position there is nothing
    /// to return from, and lowering silently dropped it.
    in_fn_body: std::cell::Cell<bool>,
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
    /// Trait name -> the methods an implementation must provide (those the
    /// trait declares without a default body). Spec 3.20: a trait is a
    /// compile-time contract, so a partial impl is an error.
    trait_required: HashMap<String, Vec<String>>,
    /// (operator trait, implementing type) -> (input type, output type).
    /// Multiple entries are overloads selected by the right operand.
    operator_sigs: OperatorSignatures,
    /// (`Index`/`IndexAssign`, target) -> (index type, value/output type).
    index_sigs: OperatorSignatures,
    operator_precedence: HashMap<String, (u8, Span)>,
    /// Enum name -> its EFFECTIVE variant names (inherited + own).
    enum_variants: HashMap<String, Vec<String>>,
    /// Enum name -> only its own declared variants (pre-inheritance).
    own_variants: HashMap<String, Vec<String>>,
    /// Enum name -> the head name after `:` (a base enum or numeric repr).
    enum_bases: HashMap<String, String>,
    /// Struct name -> (derivation base, own field names) for inheritance.
    structs: HashMap<String, (Option<Type>, Vec<String>)>,
    /// View name -> underlying struct type.
    views: HashMap<String, Type>,
    /// Structs opting into packed numeric storage through `impl Vector`.
    vector_families: HashSet<String>,
    /// Packed family -> scalar element type, following nominal vector bases.
    vector_elements: HashMap<String, String>,
    /// Trait/operator keys implemented generically for an unconstrained array
    /// target (`impl<T: Tr> Tr for T[]`). A nominal Vector family may forward
    /// one of these only when its element type implements the same key.
    blanket_array_impls: HashMap<String, String>,
    /// Generic module fns: name -> (type params with bounds, value params).
    /// Bounds are checked at each call (spec: generic bounds).
    generic_fns: HashMap<String, GenericFnSignature>,
    /// Declared free function -> its parameter count, for call-arity checking.
    /// Covers module `fn`s and `extern "C"` declarations; runtime-provided std
    /// functions (rand/fs) have no declaration and are not listed.
    fn_arity: HashMap<String, usize>,
    /// Free-function name -> its declared parameter types. Arguments were
    /// checked for count but never for type, so a value of the wrong type was
    /// reinterpreted bit-for-bit at the call.
    fn_param_types: HashMap<String, Vec<Option<Type>>>,
    /// Literal suffix -> the type names defining it via `impl Suffix<sym, _>
    /// for T` (more than one is an ambiguity error at the use site).
    suffix_types: HashMap<String, Vec<String>>,
    /// Bit-string prefix (`x`, `o`) -> the type names defining it via
    /// `impl Prefix<sym, _> for T` (spec 3.24). std declares which prefixes
    /// exist; the compiler evaluates the known radix ones intrinsically.
    prefix_types: HashMap<String, Vec<String>>,
    /// `using X = T;` aliases, resolved through when typing.
    aliases: HashMap<String, Type>,
    /// Aliases currently being expanded, so a cycle (`using A = B; using B =
    /// A`) is caught instead of recursing until the stack overflows.
    expanding: std::cell::RefCell<HashSet<String>>,
    /// (type head, method name) -> the method's declared return type, for
    /// typing method calls `recv.method(args)` (spec 3.20). Covers both
    /// inherent (`impl T`) and trait (`impl Tr for T`) impl methods.
    methods: HashMap<(String, String), Option<Type>>,
    /// Named view -> per-field directions.
    view_dirs: HashMap<String, HashMap<String, Direction>>,
    /// Persistent Stage-4 facts keyed by the AST expression's stable span.
    expr_types: std::cell::RefCell<HashMap<Span, Ty>>,
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
            attr_targets.insert(
                name.to_string(),
                targets.iter().map(|s| s.to_string()).collect(),
            );
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
        Checker {
            test_entities: HashSet::new(),
            in_testbench: std::cell::Cell::new(false),
            in_fn_body: std::cell::Cell::new(false),
            sink,
            resolved,
            entities: HashMap::new(),
            attr_targets,
            attr_value_kinds,
            trait_impls: HashMap::new(),
            trait_required: HashMap::new(),
            operator_sigs: HashMap::new(),
            index_sigs: HashMap::new(),
            operator_precedence: HashMap::new(),
            enum_variants: HashMap::new(),
            own_variants: HashMap::new(),
            enum_bases: HashMap::new(),
            structs: HashMap::new(),
            views: HashMap::new(),
            vector_families: HashSet::new(),
            vector_elements: HashMap::new(),
            blanket_array_impls: HashMap::new(),
            generic_fns: HashMap::new(),
            fn_arity: HashMap::new(),
            fn_param_types: HashMap::new(),
            suffix_types: HashMap::new(),
            prefix_types: HashMap::new(),
            aliases: HashMap::new(),
            expanding: std::cell::RefCell::new(HashSet::new()),
            methods: HashMap::new(),
            view_dirs: HashMap::new(),
            expr_types: std::cell::RefCell::new(HashMap::new()),
        }
    }

    fn finish(self) -> Typed {
        Typed {
            expr_types: self.expr_types.into_inner(),
        }
    }

    /// First pass: record entity port types and declared attribute targets.
    fn collect(&mut self, modules: &[Module]) {
        // Two passes: gather type declarations (structs, enums, aliases,
        // attrs, impls) first, so entity-port typing below can already see
        // e.g. `struct unsigned : Logic[]` regardless of module/item order.
        for m in modules {
            for item in &m.items {
                if matches!(item, Item::Entity(_)) {
                    continue;
                }
                self.collect_decl(item);
            }
        }
        // A field-less struct deriving from another vector family is itself one
        // (`struct Byte : unsigned[8]`); resolve that transitively before typing
        // ports, so such a type is treated as a numeric vector.
        self.resolve_transitive_vector_families();
        self.resolve_vector_elements();
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
                            view: view_key(&p.ty, &self.views),
                            range: self.declared_range(&p.ty),
                        })
                        .collect();
                    if e.attrs
                        .iter()
                        .any(|a| a.name.segments.last().map(|s| s.text.as_str()) == Some("test"))
                    {
                        self.test_entities.insert(e.name.text.clone());
                    }
                    self.entities.insert(e.name.text.clone(), ports);
                }
            }
        }
        // (inherited enum variants are expanded in collect_decl's tail)
        self.break_derivation_cycles();
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
                // Record every impl method by (type head, name) with its
                // declared return type, so `recv.method(args)` types (spec 3.20).
                if let Some(ty) = type_identity(&im.target) {
                    for it in &im.items {
                        if let ImplItem::Fn(f) = it {
                            self.methods
                                .insert((ty.clone(), f.name.text.clone()), f.ret.clone());
                        }
                    }
                }
                // Record `impl Trait for Type` so trait-driven checks (e.g.
                // conditions) can ask "does T implement Trait?".
                if let Some(tr) = &im.trait_ {
                    let trait_name = tr.segments.last().map(|s| s.text.clone());
                    let target = type_identity(&im.target);
                    if let (Some(mut t), Some(ty)) = (trait_name, target) {
                        if t == "Vector" {
                            self.vector_families.insert(ty.clone());
                        }
                        // `impl Operator<"<sym>", Input, Output> for T`: the
                        // first trait argument is the operator symbol, which
                        // keys the impl. A user operator (a non-standard symbol)
                        // must declare `#[precedence = N]`; the standard symbols
                        // carry built-in precedence.
                        let operator = if t == "Operator" {
                            im.trait_args.first().and_then(|a| match a {
                                GenericArg::Positional(Expr::StrLit { text, .. }) => {
                                    Some(text.clone())
                                }
                                _ => None,
                            })
                        } else {
                            None
                        };
                        if let Some(symbol) = &operator {
                            t = symbol.clone();
                            if crate::syntax::ast::is_reserved_operator(symbol) {
                                let hint = if crate::syntax::ast::is_comparison_operator(symbol) {
                                    " — derive comparisons from a `<=>` impl instead"
                                } else {
                                    " and cannot be overloaded"
                                };
                                self.error(
                                    codes::TYPE_MISMATCH,
                                    im.span,
                                    format!("`{symbol}` is reserved by the language{hint}"),
                                );
                            } else if !crate::syntax::ast::is_builtin_operator(symbol) {
                                let precedence = im.attrs.iter().find_map(|a| {
                                    (a.name
                                        .segments
                                        .last()
                                        .is_some_and(|n| n.text == "precedence"))
                                    .then_some(a)
                                    .and_then(|a| a.value.as_ref())
                                    .and_then(|v| match v {
                                        Expr::Int { text, span } => text
                                            .replace('_', "")
                                            .parse::<u8>()
                                            .ok()
                                            .map(|p| (p, *span)),
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
                        }
                        // `impl Suffix<"ns", _> for T` / `impl Prefix<"x", _>
                        // for T`: the first trait argument is the affix symbol
                        // and the impl target `T` is what the literal produces
                        // (spec 3.24). std owns which affixes exist.
                        if t == "Suffix" || t == "Prefix" {
                            if let Some(GenericArg::Positional(Expr::StrLit { text, .. })) =
                                im.trait_args.first()
                            {
                                let table = if t == "Suffix" {
                                    &mut self.suffix_types
                                } else {
                                    &mut self.prefix_types
                                };
                                table.entry(text.clone()).or_default().push(ty.clone());
                            }
                        }
                        // Operator overload signature: `input`/`output` are the
                        // 2nd/3rd trait arguments (after the symbol), falling
                        // back to the `apply` method's rhs-param / return types.
                        if operator.is_some() {
                            let arg_name = |index: usize| {
                                im.trait_args.get(index + 1).and_then(|a| match a {
                                    GenericArg::Positional(Expr::Path(p)) => {
                                        p.segments.last().map(|s| s.text.clone())
                                    }
                                    _ => None,
                                })
                            };
                            let input = arg_name(0).or_else(|| {
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
                            let output = arg_name(1).or_else(|| {
                                im.items.iter().find_map(|item| match item {
                                    ImplItem::Fn(f) => {
                                        f.ret.as_ref().and_then(type_head_name).map(str::to_string)
                                    }
                                    _ => None,
                                })
                            });
                            self.operator_sigs
                                .entry((t.clone(), ty.clone()))
                                .or_default()
                                .push((input, output));
                        }
                        if matches!(t.as_str(), "Index" | "IndexAssign") {
                            let arg_name = |index: usize| {
                                im.trait_args.get(index).and_then(|a| match a {
                                    GenericArg::Positional(Expr::Path(p)) => {
                                        p.segments.last().map(|s| s.text.clone())
                                    }
                                    _ => None,
                                })
                            };
                            self.index_sigs
                                .entry((t.clone(), ty.clone()))
                                .or_default()
                                .push((arg_name(0), arg_name(1)));
                        }
                        if is_blanket_array_impl(im) {
                            let requirement = blanket_requirement(im).unwrap_or_else(|| t.clone());
                            let supported = is_liftable_array_key(&t);
                            if !supported {
                                self.error(
                                    codes::TYPE_MISMATCH,
                                    im.span,
                                    format!(
                                        "element-wise array forwarding is not implemented for `{t}`"
                                    ),
                                );
                            }
                            let matching_bound = requirement == t;
                            if supported && !matching_bound {
                                self.error(
                                    codes::TYPE_MISMATCH,
                                    im.span,
                                    format!(
                                        "element-wise `{t}` forwarding requires the element bound `{t}`, found `{requirement}`"
                                    ),
                                );
                            }
                            if supported && matching_bound {
                                self.blanket_array_impls.insert(t, requirement);
                            }
                        } else {
                            self.trait_impls.entry(t).or_default().insert(ty);
                        }
                    }
                }
            }
            Item::Trait(t) => {
                let required = t
                    .items
                    .iter()
                    .filter(|f| f.body.is_none())
                    .map(|f| f.name.text.clone())
                    .collect();
                self.trait_required.insert(t.name.text.clone(), required);
            }
            Item::Enum(e) => {
                let vars: Vec<String> = e.variants.iter().map(|v| v.name.text.clone()).collect();
                self.own_variants.insert(e.name.text.clone(), vars.clone());
                self.enum_variants.insert(e.name.text.clone(), vars);
                if let Some(t) = &e.repr {
                    if let Some(h) = type_head_name(t) {
                        self.enum_bases.insert(e.name.text.clone(), h.to_string());
                    }
                }
            }
            Item::ExternBlock { fns, .. } => {
                for f in fns {
                    self.fn_arity.insert(
                        f.name.text.clone(),
                        f.params.iter().filter(|p| !p.is_self).count(),
                    );
                }
            }
            Item::Fn(f) if !f.generics.params.is_empty() => {
                self.fn_arity.insert(
                    f.name.text.clone(),
                    f.params.iter().filter(|p| !p.is_self).count(),
                );
                let vps = f
                    .params
                    .iter()
                    .filter(|p| !p.is_self)
                    .filter_map(|p| Some((p.name.as_ref()?.text.clone(), p.ty.clone()?)))
                    .collect();
                self.generic_fns
                    .insert(f.name.text.clone(), (f.generics.params.clone(), vps));
            }
            Item::Fn(f) => {
                self.fn_arity.insert(
                    f.name.text.clone(),
                    f.params.iter().filter(|p| !p.is_self).count(),
                );
                // Generic functions are verified at each call, where the
                // concrete types are known; only concrete ones are recorded.
                self.fn_param_types.insert(
                    f.name.text.clone(),
                    f.params
                        .iter()
                        .filter(|p| !p.is_self)
                        .map(|p| p.ty.clone())
                        .collect(),
                );
            }
            Item::Struct(st) => {
                let fields = st.fields.iter().map(|f| f.name.text.clone()).collect();
                self.structs
                    .insert(st.name.text.clone(), (st.base.clone(), fields));
            }
            Item::View(v) => {
                let key = declared_view_key(v);
                if self.views.insert(key.clone(), v.target.clone()).is_some() {
                    self.error(
                        codes::DUPLICATE_ITEM,
                        v.name.span,
                        format!(
                            "view `{}` is declared more than once for the same backing type",
                            v.name.text
                        ),
                    );
                }
                let dirs = v
                    .fields
                    .iter()
                    .map(|f| (f.name.text.clone(), f.dir))
                    .collect();
                self.view_dirs.insert(key, dirs);
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

    /// Derived-enum validation (spec 3.28): `enum B(A);` is a newtype over
    /// `A`'s variants, so `A` must itself be an enum. An enum carries no
    /// storage annotation — its width is derived from its variants and
    /// discriminants, and a specific wire width belongs to whatever carries
    /// the value (a port, a field, a function's return type), each of which
    /// already declares a type.
    ///
    /// Extension needs no check: the newtype form takes no body, so adding
    /// variants is not expressible. The older `enum B : A { … }` spelling is
    /// reported by the parser, which owns that message.
    fn check_enum(&mut self, e: &EnumDecl) {
        let Some(repr) = &e.repr else { return };
        let Some(head) = type_head_name(repr) else {
            return;
        };
        if self.own_variants.contains_key(head) {
            return;
        }
        let name = &e.name.text;
        self.error_with_help(
            codes::TYPE_MISMATCH,
            e.name.span,
            format!("`{head}` is not an enum, so `{name}` cannot derive from it"),
            format!(
                "an enum's width is derived from its variants — write `enum {name} \
                 {{ … }}` and, where a specific width is needed, declare it at the \
                 boundary that carries the value (a port, a field, or a function's \
                 return type) and convert there"
            ),
        );
    }

    /// Drop the base of any struct that (transitively) derives from itself.
    /// Resolve reports the cycle; this makes the table *acyclic* so the many
    /// walkers over it — field collection, width, vector-family fixpoint —
    /// terminate. Guarding each one individually was whack-a-mole: the crash
    /// was a stack overflow that aborted the process, so any missed walker is
    /// another core dump.
    fn break_derivation_cycles(&mut self) {
        let names: Vec<String> = self.structs.keys().cloned().collect();
        let mut cyclic: Vec<String> = Vec::new();
        for name in names {
            let mut seen: HashSet<String> = HashSet::new();
            let mut cur = name.clone();
            while seen.insert(cur.clone()) {
                let Some(next) = self
                    .structs
                    .get(&cur)
                    .and_then(|(b, _)| b.as_ref())
                    .and_then(type_head_name)
                    .map(str::to_string)
                else {
                    break;
                };
                if next == name {
                    cyclic.push(name.clone());
                    break;
                }
                cur = next;
            }
        }
        for name in cyclic {
            if let Some((base, _)) = self.structs.get_mut(&name) {
                *base = None;
            }
        }
    }

    /// The (transitive) field names of a struct-shaped base type.
    fn base_struct_fields(&self, ty: &Type) -> Vec<String> {
        self.base_struct_fields_at(ty, &mut HashSet::new())
    }

    fn struct_field_count(&self, id: crate::resolve::DefId) -> Option<usize> {
        let name = &self.resolved.def(id)?.name;
        self.struct_field_count_at(name, &mut HashSet::new())
    }

    fn struct_field_count_at(&self, name: &str, seen: &mut HashSet<String>) -> Option<usize> {
        if !seen.insert(name.to_string()) {
            return None;
        }
        let (base, own) = self.structs.get(name)?;
        let inherited = match base.as_ref().and_then(type_head_name) {
            Some(base) if self.structs.contains_key(base) => {
                self.struct_field_count_at(base, seen)?
            }
            _ => 0,
        };
        seen.remove(name);
        inherited.checked_add(own.len())
    }

    /// Cycle-safe because resolution reports cyclic derivation but checking
    /// continues best-effort.
    fn base_struct_fields_at(&self, ty: &Type, seen: &mut HashSet<String>) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(head) = type_head_name(ty) {
            if !seen.insert(head.to_string()) {
                return out;
            }
            if let Some((base, own)) = self.structs.get(head) {
                if let Some(b) = base {
                    out.extend(self.base_struct_fields_at(b, seen));
                }
                out.extend(own.iter().cloned());
            }
            seen.remove(head);
        }
        out
    }

    /// Whether `name` opted into packed numeric storage through `Vector`, or
    /// inherits from a family that did. There is no signedness — that lives in
    /// the family's operator impls.
    fn is_vector_family(&self, name: &str) -> bool {
        self.vector_families.contains(name)
    }

    /// Fixpoint: a field-less struct whose base is an already-known vector
    /// family is itself a vector family, so
    /// `struct Byte : unsigned[8]` inherits unsigned's numeric nature.
    fn resolve_transitive_vector_families(&mut self) {
        loop {
            let mut changed = false;
            let names: Vec<String> = self.structs.keys().cloned().collect();
            for name in names {
                if self.vector_families.contains(&name) {
                    continue;
                }
                let Some((base, fields)) = self.structs.get(&name) else {
                    continue;
                };
                if !fields.is_empty() {
                    continue;
                }
                let elem: Option<String> = match base {
                    Some(Type::Indexed { base, .. }) => type_head_name(base).map(str::to_string),
                    Some(Type::Path(p)) => p.segments.last().map(|s| s.text.clone()),
                    _ => None,
                };
                let is_vec = elem
                    .as_deref()
                    .is_some_and(|head| self.vector_families.contains(head));
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

    fn resolve_vector_elements(&mut self) {
        for family in self.vector_families.clone() {
            let mut current = family.as_str();
            let mut seen = HashSet::new();
            while seen.insert(current.to_string()) {
                let Some((base, _)) = self.structs.get(current) else {
                    break;
                };
                let Some(base) = base else {
                    break;
                };
                let Some(head) = type_head_name(base) else {
                    break;
                };
                if matches!(base, Type::Indexed { .. }) && !self.vector_families.contains(head) {
                    self.vector_elements
                        .insert(family.clone(), head.to_string());
                    break;
                }
                current = head;
            }
        }
    }

    fn has_impl(&self, key: &str, owner: &str) -> bool {
        if self
            .trait_impls
            .get(key)
            .is_some_and(|types| types.contains(owner))
        {
            return true;
        }
        let Some(element) = self.vector_elements.get(owner) else {
            return false;
        };
        self.blanket_array_impls
            .get(key)
            .is_some_and(|requirement| {
                self.trait_impls
                    .get(requirement)
                    .is_some_and(|types| types.contains(element))
            })
    }

    fn check_item(&mut self, item: &Item) {
        self.check_item_type_layouts(item);
        let sym = HashMap::new();
        let sym = &sym;
        match item {
            Item::Const(c) => {
                self.check_const_not_entity(c);
                self.check_expr(&c.value, sym);
            }
            Item::Enum(e) => {
                self.check_enum(e);
                for v in &e.variants {
                    if let Some(val) = &v.value {
                        self.check_expr(val, sym);
                    }
                }
                // Variants are values: two sharing a discriminant compare equal
                // and are indistinguishable at runtime (and in a waveform).
                // Only explicit constants are checked — implicit numbering
                // cannot collide.
                let mut seen: HashMap<i64, &str> = HashMap::new();
                for v in &e.variants {
                    let Some(val) = &v.value else { continue };
                    let Some(n) = Self::const_literal(val) else {
                        continue;
                    };
                    if let Some(prev) = seen.insert(n, v.name.text.as_str()) {
                        self.error(
                            codes::DUPLICATE_ITEM,
                            v.name.span,
                            format!(
                                "`{}::{}` and `{}::{prev}` both have the value {n}",
                                e.name.text, v.name.text, e.name.text
                            ),
                        );
                    }
                }
            }
            Item::Entity(e) => {
                for port in &e.ports {
                    self.check_applied_view(&port.ty);
                }
                for a in &e.attrs {
                    self.check_attr_target(a, "entity", Some(e.name.text.as_str()));
                    self.check_attr_value(a);
                    if let Some(v) = &a.value {
                        self.check_expr(v, sym);
                    }
                }
            }
            Item::Impl(im) => {
                self.check_applied_view(&im.target);
                self.check_impl(im);
            }
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
            // A struct newtype needs no check of its own: the form carries no
            // body, so there is nothing to validate past its base type.
            Item::Struct(_) => {}
            Item::View(v) => self.check_view(v),
            Item::Using(_) | Item::AttrDecl(_) | Item::ExternBlock { .. } => {}
        }
    }

    /// Validate constant layout bounds before elaboration/lowering tries to
    /// flatten them. Symbolic widths are checked after substitution; here we
    /// catch source constants that cannot fit the compiler's `u32` layout
    /// representation and would otherwise truncate or attempt an impossible
    /// allocation during error recovery.
    fn check_type_layout(&mut self, ty: &Type) {
        match ty {
            Type::Path(_) => {}
            Type::Indexed { base, index, .. } => {
                self.check_type_layout(base);
                let Some(index) = index.as_deref() else {
                    return;
                };
                match index {
                    Expr::Range { lo, hi, .. } => {
                        let (Some(left), Some(right)) = (signed_lit(lo), signed_lit(hi)) else {
                            return;
                        };
                        let length = (i128::from(left) - i128::from(right)).unsigned_abs() + 1;
                        if length > u128::from(u32::MAX) {
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr_span(index),
                                format!(
                                    "range contains {length} elements, exceeding the compiler \
                                     layout maximum of {}",
                                    u32::MAX
                                ),
                            );
                        }
                    }
                    _ => {
                        let Some(width) = signed_lit(index) else {
                            return;
                        };
                        if width < 0 {
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr_span(index),
                                "a type width cannot be negative".to_string(),
                            );
                        } else if u32::try_from(width).is_err() {
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr_span(index),
                                format!(
                                    "type width {width} exceeds the compiler layout maximum of {}",
                                    u32::MAX
                                ),
                            );
                        }
                    }
                }
            }
            Type::Generic { base, .. } => self.check_type_layout(base),
            Type::View { target, .. } => self.check_type_layout(target),
        }
    }

    fn check_fn_type_layouts(&mut self, function: &FnDecl) {
        self.check_param_type_layouts(&function.generics);
        for parameter in &function.params {
            if let Some(ty) = &parameter.ty {
                self.check_type_layout(ty);
            }
        }
        if let Some(ret) = &function.ret {
            self.check_type_layout(ret);
        }
    }

    fn check_param_type_layouts(&mut self, parameters: &Params) {
        for parameter in &parameters.params {
            if let Some(bound) = &parameter.bound {
                self.check_type_layout(bound);
            }
        }
    }

    fn check_item_type_layouts(&mut self, item: &Item) {
        match item {
            Item::Using(using) => {
                if let UsingKind::Alias { ty, .. } = &using.kind {
                    self.check_type_layout(ty);
                }
            }
            Item::Const(constant) => self.check_type_layout(&constant.ty),
            Item::Fn(function) => self.check_fn_type_layouts(function),
            Item::ExternBlock { fns, .. } => {
                for function in fns {
                    self.check_fn_type_layouts(function);
                }
            }
            Item::Struct(structure) => {
                self.check_param_type_layouts(&structure.params);
                if let Some(base) = &structure.base {
                    self.check_type_layout(base);
                }
                for field in &structure.fields {
                    self.check_type_layout(&field.ty);
                }
            }
            Item::View(view) => {
                self.check_param_type_layouts(&view.params);
                self.check_type_layout(&view.target);
            }
            Item::Enum(en) => {
                if let Some(repr) = &en.repr {
                    self.check_type_layout(repr);
                }
            }
            Item::Entity(entity) => {
                self.check_param_type_layouts(&entity.params);
                for port in &entity.ports {
                    self.check_type_layout(&port.ty);
                }
            }
            Item::Impl(implementation) => {
                self.check_param_type_layouts(&implementation.params);
                self.check_type_layout(&implementation.target);
                for item in &implementation.items {
                    match item {
                        ImplItem::Const(constant) => self.check_type_layout(&constant.ty),
                        ImplItem::Let(declaration) => {
                            if let Some(ty) = &declaration.ty {
                                self.check_type_layout(ty);
                            }
                        }
                        ImplItem::Fn(function) => self.check_fn_type_layouts(function),
                        ImplItem::ModeField { .. } | ImplItem::Stmt(_) => {}
                    }
                }
            }
            Item::Trait(trait_) => {
                self.check_param_type_layouts(&trait_.params);
                for function in &trait_.items {
                    self.check_fn_type_layouts(function);
                }
            }
            Item::AttrDecl(attribute) => self.check_type_layout(&attribute.ty),
        }
    }

    fn check_view(&mut self, view: &ViewDecl) {
        let target_ty = &view.target;
        let Some(target) = type_head_name(target_ty) else {
            return;
        };
        if !self.structs.contains_key(target) {
            self.error(
                codes::TYPE_MISMATCH,
                type_head_span(target_ty).unwrap_or(view.span),
                format!(
                    "view `{}` must target a struct, found `{target}`",
                    view.name.text
                ),
            );
            return;
        }
        let fields = self.base_struct_fields(target_ty);
        let mut seen = HashSet::new();
        for f in &view.fields {
            if !fields.iter().any(|n| n == &f.name.text) {
                self.error(
                    codes::TYPE_MISMATCH,
                    f.name.span,
                    format!("struct `{target}` has no field `{}`", f.name.text),
                );
            } else if !seen.insert(f.name.text.clone()) {
                self.error(
                    codes::DUPLICATE_ITEM,
                    f.name.span,
                    format!("view field `{}` is declared more than once", f.name.text),
                );
            }
        }
        for field in fields {
            if !seen.contains(&field) {
                self.error(
                    codes::TYPE_MISMATCH,
                    view.name.span,
                    format!(
                        "view `{}` does not specify direction for `{field}`",
                        view.name.text
                    ),
                );
            }
        }
    }

    fn check_applied_view(&mut self, ty: &Type) {
        let Type::View { view, target, span } = ty else {
            return;
        };
        let Some(view_name) = view.segments.last().map(|i| i.text.as_str()) else {
            return;
        };
        let Some(target_name) = type_head_name(target) else {
            return;
        };
        let key = format!("{view_name}@{target_name}");
        if !self.views.contains_key(&key) {
            let msg = format!("view `{view_name}` is not declared for struct `{target_name}`");
            // The two names in the reverse order is the pre-migration spelling
            // (`impl Source Stream`). Naming that outright beats a message that
            // reads backwards from what was written.
            if self
                .views
                .contains_key(&format!("{target_name}@{view_name}"))
            {
                self.error_with_help(
                    codes::TYPE_MISMATCH,
                    *span,
                    msg,
                    format!(
                        "write `{view_name} {target_name}` — the backing type leads \
                         and the view follows it, as in a port's `name: Type view`"
                    ),
                );
            } else {
                self.error(codes::TYPE_MISMATCH, *span, msg);
            }
        }
    }

    /// Spec 3.5: an attribute may only be applied to a target its declaration
    /// allows. Targets are item kinds (`entity`, `let`, `port`) or **type
    /// names** — `pub attr external_clock: Bool for Pll;` is valid only on
    /// the `Pll` entity or on declarations/instances of `Pll` (per-instance
    /// vendor metadata, preserved for external tools). Unknown attribute
    /// names on entities are reported by name resolution.
    fn check_attr_target(&mut self, a: &Attr, kind: &str, type_name: Option<&str>) {
        let name = a
            .name
            .segments
            .last()
            .map(|s| s.text.as_str())
            .unwrap_or("");
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
        self.warn_unimplemented_attr(name, a.name.span);
    }

    /// Attributes `std::attrs` declares and the compiler resolves, but which
    /// no stage reads. Writing one has no effect, and nothing else says so —
    /// `#[name = "foo"]` looks like it renames the emitted entity and does
    /// nothing at all.
    fn warn_unimplemented_attr(&mut self, name: &str, span: Span) {
        let purpose = match name {
            "keep" => "preserving a signal through optimization",
            "library" => "the emitted library name",
            "name" => "the emitted entity name",
            _ => return,
        };
        self.warn(
            codes::UNIMPLEMENTED_ATTR,
            span,
            format!("attribute `{name}` has no effect yet"),
            &format!("it is reserved for {purpose}; nothing reads it today"),
        );
    }

    /// `p.nosuch` on a struct: the field access lowered to `Unknown`, so the
    /// driver silently carried no value. Only *plain struct* receivers are
    /// checked — an instance port (`dut.y`), a view leaf and a derived
    /// struct's inherited fields all reach here as field accesses too, so the
    /// check walks the derivation chain and stays silent on anything else.
    fn check_field_exists(&mut self, base: &Expr, field: &Ident, sym: &HashMap<String, Ty>) {
        let Some(head) = self.ty_head(&self.type_of(base, sym)) else {
            return;
        };
        // `s.ready()` parses as a call over a *field* node, so a method name
        // reaches here too — it is not a missing field.
        if self
            .methods
            .contains_key(&(head.clone(), field.text.clone()))
        {
            return;
        }
        // Walk `struct B : A` so an inherited field counts as present.
        let mut seen = HashSet::new();
        let mut cur = Some(head.clone());
        while let Some(name) = cur {
            if !seen.insert(name.clone()) {
                return; // cyclic derivation: already diagnosed elsewhere
            }
            let Some((base_ty, fields)) = self.structs.get(&name) else {
                return;
            };
            if fields.contains(&field.text) {
                return;
            }
            cur = base_ty
                .as_ref()
                .and_then(type_head_name)
                .map(str::to_string);
        }
        let fields = self
            .structs
            .get(&head)
            .map(|(_, f)| f.clone())
            .unwrap_or_default();
        let mut d = Diagnostic::error(format!("`{head}` has no field `{}`", field.text))
            .with_code(codes::UNKNOWN_NAME)
            .at(field.span);
        if !fields.is_empty() {
            d = d.help(format!("it has: {}", fields.join(", ")));
        }
        self.sink.emit(d);
    }

    /// A method call whose name no impl provides for the receiver's type.
    /// It used to lower to `Unknown` — silently producing a driver with an
    /// unknown value, or (worse) an unknown *condition*, so an `if
    /// clk.typo()` block quietly became combinational.
    ///
    /// Deliberately conservative: only a receiver whose type head is known
    /// *and* which has at least one method recorded is checked, so a type
    /// whose methods this stage never collected can't false-positive.
    fn check_method_exists(&mut self, callee: &Expr, sym: &HashMap<String, Ty>) {
        let Expr::Field { base, field, span } = callee else {
            return;
        };
        let _ = &base;
        let recv = self.type_of(base, sym);
        let Some(head) = self.ty_head(&recv) else {
            return;
        };
        if self
            .methods
            .contains_key(&(head.clone(), field.text.clone()))
        {
            return;
        }
        // Only complain about a type we actually know methods for.
        if !self.methods.keys().any(|(h, _)| *h == head) {
            return;
        }
        let mut known: Vec<&str> = self
            .methods
            .keys()
            .filter(|(h, _)| *h == head)
            .map(|(_, m)| m.as_str())
            .collect();
        known.sort();
        let mut d = Diagnostic::error(format!("`{head}` has no method `{}`", field.text))
            .with_code(codes::INVALID_METHOD_CALL)
            .at(*span);
        if !known.is_empty() {
            d = d.help(format!("it has: {}", known.join(", ")));
        }
        self.sink.emit(d);
    }

    /// Spec 3.20: a trait is a compile-time contract, so an implementation
    /// must provide every method the trait declares without a default body.
    /// A partial impl used to pass, leaving the missing method to fail much
    /// later (or silently do nothing).
    fn check_trait_contract(&mut self, im: &ImplDecl) {
        let Some(tr) = im.trait_.as_ref().and_then(|t| t.segments.last()) else {
            return;
        };
        let Some(required) = self.trait_required.get(&tr.text) else {
            return;
        };
        let provided: HashSet<&str> = im
            .items
            .iter()
            .filter_map(|it| match it {
                ImplItem::Fn(f) => Some(f.name.text.as_str()),
                _ => None,
            })
            .collect();
        let missing: Vec<String> = required
            .iter()
            .filter(|m| !provided.contains(m.as_str()))
            .map(|m| format!("`{m}`"))
            .collect();
        if !missing.is_empty() {
            let target = type_head_name(&im.target).unwrap_or("this type");
            self.error(
                codes::TYPE_MISMATCH,
                im.span,
                format!(
                    "`impl {} for {target}` is missing {}",
                    tr.text,
                    missing.join(", ")
                ),
            );
        }
    }

    fn check_impl(&mut self, im: &ImplDecl) {
        // Stimulus primitives are meaningful in a testbench; in an entity body
        // lowering dropped them without a word.
        let saved_tb = self
            .in_testbench
            .replace(type_head_name(&im.target).is_some_and(|n| self.test_entities.contains(n)));
        self.check_impl_inner(im);
        self.in_testbench.set(saved_tb);
    }

    fn check_impl_inner(&mut self, im: &ImplDecl) {
        self.check_trait_contract(im);
        // The supported constrained array implementations are lowered by the
        // element-wise vector machinery. Still validate their metadata here;
        // abstract `T[]` has no standalone runtime representation for the
        // ordinary body checker.
        if is_blanket_array_impl(im) {
            let sym = HashMap::new();
            for attr in &im.attrs {
                self.check_attr_target(attr, "impl", type_head_name(&im.target));
                self.check_attr_value(attr);
                if let Some(value) = &attr.value {
                    self.check_expr(value, &sym);
                }
            }
            return;
        }
        let (dirs, sym, ranged) = self.impl_env(im);
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
                        let name = a
                            .name
                            .segments
                            .last()
                            .map(|s| s.text.as_str())
                            .unwrap_or("");
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
                ImplItem::Stmt(s) => self.check_stmt(s, &dirs, &sym, &ranged),
            }
        }
        self.lint_dead_assignments(im.items.iter().filter_map(|it| match it {
            ImplItem::Stmt(s) => Some(s),
            _ => None,
        }));
    }

    /// Build the value environment for an impl body: the `in` ports (for the
    /// write check) and a name -> type table (ports + impl-level lets/consts).
    fn impl_env(&self, im: &ImplDecl) -> ImplEnvironment {
        let mut illegal = HashSet::new();
        let mut plain_in_roots = HashSet::new();
        let mut sym = HashMap::new();
        let mut ranged: HashMap<String, (i64, i64)> = HashMap::new();
        if im.trait_.is_none() {
            if let Some(ports) = type_head_name(&im.target).and_then(|n| self.entities.get(n)) {
                for p in ports {
                    sym.insert(p.name.clone(), p.ty.clone());
                    if let Some(r) = p.range {
                        ranged.insert(p.name.clone(), r);
                    }
                    if p.dir == Some(Direction::In) {
                        illegal.insert(p.name.clone());
                        // A *plain* (non-bus-mode) `in` port has no writable
                        // parts: driving a field/index of it is illegal too.
                        if p.view.is_none() {
                            plain_in_roots.insert(p.name.clone());
                        }
                    }
                    // A bus-mode port contributes each `in` leaf (`bus.ready`),
                    // so driving it inside the entity is rejected (spec 3.19).
                    if let Some(dirs) = p.view.clone().and_then(|k| self.view_dirs.get(&k)) {
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
                    let mut ty = l.ty.as_ref().map(|t| self.ast_ty(t)).unwrap_or(Ty::Error);
                    // An unconstrained local still acquires fixed storage in a
                    // native test executable. Retain the initializer's known
                    // shape in the statement environment so later writes are
                    // checked against that storage instead of treating `len=0`
                    // as a wildcard forever.
                    if let Ty::Array { len, .. } = &mut ty {
                        if *len == 0 {
                            *len = match l.value.as_ref() {
                                Some(Expr::StrLit { text, .. }) => {
                                    u32::try_from(text.chars().count()).unwrap_or(u32::MAX)
                                }
                                Some(Expr::Array { elems, .. }) => {
                                    u32::try_from(elems.len()).unwrap_or(u32::MAX)
                                }
                                Some(value) => match self.type_of(value, &sym) {
                                    Ty::Array { len, .. } => len,
                                    _ => 0,
                                },
                                None => 0,
                            };
                        }
                    }
                    sym.insert(l.name.text.clone(), ty);
                    if let Some(r) = l.ty.as_ref().and_then(|t| self.declared_range(t)) {
                        ranged.insert(l.name.text.clone(), r);
                    }
                }
                ImplItem::Const(c) => {
                    sym.insert(c.name.text.clone(), self.ast_ty(&c.ty));
                }
                _ => {}
            }
        }
        (
            PortDirs {
                illegal,
                plain_in_roots,
            },
            sym,
            ranged,
        )
    }

    /// Two *unconditional* assignments to one target in the same block: the
    /// first can never be observed, because within a driver context a later
    /// assignment overrides (spec 3.14). Almost always a typo or leftover.
    /// Conservative — any conditional/looping statement in between resets the
    /// scan, so the common `default then override` shapes never trip it.
    fn lint_dead_assignments<'s>(&mut self, stmts: impl Iterator<Item = &'s Stmt>) {
        let mut seen: HashMap<String, Span> = HashMap::new();
        for s in stmts {
            match s {
                Stmt::Assign { target, span, .. } => {
                    let key = crate::syntax::pretty::expr_string(target);
                    if let Some(prev) = seen.insert(key.clone(), *span) {
                        self.sink.emit(
                            Diagnostic::warning(format!(
                                "`{key}` is assigned again here; the earlier \
                                 assignment has no effect"
                            ))
                            .with_code(codes::DEAD_ASSIGNMENT)
                            .at(*span)
                            .label(prev, "this assignment is overridden")
                            .help(
                                "remove the dead assignment, or make one of them \
                                 conditional if you meant a default",
                            ),
                        );
                    }
                }
                // Anything that may write conditionally ends the run.
                _ => seen.clear(),
            }
        }
    }

    fn check_block(&mut self, b: &Block) {
        // Every caller of this is a function body — a trait method, a free
        // function, or an impl method — so `return` is legal inside it.
        let saved = self.in_fn_body.replace(true);
        let (dirs, sym) = (
            PortDirs {
                illegal: HashSet::new(),
                plain_in_roots: HashSet::new(),
            },
            HashMap::new(),
        );
        let ranged = HashMap::new();
        self.lint_dead_assignments(b.stmts.iter());
        for s in &b.stmts {
            self.check_stmt(s, &dirs, &sym, &ranged);
        }
        self.in_fn_body.set(saved);
    }

    fn check_stmt(
        &mut self,
        s: &Stmt,
        dirs: &PortDirs,
        sym: &HashMap<String, Ty>,
        ranged: &HashMap<String, (i64, i64)>,
    ) {
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
                self.check_assign_range(target, value, ranged);
                let custom_index = self.check_index_assign(target, value, sym);
                if !custom_index {
                    self.check_assignment(target, value, sym);
                    self.check_expr(target, sym);
                } else if let Expr::Index { base, index, .. } = target {
                    self.check_expr(base, sym);
                    if let Expr::PartialRange { lo, hi, .. } = index.as_ref() {
                        if let Some(lo) = lo {
                            self.check_expr(lo, sym);
                        }
                        if let Some(hi) = hi {
                            self.check_expr(hi, sym);
                        }
                    } else {
                        self.check_expr(index, sym);
                    }
                }
                self.check_expr(value, sym);
            }
            Stmt::If(i) => self.check_if(i, dirs, sym, ranged),
            Stmt::Match(m) => {
                self.check_match_exhaustive(m, sym);
                self.check_unreachable_arms(m);
                for arm in &m.arms {
                    self.check_pattern_form(&arm.pattern);
                }
                self.check_expr(&m.scrutinee, sym);
                for arm in &m.arms {
                    for s in &arm.body.stmts {
                        self.check_stmt(s, dirs, sym, ranged);
                    }
                }
            }
            Stmt::For {
                var, range, body, ..
            } => {
                self.check_expr(range, sym);
                let loop_ty = match range {
                    Expr::Range { .. } => Ty::Integer,
                    _ => match self.type_of(range, sym) {
                        Ty::Array { elem, .. } => *elem,
                        _ => Ty::Error,
                    },
                };
                let mut loop_sym = sym.clone();
                loop_sym.insert(var.text.clone(), loop_ty);
                for s in &body.stmts {
                    self.check_stmt(s, dirs, &loop_sym, ranged);
                }
            }
            Stmt::Expr(e) => {
                self.check_stimulus_context(e);
                self.check_expr(e, sym);
            }
            Stmt::Return { value, span } => {
                if let Some(v) = value {
                    self.check_expr(v, sym);
                }
                if !self.in_fn_body.get() {
                    self.error_with_help(
                        codes::INVALID_METHOD_CALL,
                        *span,
                        "`return` outside a function".to_string(),
                        "an entity body describes hardware that is always active, so there \
                         is nothing to return from — lowering used to drop this statement \
                         silently"
                            .to_string(),
                    );
                }
            }
        }
    }

    /// `await`, `assert!`, `print!` and `warn!` drive and observe a simulation;
    /// an entity body describes hardware that is always active and has no
    /// stimulus to run. Lowering handles them only in a testbench and its
    /// catch-all dropped them elsewhere, so an assertion written into a design
    /// silently never ran.
    fn check_stimulus_context(&mut self, e: &Expr) {
        if self.in_testbench.get() || self.in_fn_body.get() {
            return;
        }
        let Expr::Call { callee, span, .. } = e else {
            return;
        };
        let Expr::Path(p) = callee.as_ref() else {
            return;
        };
        let name = match p.segments.as_slice() {
            [seg] => seg.text.as_str(),
            _ => return,
        };
        if !matches!(name, "await" | "wait" | "assert" | "print" | "warn") {
            return;
        }
        self.error_with_help(
            codes::INVALID_METHOD_CALL,
            *span,
            format!("`{name}` is only available in a testbench"),
            "an entity body describes hardware that is always active — put stimulus and \
             checks in a `#[test]` entity, which drives this one"
                .to_string(),
        );
    }

    fn check_if(
        &mut self,
        i: &IfStmt,
        dirs: &PortDirs,
        sym: &HashMap<String, Ty>,
        ranged: &HashMap<String, (i64, i64)>,
    ) {
        self.check_condition(&i.cond, sym);
        self.check_expr(&i.cond, sym);
        for s in &i.then.stmts {
            self.check_stmt(s, dirs, sym, ranged);
        }
        match i.else_.as_deref() {
            Some(ElseBranch::Block(b)) => {
                for s in &b.stmts {
                    self.check_stmt(s, dirs, sym, ranged);
                }
            }
            Some(ElseBranch::If(inner)) => self.check_if(inner, dirs, sym, ranged),
            None => {}
        }
    }

    /// A condition's type must implement `Boolean` (spec 3.16, generalized).
    /// `Bit`/`Bool` have built-in impls; user types opt in with `impl Boolean
    /// for T`; `Logic` has none, so it still requires an explicit comparison.
    /// An unknown (`Error`) condition type is skipped to avoid false positives.
    fn check_condition(&mut self, cond: &Expr, sym: &HashMap<String, Ty>) {
        let ty = self.type_of(cond, sym);
        let Some(name) = self.type_kind_name(&ty) else {
            return;
        };
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
        self.check_arms_exhaustive(&m.scrutinee, &m.arms, m.span, sym);
    }

    /// Shared by the statement and expression forms. A match *expression* was
    /// not checked at all, so a missing variant drew no diagnostic and only
    /// surfaced much later as a design no engine would run.
    fn check_arms_exhaustive(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        span: Span,
        sym: &HashMap<String, Ty>,
    ) {
        let Ty::Named(id) = self.type_of(scrutinee, sym) else {
            return;
        };
        let Some(enum_name) = self.resolved.def(id).map(|d| d.name.clone()) else {
            return;
        };
        let Some(variants) = self.enum_variants.get(&enum_name).cloned() else {
            return;
        };

        // Collect the covered variant names, flattening or-patterns; a wildcard
        // (bare or inside an `|`) makes the match exhaustive.
        let mut covered: HashSet<String> = HashSet::new();
        for a in arms {
            let (vars, wild) = pattern_covers(&a.pattern);
            if wild {
                return;
            }
            covered.extend(vars);
        }
        let missing: Vec<String> = variants
            .into_iter()
            .filter(|v| !covered.contains(v.as_str()))
            .collect();
        if !missing.is_empty() {
            let names = missing
                .iter()
                .map(|v| format!("`{v}`"))
                .collect::<Vec<_>>()
                .join(", ");
            self.sink.emit(
                Diagnostic::warning(format!(
                    "non-exhaustive match on `{enum_name}`: missing {names}"
                ))
                .with_code(codes::NON_EXHAUSTIVE_MATCH)
                .at(span)
                .help("add the missing arms, or a `_` wildcard"),
            );
        }
    }

    /// Reject a pattern shape the lowering cannot honour (spec Stage 10,
    /// "invalid pattern"). Variants are `::`-qualified (`Color::Red`), so a
    /// bare name matches nothing the compiler knows — and `arm_match_cond`
    /// treats every pattern it cannot lower as a wildcard, which would make
    /// such an arm silently swallow the whole match, `_` arms included.
    fn check_pattern_form(&mut self, p: &Pattern) {
        match p {
            Pattern::Path(path) if path.segments.len() == 1 => {
                let name = &path.segments[0].text;
                self.sink.emit(
                    Diagnostic::error(format!("`{name}` is not a valid pattern"))
                        .with_code(codes::INVALID_PATTERN)
                        .at(path.segments[0].span)
                        .help(
                            "enum patterns name their type (`Color::Red`); a bare name is not \
                             a binding — use `_` to match anything",
                        ),
                );
            }
            // A pattern whose text is not a well-formed mask (a digit outside
            // the radix) is just as invisible: IR
            // lowering wildcards it while the runner never matches it, so the
            // engines disagree on top of it silently swallowing the arm.
            Pattern::BitPattern { text, span }
                if crate::syntax::bit_pattern_mask(text).is_none() =>
            {
                self.sink.emit(
                    Diagnostic::error(format!("`{text}` is not a valid bit pattern"))
                        .with_code(codes::INVALID_PATTERN)
                        .at(*span)
                        .help(
                            "a bare string is per-bit with `-` as the don't-care (`\"01--\"`); \
                             an `x`/`o` prefix takes hex/octal digits with `?` masking a group",
                        ),
                );
            }
            Pattern::Or { alts, .. } => {
                for a in alts {
                    self.check_pattern_form(a);
                }
            }
            _ => {}
        }
    }

    /// Warn (spec Stage 10) on arms that can never match: anything after a `_`
    /// wildcard, or a variant already covered by an earlier arm.
    fn check_unreachable_arms(&mut self, m: &MatchStmt) {
        let mut after_wildcard = false;
        let mut seen: HashSet<String> = HashSet::new();
        // Inclusive integer ranges already matched (a bare literal is lo==hi).
        let mut ranges: Vec<(i64, i64)> = Vec::new();
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
                    // A range (or bare literal) wholly inside one already
                    // matched can never be reached — first match wins.
                    Pattern::Range { lo, hi, .. } => {
                        let (lo, hi) = (*lo.min(hi), *lo.max(hi));
                        let covered = ranges.iter().find(|(a, b)| lo >= *a && hi <= *b).copied();
                        ranges.push((lo, hi));
                        covered.map(|(a, b)| {
                            let this = if lo == hi {
                                format!("`{lo}`")
                            } else {
                                format!("`{lo}..{hi}`")
                            };
                            let prev = if a == b {
                                format!("`{a}`")
                            } else {
                                format!("`{a}..{b}`")
                            };
                            format!("{this} is already covered by the earlier arm {prev}")
                        })
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
        self.has_impl("Boolean", name)
    }

    /// The name a type is keyed by in the trait-impl table (`unsigned[8]` and
    /// `unsigned` share `unsigned`). `Error`/array types have no name.
    fn type_kind_name(&self, t: &Ty) -> Option<String> {
        match t {
            Ty::Integer => Some("integer".to_string()),
            Ty::Real => Some("real".to_string()),
            Ty::Char => Some("Char".to_string()),
            Ty::Named(id) => self.resolved.def(*id).map(|d| d.name.clone()),
            Ty::Array {
                family: Some(name), ..
            } => Some(name.clone()),
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
        let bad = exact
            .as_deref()
            .filter(|n| dirs.illegal.contains(*n))
            .map(str::to_string)
            .or_else(|| root.filter(|r| dirs.plain_in_roots.contains(r)));
        if let Some(name) = bad {
            self.sink.emit(
                Diagnostic::error(format!("cannot assign to input port `{name}`"))
                    .with_code(codes::WRITE_TO_INPUT_PORT)
                    .at(expr_span(target))
                    .help("input ports are read-only inside the entity; drive it from the instantiating scope"),
            );
        }
    }

    /// Check a custom indexed write. Returns true when `target` is a
    /// non-intrinsic index operation, so ordinary lvalue assignment checking
    /// must not treat the `Index` read result as storage.
    fn check_index_assign(
        &mut self,
        target: &Expr,
        value: &Expr,
        sym: &HashMap<String, Ty>,
    ) -> bool {
        let Expr::Index { base, index, span } = target else {
            return false;
        };
        let base_ty = self.type_of(base, sym);
        if matches!(base_ty, Ty::Array { .. }) {
            return false;
        }
        let Some(owner) = self.type_kind_name(&base_ty) else {
            return false;
        };
        if matches!(index.as_ref(), Expr::PartialRange { .. }) {
            self.error(
                codes::TYPE_MISMATCH,
                *span,
                format!(
                    "partial range indexing on `{owner}` has no declared bounds; use an explicit `left..right` range"
                ),
            );
            return true;
        }
        let input = if matches!(
            index.as_ref(),
            Expr::Range { .. } | Expr::PartialRange { .. }
        ) {
            Some("Range".to_string())
        } else {
            self.type_kind_name(&self.type_of(index, sym))
        };
        let value_ty = self.type_kind_name(&self.type_of(value, sym));
        let found = self
            .index_sigs
            .get(&("IndexAssign".to_string(), owner.clone()))
            .is_some_and(|sigs| {
                sigs.iter().any(|(i, v)| {
                    (i.is_none() || i.as_ref() == input.as_ref())
                        && (v.is_none() || v.as_ref() == value_ty.as_ref())
                })
            });
        if !found {
            self.error(
                codes::TYPE_MISMATCH,
                *span,
                format!(
                    "indexed assignment on `{owner}` needs `impl IndexAssign<{}, {}> for {owner}`",
                    input.as_deref().unwrap_or("_"),
                    value_ty.as_deref().unwrap_or("_"),
                ),
            );
        }
        true
    }

    /// Spec 3.5: an attribute's value must match the type its declaration gives.
    fn check_attr_value(&mut self, a: &Attr) {
        let Some(value) = &a.value else { return };
        let name = a
            .name
            .segments
            .last()
            .map(|s| s.text.as_str())
            .unwrap_or("");
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
        let mut diag = Diagnostic::error(format!("`let {}` needs a type annotation", l.name.text))
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
        let Some(head) = type_head_name(&c.ty) else {
            return;
        };
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
        if matches!(lhs, Ty::Named(_))
            && matches!(value, Expr::Construct { .. } | Expr::Concat { .. })
        {
            return;
        }
        if !matches!(lhs, Ty::Error) && !self.assignable(&lhs, value, sym) {
            let rhs = self.type_of(value, sym);
            let mut diag = Diagnostic::error(format!(
                "cannot initialize {} with {} without an explicit conversion",
                self.ty_display(&lhs),
                self.ty_display(&rhs)
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
                format!(
                    "wrap it in a conversion, e.g. `{}(...)`",
                    self.ty_display(&lhs)
                )
            });
            self.sink.emit(
                Diagnostic::error(format!(
                    "cannot assign {} to {} without an explicit conversion",
                    self.ty_display(&rhs),
                    self.ty_display(&lhs)
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
        let Some(d) = self.resolved.def(id) else {
            return false;
        };
        self.enum_variants
            .get(&d.name)
            .is_some_and(|vars| vars.iter().any(|v| v.trim_matches('\'') == ch.to_string()))
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
            let Some(trait_name) = type_head_name(bound) else {
                continue;
            };
            // Infer the type param from the first value param named after it.
            let inferred = vparams
                .iter()
                .position(|(_, t)| type_head_name(t) == Some(&gp.name.text))
                .and_then(|i| args.get(i))
                .map(|a| self.type_of(a, sym));
            let Some(ty) = inferred else { continue };
            if !self.satisfies(&ty, trait_name) {
                let name = self.ty_display(&ty);
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
    /// impl without false-flagging unsigned/signed/etc.
    fn satisfies(&self, ty: &Ty, trait_name: &str) -> bool {
        match self.type_kind_name(ty) {
            Some(kind) => {
                if self.has_impl(trait_name, &kind)
                    || self
                        .trait_impls
                        .get(trait_name)
                        .is_some_and(|implementors| {
                            implementors.contains(&kind)
                            // Applied views are stored by their full nominal
                            // identity (`Controller@Spi`), while expression
                            // typing names the visible view (`Controller`).
                            // Either matching applied identity satisfies a
                            // capability bound on that view.
                            || implementors
                                .iter()
                                .any(|name| name.starts_with(&format!("{kind}@")))
                        })
                {
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
                    match op {
                        BinOp::Add => a.checked_add(b),
                        BinOp::Sub => a.checked_sub(b),
                        BinOp::Mul => a.checked_mul(b),
                        BinOp::Div => a.checked_div(b),
                        _ => None,
                    }
                }
                _ => signed_lit(e),
            }
        }
        let Some(v) = args.first().and_then(const_fold) else {
            return;
        };
        self.check_fits_width(v, width as u32, expr_span(site));
    }

    /// `print!("{} {}", x)` silently rendered an empty slot, and a spare
    /// argument was silently dropped — in a testbench that is exactly where a
    /// wrong value costs you debugging time. Both engines share the arity, so
    /// checking it here covers them at once.
    fn check_format_arity(&mut self, callee: &Expr, args: &[Expr]) {
        let name = match callee {
            Expr::Path(p) if p.segments.len() == 1 => p.segments[0].text.as_str(),
            _ => return,
        };
        // `assert!`/`warn!(cond, "msg", args..)` put the format string second.
        let fmt_at = match name {
            "print" => 0,
            "assert" | "warn" => 1,
            _ => return,
        };
        let Some(Expr::StrLit { text, span }) = args.get(fmt_at) else {
            return;
        };
        let want = crate::syntax::format::arity(text);
        let have = args.len().saturating_sub(fmt_at + 1);
        if want != have {
            self.error(
                codes::TYPE_MISMATCH,
                *span,
                format!("format string takes {want} argument(s) but {have} were given"),
            );
        }
    }

    /// Render a type for a diagnostic, resolving a named struct/enum/entity to
    /// its declared name. The free `ty_name` has no definition table, so it can
    /// only say "a named type" — never show that to a user.
    fn ty_display(&self, t: &Ty) -> String {
        match t {
            Ty::Named(id) => self
                .resolved
                .def(*id)
                .map(|d| d.name.clone())
                .unwrap_or_else(|| ty_name(t)),
            Ty::Array {
                elem: _,
                len,
                family: Some(name),
            } => format!("{name}[{len}]"),
            Ty::Array {
                elem,
                len,
                family: None,
            } => format!("{}[{len}]", self.ty_display(elem)),
            _ => ty_name(t),
        }
    }

    /// A call to a *declared* function must pass one argument per parameter.
    /// Nothing checked this: a short call left a parameter unbound, and a short
    /// `extern "C"` call passed a garbage argument to real native code.
    /// Conversions, method calls and runtime-provided std functions have no
    /// declaration here and are skipped.
    fn check_call_arity(&mut self, callee: &Expr, args: &[Expr]) {
        let Expr::Path(p) = callee else { return };
        let [name] = p.segments.as_slice() else {
            return;
        };
        let Some(&want) = self.fn_arity.get(&name.text) else {
            return;
        };
        if args.len() != want {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(callee),
                format!(
                    "`{}` takes {want} argument(s) but {} were given",
                    name.text,
                    args.len()
                ),
            );
            return;
        }
        self.check_call_arg_types(&name.text, args);
    }

    /// Each argument must be assignable to its parameter, by the same rule an
    /// assignment uses. Only the count was checked, so a `real` handed to an
    /// `integer` parameter passed its f64 bits through as an integer, and a
    /// `signed[8]` passed its raw bit pattern — `abs(-5)` returned 251.
    fn check_call_arg_types(&mut self, name: &str, args: &[Expr]) {
        let Some(params) = self.fn_param_types.get(name).cloned() else {
            return;
        };
        let sym = HashMap::new();
        for (arg, pty) in args.iter().zip(params.iter()) {
            let Some(pty) = pty else { continue };
            let want = self.ast_ty(pty);
            if want == Ty::Error || self.assignable(&want, arg, &sym) {
                continue;
            }
            let got = self.type_of(arg, &sym);
            if got == Ty::Error {
                continue;
            }
            self.error_with_help(
                codes::TYPE_MISMATCH,
                expr_span(arg),
                format!(
                    "cannot pass {} to a {} parameter of `{name}` without an explicit conversion",
                    self.ty_display(&got),
                    self.ty_display(&want)
                ),
                format!(
                    "wrap it in a conversion, e.g. `{}(...)`",
                    self.ty_display(&want)
                ),
            );
        }
    }

    /// Runtime-provided std functions have no source `FnDecl`, so they do not
    /// enter `fn_arity`. Check their public call contract explicitly rather
    /// than letting generated C silently ignore extra arguments or fail much
    /// later while building the harness.
    fn check_runtime_call_arity(&mut self, callee: &Expr, args: &[Expr]) {
        let Expr::Path(path) = callee else { return };
        let [name] = path.segments.as_slice() else {
            return;
        };
        let expected = match name.text.as_str() {
            "rand" | "uniform" => 0,
            "seed" | "exists" | "read" | "read_to_string" => 1,
            "randint" => 2,
            _ => return,
        };
        if args.len() != expected {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(callee),
                format!(
                    "`{}` takes {expected} argument(s) but {} were given",
                    name.text,
                    args.len()
                ),
            );
        }
    }

    /// Whether `t` names an entity — i.e. an array of it is an *instance*
    /// array, which is always declared with a plain element count.
    fn is_entity_ty(&self, t: &Ty) -> bool {
        matches!(t, Ty::Named(id)
            if self.resolved.def(*id).map(|d| d.kind) == Some(DefKind::Entity))
    }

    /// A constant that cannot be represented in `width` bits is an error
    /// wherever it meets a sized vector — a conversion or a comparison. No
    /// signedness: a literal fits an N-bit vector if it lands in the union of
    /// the unsigned (`0..2^N`) and signed (`-2^(N-1)..`) ranges.
    fn check_fits_width(&mut self, v: i64, width: u32, span: Span) {
        if !(1..=64).contains(&width) {
            return;
        }
        // Both bounds are computed in `i64` and so saturate near its top:
        // `1i64 << 63` is already `i64::MIN`, which makes `- 1` overflow at
        // width 63 and the negation overflow at width 64. Those widths are
        // exactly where a bit vector meets a plain integer literal, so this is
        // reachable from any `a == 5` on a 63- or 64-bit signal.
        let hi = if width >= 63 {
            i64::MAX
        } else {
            (1i64 << width) - 1
        };
        let lo = if width >= 64 {
            i64::MIN
        } else {
            -(1i64 << (width - 1))
        };
        if v < lo || v > hi {
            self.error(
                codes::TYPE_MISMATCH,
                span,
                format!("`{v}` does not fit in a {width}-bit vector"),
            );
        }
    }

    /// Const-fold a literal arithmetic expression (`3`, `0 - 3`). `None` once
    /// any operand is a runtime value.
    fn const_literal(e: &Expr) -> Option<i64> {
        match e {
            Expr::Binary { op, lhs, rhs, .. } => {
                let (a, b) = (Self::const_literal(lhs)?, Self::const_literal(rhs)?);
                Some(match op {
                    BinOp::Add => a.checked_add(b)?,
                    BinOp::Sub => a.checked_sub(b)?,
                    BinOp::Mul => a.checked_mul(b)?,
                    BinOp::Div if b != 0 => a / b,
                    _ => return None,
                })
            }
            _ => signed_lit(e),
        }
    }

    /// A constant bit index or slice outside a packed vector's width has no
    /// hardware meaning — it lowered to `Unknown` and surfaced much later as a
    /// generic "no engine can run this design". Only *packed* vectors
    /// (family-carrying `Ty::Array`, indices `0..width-1`) are checked: an array with a
    /// declared range (`Logic[15..8]`) is indexed by its own bounds.
    fn check_index_bounds(&mut self, base: &Expr, index: &Expr, sym: &HashMap<String, Ty>) {
        // What we can bound-check, and how to name it. A packed vector is
        // indexed `0..width-1`. An *instance array* (`let s: Sub[4]`) is always
        // declared with a plain count, so it is 0-based too — unlike a data
        // array, which may carry a declared range (`Logic[15..8]`) and is
        // therefore left alone.
        let (len, noun) = match self.type_of(base, sym) {
            Ty::Array {
                len,
                family: Some(_),
                ..
            } => (len, "bit"),
            Ty::Array {
                elem,
                len,
                family: None,
            } if self.is_entity_ty(&elem) => (len, "instance"),
            _ => return,
        };
        if len == 0 {
            return; // parametric: not known yet
        }
        let mut check = |v: i64, e: &Expr| {
            if v < 0 || v >= len as i64 {
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(e),
                    match noun {
                        "bit" => format!(
                            "bit {v} is outside `0..{}` of this {len}-bit vector",
                            len - 1
                        ),
                        _ => format!(
                            "instance {v} is outside `0..{}` of this {len}-instance array",
                            len - 1
                        ),
                    },
                );
            }
        };
        match index {
            Expr::Range { lo, hi, .. } => {
                if let Some(v) = Self::const_literal(lo) {
                    check(v, lo);
                }
                if let Some(v) = Self::const_literal(hi) {
                    check(v, hi);
                }
            }
            Expr::PartialRange { lo, hi, .. } => {
                if let Some(lo) = lo {
                    if let Some(v) = Self::const_literal(lo) {
                        check(v, lo);
                    }
                }
                if let Some(hi) = hi {
                    if let Some(v) = Self::const_literal(hi) {
                        check(v, hi);
                    }
                }
            }
            _ => {
                if let Some(v) = Self::const_literal(index) {
                    check(v, index);
                }
            }
        }
    }

    fn check_custom_index(&mut self, base: &Expr, index: &Expr, sym: &HashMap<String, Ty>) {
        let base_ty = self.type_of(base, sym);
        if matches!(base_ty, Ty::Array { .. }) {
            return;
        }
        let Some(owner) = self.type_kind_name(&base_ty) else {
            return;
        };
        if matches!(index, Expr::PartialRange { .. }) {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(index),
                format!(
                    "partial range indexing on `{owner}` has no declared bounds; use an explicit `left..right` range"
                ),
            );
            return;
        }
        let input = if matches!(index, Expr::Range { .. } | Expr::PartialRange { .. }) {
            Some("Range".to_string())
        } else {
            self.type_kind_name(&self.type_of(index, sym))
        };
        let found = self
            .index_sigs
            .get(&("Index".to_string(), owner.clone()))
            .is_some_and(|sigs| {
                sigs.iter()
                    .any(|(i, _)| i.is_none() || i.as_ref() == input.as_ref())
            });
        if !found {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(index),
                format!(
                    "indexing `{owner}` needs `impl Index<{}, Output> for {owner}`",
                    input.as_deref().unwrap_or("_"),
                ),
            );
        }
    }

    /// `sig == 600` with `sig: unsigned[8]`: the comparison masks both sides to the
    /// operand width, so the literal silently becomes 88 and the guard fires on
    /// the wrong value. The masking is right for a *wrapped* expression
    /// (`q == 0 - 3` really is 253), so reject the un-representable literal
    /// instead (spec 3.17/3.26).
    fn check_comparison_fit(&mut self, lhs: &Expr, rhs: &Expr, sym: &HashMap<String, Ty>) {
        for (operand, lit) in [(lhs, rhs), (rhs, lhs)] {
            // Only flag a bare constant against a sized runtime operand.
            if Self::const_literal(operand).is_some() {
                continue;
            }
            let Some(v) = Self::const_literal(lit) else {
                continue;
            };
            if let Ty::Array {
                len,
                family: Some(_),
                ..
            } = self.type_of(operand, sym)
            {
                self.check_fits_width(v, len, expr_span(lit));
            }
        }
    }

    fn assignable(&self, lhs: &Ty, value: &Expr, sym: &HashMap<String, Ty>) -> bool {
        match value {
            // A numeric literal also initialises `real` (`.re = 10` is 10.0).
            Expr::Int { .. } => {
                matches!(
                    lhs,
                    Ty::Array {
                        family: Some(_),
                        ..
                    } | Ty::Integer
                        | Ty::Real
                        | Ty::Error
                )
            }
            Expr::CharLit { ch, .. } => {
                // A character literal reads through its context type (spec:
                // type kernel): builtin scalars, `Char`, or a user enum with
                // a matching character variant (e.g. ULogic's 'Z').
                if let Ty::Named(id) = lhs {
                    return self.enum_has_char_variant(*id, *ch);
                }
                matches!(lhs, Ty::Char | Ty::Error)
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
                Ty::Array {
                    elem,
                    len,
                    family: None,
                } => {
                    elems.len() as u32 == *len
                        && elems.iter().all(|e| self.assignable(elem, e, sym))
                }
                Ty::Error => true,
                _ => false,
            },
            // A name-less positional struct literal lexes as concatenation.
            // It is structurally assignable to a named struct when its arity
            // matches; field-value checks are contextualized during lowering.
            Expr::Concat { parts, .. } => match lhs {
                Ty::Named(id) => self.struct_field_count(*id) == Some(parts.len()),
                Ty::Error => true,
                _ => compatible(lhs, &self.type_of(value, sym)),
            },
            // A string is a sequence of characters: assigned to a `Logic`-vector
            // it fills each element with the matching `std_ulogic` (like `b"…"`),
            // and assigned to an array of a char-enum each character is a variant
            // — a string of logic values *is* a logic array, no prefix needed.
            Expr::StrLit { text, .. } => {
                let n = text.chars().count() as u32;
                match lhs {
                    Ty::Array {
                        len,
                        family: Some(_),
                        ..
                    } => (*len == 0 || n == *len) && text.chars().all(|c| "01ZXUWLH-".contains(c)),
                    // A char-enum array (`Color[3] = "rgb"`): each char a variant.
                    Ty::Array {
                        elem,
                        len,
                        family: None,
                    } if matches!(elem.as_ref(), Ty::Named(_)) => {
                        let Ty::Named(id) = elem.as_ref() else {
                            unreachable!()
                        };
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
                        format!(
                            "`'{}` is Phase-2 analogue syntax, not available in Phase 1",
                            attr.text
                        ),
                    );
                }
                // Anything outside the implemented set is reported here.
                // Silently lowering it produced an `Unknown` that only failed
                // at codegen, naming a driver index rather than the attribute.
                let a = attr.text.as_str();
                if !PHASE2_ATTRS.contains(&a) && !SYS_ATTRS.contains(&a) {
                    // The edge helpers are ordinary trait methods now, so this
                    // is the one wrong attribute worth a migration hint.
                    let help = if matches!(a, "rising" | "falling" | "edge") {
                        format!("the edge helpers are `ClockLike` methods now — write `.{a}()`")
                    } else {
                        format!(
                            "known system attributes: {}",
                            SYS_ATTRS
                                .iter()
                                .map(|s| format!("`'{s}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    self.error_with_help(
                        codes::UNKNOWN_NAME,
                        *span,
                        format!("unknown system attribute `'{a}`"),
                        help,
                    );
                }
                if matches!(attr.text.as_str(), "event" | "old") {
                    let base_ty = self.type_of(base, sym);
                    // An unresolved receiver (notably generic `self` in a
                    // trait method) cannot be classified here; avoid a
                    // cascading error and let its declaration/use checks speak.
                    if base_ty != Ty::Error && !self.is_digital_ty(&base_ty) {
                        self.error(
                            codes::INVALID_ATTR_TARGET,
                            *span,
                            format!(
                                "`::{}` requires a digital value, found {}",
                                attr.text,
                                ty_name(&base_ty)
                            ),
                        );
                    }
                }
                self.check_expr(base, sym);
            }
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => {
                self.check_expr(scrutinee, sym);
                // An expression must yield a value for every case it can meet,
                // so a missing variant matters at least as much here as in a
                // statement — and this form was not checked at all.
                self.check_arms_exhaustive(scrutinee, arms, *span, sym);
                for arm in arms {
                    self.check_pattern_form(&arm.pattern);
                    if let Some(v) = arm.value_expr() {
                        self.check_expr(v, sym);
                    }
                }
            }
            Expr::Field { base, field, .. } => {
                self.check_expr(base, sym);
                self.check_field_exists(base, field, sym);
            }
            Expr::Index { base, index, .. } => {
                self.check_expr(base, sym);
                match index.as_ref() {
                    Expr::PartialRange { lo, hi, .. } => {
                        if let Some(lo) = lo {
                            self.check_expr(lo, sym);
                        }
                        if let Some(hi) = hi {
                            self.check_expr(hi, sym);
                        }
                    }
                    _ => self.check_expr(index, sym),
                }
                self.check_index_bounds(base, index, sym);
                self.check_custom_index(base, index, sym);
            }
            Expr::Range { lo, hi, .. } => {
                self.check_expr(lo, sym);
                self.check_expr(hi, sym);
            }
            Expr::PartialRange { lo, hi, span } => {
                if let Some(lo) = lo {
                    self.check_expr(lo, sym);
                }
                if let Some(hi) = hi {
                    self.check_expr(hi, sym);
                }
                self.error(
                    codes::TYPE_MISMATCH,
                    *span,
                    "a partial range needs an indexed value to supply its omitted bounds"
                        .to_string(),
                );
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
                            format!(
                                "`not` is a per-bit operator; `{}` is not a bit-derived type",
                                self.ty_display(&t)
                            ),
                        );
                    }
                    if let Some(owner) = self.ty_head(&t) {
                        if !self.has_impl("not", &owner) {
                            self.error(
                                codes::TYPE_MISMATCH,
                                *span,
                                format!("`not` needs an `impl Operator<\"not\", …> for {owner}`"),
                            );
                        }
                    }
                }
            }
            Expr::IfExpr {
                cond, then, els, ..
            } => {
                // Same condition rule as statement `if` (must be Boolean).
                self.check_condition(cond, sym);
                self.check_expr(cond, sym);
                self.check_expr(then, sym);
                self.check_expr(els, sym);
            }
            Expr::Binary { op, lhs, rhs, span } => {
                self.check_expr(lhs, sym);
                self.check_expr(rhs, sym);
                if is_comparison(op) {
                    self.check_comparison_fit(lhs, rhs, sym);
                }
                // A constant zero divisor is always a mistake: hardware has no
                // trap for it, so today it just yields 0 with no complaint.
                if matches!(op, BinOp::Div) && Self::const_literal(rhs) == Some(0) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        expr_span(rhs),
                        "division by a constant zero".to_string(),
                    );
                }
                // A character literal's identity comes from its counterpart's
                // type (spec: type kernel); a numeric counterpart cannot read
                // one — conversion goes through an encoding table.
                for (lit, other) in [(lhs, rhs), (rhs, lhs)] {
                    if matches!(lit.as_ref(), Expr::CharLit { .. })
                        && matches!(
                            self.type_of(other, sym),
                            Ty::Array {
                                family: Some(_),
                                ..
                            } | Ty::Integer
                                | Ty::Real
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
                                    "`{op_str}` needs bit-derived operands (Bit/Logic/Bool/unsigned/signed); `{}` is a number",
                                    self.ty_display(&t)
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
                // vectors (`unsigned`/`signed`) legitimately compare to integers, so
                // they are excluded. (W-P008)
                if matches!(op_str, "==" | "!=") {
                    for (lit, other) in [(lhs, rhs), (rhs, lhs)] {
                        let is_int_lit =
                            matches!(lit.as_ref(), Expr::Int { text, .. } if !text.contains('.'));
                        if is_int_lit {
                            if let Some(name) = self.enum_operand_name(&self.type_of(other, sym)) {
                                let hint = match name.as_str() {
                                    "Bit" | "Logic" => {
                                        "compare against a value literal, e.g. `== '1'`"
                                    }
                                    "Bool" => {
                                        "compare against `true`/`false`, or use the value directly"
                                    }
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
                        let has_op = |tr: &str| self.has_impl(tr, &name);
                        // Operators dispatch by symbol; one three-way `<=>`
                        // (`-> Ordering`) impl derives every comparison.
                        let is_cmp = matches!(op_str, "<" | "<=" | ">" | ">=");
                        let sym = if is_cmp { "<=>" } else { op_str };
                        if !has_op(sym) {
                            self.error(
                                codes::TYPE_MISMATCH,
                                *span,
                                format!(
                                    "`{op_str}` needs an `impl Operator<\"{sym}\", …> for {name}`"
                                ),
                            );
                        }
                    }
                }
            }
            Expr::Call {
                callee,
                args,
                bang,
                span,
            } => {
                // A method callee is a `Field` node, but its name is a method,
                // not a field — check the receiver and let `check_method_exists`
                // judge the name, so one mistake yields one diagnostic.
                match callee.as_ref() {
                    Expr::Field { base, .. } => self.check_expr(base, sym),
                    _ => self.check_expr(callee, sym),
                }
                for a in args {
                    self.check_expr(a, sym);
                }
                if *bang {
                    self.check_format_arity(callee, args);
                } else if matches!(callee.as_ref(), Expr::Field { .. }) {
                    self.check_method_exists(callee, sym);
                }
                // Reset assertion is normally level-sensitive inside the
                // design clock's event block. Edge-detecting a conventionally
                // named reset creates an accidental second clock domain.
                if let Expr::Field { base, field, .. } = callee.as_ref() {
                    let reset_name = path_string(base).is_some_and(|name| {
                        let leaf = name.rsplit('.').next().unwrap_or(&name);
                        leaf.eq_ignore_ascii_case("rst")
                            || leaf.eq_ignore_ascii_case("reset")
                            || leaf.ends_with("_rst")
                            || leaf.ends_with("_reset")
                    });
                    if reset_name && matches!(field.text.as_str(), "rising" | "falling" | "edge") {
                        self.sink.emit(
                            Diagnostic::warning(format!(
                                "reset signal is edge-detected with `.{}`",
                                field.text
                            ))
                            .with_code(codes::SUSPICIOUS_RESET)
                            .at(*span)
                            .help("test the reset level inside the design's clocked block instead"),
                        );
                    }
                }
                // A constant conversion argument must FIT the target
                // (spec 3.17/3.26): `unsigned[4](300)` is a compile-time error,
                // like `let b: Byte = 300`. Dynamic values get simulation
                // range checks later (with the S3 reporting machinery).
                self.check_conversion_fit(callee, args, e);
                self.check_generic_bounds(callee, args, sym);
                self.check_call_arity(callee, args);
                self.check_runtime_call_arity(callee, args);
            }
            Expr::Construct { args, .. } => {
                for c in args {
                    if let Some(v) = &c.value {
                        self.check_expr(v, sym);
                    }
                }
                // `{ .a = '1', .a = '0' }` silently kept one of them.
                let mut seen: HashSet<&str> = HashSet::new();
                for c in args {
                    let Some(f) = &c.field else { continue };
                    if !seen.insert(f.text.as_str()) {
                        self.error(
                            codes::DUPLICATE_ITEM,
                            f.span,
                            format!("field `{}` is given twice in this literal", f.text),
                        );
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
                // std owns which prefixes exist (`impl Prefix<sym, _> for T`): when it is
                // in scope, a prefix it doesn't declare is unknown. When std is
                // absent (some unit tests) the compiler still recognizes its
                // intrinsic radix prefixes — mirroring the suffix fs/Hz fallback.
                // (A plain string `"1X10"` needs no prefix; `b"…"` is gone.)
                if !self.prefix_types.is_empty()
                    && !self.prefix_types.contains_key(&base.to_string())
                {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        format!(
                            "unknown bit-string prefix `{base}` — no `impl Prefix` declares it"
                        ),
                    );
                    return;
                }
                // Evaluation is a compiler intrinsic until const string ops
                // exist, so only the known radix prefixes carry an alphabet.
                let (ok, kind) = match *base {
                    'x' => (digits.chars().all(|c| c.is_ascii_hexdigit()), "hex"),
                    'o' => (digits.chars().all(|c| ('0'..='7').contains(&c)), "octal"),
                    _ => {
                        self.error(
                            codes::TYPE_MISMATCH,
                            *span,
                            format!("bit-string prefix `{base}` has no compiler evaluation yet"),
                        );
                        return;
                    }
                };
                if digits.is_empty() || !ok {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        format!("invalid {kind} bit-string literal `{base}\"{digits}\"`"),
                    );
                }
            }
            Expr::Int { .. } | Expr::CharLit { .. } | Expr::StrLit { .. } | Expr::Path(_) => {}
        }
    }

    /// Whether a system event/history attribute can observe this type.
    /// Named values are classified from their resolved declaration instead of
    /// being accepted blindly (in particular, entity instances are not data).
    fn is_digital_ty(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Error => false,
            Ty::Named(id) => self
                .resolved
                .kind_of(*id)
                .is_some_and(|kind| matches!(kind, DefKind::Struct | DefKind::Enum)),
            Ty::Array { elem, .. } => self.is_digital_ty(elem),
            _ => true,
        }
    }

    // --- type inference core ------------------------------------------------

    /// Best-effort type of an expression given the in-scope value table. Unknown
    /// or unsupported cases yield [`Ty::Error`], which suppresses dependent
    /// checks rather than producing a false positive.
    fn type_of(&self, e: &Expr, sym: &HashMap<String, Ty>) -> Ty {
        let ty = self.infer_type_of(e, sym);
        self.expr_types
            .borrow_mut()
            .insert(expr_span(e), ty.clone());
        ty
    }

    fn infer_type_of(&self, e: &Expr, sym: &HashMap<String, Ty>) -> Ty {
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
                            d.name == *ty && matches!(d.kind, DefKind::Struct | DefKind::Enum)
                        })
                        .map(|i| Ty::Named(crate::resolve::DefId(i as u32)))
                        .unwrap_or(Ty::Error);
                }
                if suffix_scale(&suffix.text).is_some() {
                    Ty::Integer
                } else {
                    Ty::Error
                }
            }
            Expr::BitStrLit { base, digits, .. } => {
                // Only the intrinsic radix prefixes have a known width; an
                // unknown prefix is `Ty::Error` so its diagnostic doesn't
                // cascade into a spurious width mismatch.
                let bits = match *base {
                    'x' => 4,
                    'o' => 3,
                    _ => return Ty::Error,
                };
                Ty::Array {
                    elem: Box::new(self.ty_from_head("Logic")),
                    family: Some("unsigned".to_string()),
                    len: digits.len() as u32 * bits,
                }
            }
            // A char literal defaults to `Char`; an annotation/target
            // overrides it (Bit/Logic/enum) via `assignable`.
            Expr::CharLit { .. } => Ty::Char,
            // A string literal is `string` = `Char[N]`.
            Expr::StrLit { text, .. } => Ty::Array {
                elem: Box::new(Ty::Char),
                len: text.chars().count() as u32,
                family: None,
            },
            Expr::Path(p) => {
                if p.segments.len() == 1 {
                    sym.get(&p.segments[0].text).cloned().unwrap_or(Ty::Error)
                } else {
                    // `Enum::Variant` has the enum's type, not the variant's.
                    // `Bool`'s variants (`true`/`false`, desugared to
                    // `Bool::true`) are ordinary enum values.
                    match self
                        .resolved
                        .resolved(p.span)
                        .and_then(|id| self.resolved.def(id))
                    {
                        Some(d) if d.kind == DefKind::EnumVariant => {
                            // The enum *named at the use site* decides the
                            // type, not the one that declares the variant.
                            // `enum Mid(Base)` inherits Base's variants, so
                            // `Mid::B` resolves to Base's `B` — typing it as
                            // `Base` would leave a newtype's own variants
                            // impossible to assign to a value of that newtype.
                            let qualifier = p
                                .segments
                                .first()
                                .and_then(|s| self.resolved.resolved(s.span))
                                .filter(|id| self.resolved.kind_of(*id) == Some(DefKind::Enum));
                            match qualifier.or(d.parent) {
                                Some(pid) => Ty::Named(pid),
                                None => Ty::Error,
                            }
                        }
                        _ => self.named_ty(p.span),
                    }
                }
            }
            Expr::SysAttr { base, attr, .. } => match attr.text.as_str() {
                // `::event` is Bool; the edge helpers are `ClockLike` methods now.
                "event" => self.ty_from_head("Bool"),
                "old" => self.type_of(base, sym),
                "length" | "high" | "low" | "left" | "right" => Ty::Integer,
                "ascending" => self.ty_from_head("Bool"),
                _ => Ty::Error,
            },
            Expr::Binary { op, lhs, rhs, .. } => {
                if is_comparison(op) {
                    return self.ty_from_head("Bool");
                }
                let lhs_ty = self.type_of(lhs, sym);
                let rhs_ty = self.type_of(rhs, sym);
                let op_str = crate::syntax::pretty::bin_op(op);
                let tr = op_str;
                if let (Some(owner), Some(input)) = (self.ty_head(&lhs_ty), self.ty_head(&rhs_ty)) {
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
                // (`100 / r` with r: signed[8] is an signed[8], via the std
                // `impl Div<signed> for integer`).
                if matches!(lhs_ty, Ty::Integer) {
                    if let r @ Ty::Array {
                        family: Some(_), ..
                    } = self.type_of(rhs, sym)
                    {
                        return r;
                    }
                }
                // A mixed-operand operator impl (`10 + 5i`) yields the
                // impl-owning operand's type.
                if !matches!(lhs_ty, Ty::Named(_)) {
                    if let Ty::Named(id) = self.type_of(rhs, sym) {
                        let has_impl = self.resolved.def(id).map(|d| &d.name).is_some_and(|name| {
                            let tr = crate::syntax::pretty::bin_op(op);
                            self.has_impl(tr, name)
                        });
                        if has_impl {
                            return Ty::Named(id);
                        }
                    }
                }
                lhs_ty
            }
            Expr::Unary {
                op: UnOp::Not, rhs, ..
            } => {
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
            // A concatenation is an anonymous packed Logic array of unknown width.
            Expr::Concat { .. } => Ty::Array {
                elem: Box::new(self.ty_from_head("Logic")),
                family: Some("unsigned".to_string()),
                len: 0,
            },
            // An array literal: element type from the first element, length
            // from the count.
            Expr::Array { elems, .. } => {
                let elem = elems
                    .first()
                    .map(|e| self.type_of(e, sym))
                    .unwrap_or(Ty::Error);
                Ty::Array {
                    elem: Box::new(elem),
                    len: elems.len() as u32,
                    family: None,
                }
            }
            Expr::Range { .. } | Expr::PartialRange { .. } => self.ty_from_head("Range"),
            Expr::Index { base, index, .. } => {
                let base_ty = self.type_of(base, sym);
                let is_range = matches!(
                    index.as_ref(),
                    Expr::Range { .. } | Expr::PartialRange { .. }
                );
                match &base_ty {
                    Ty::Array { elem, family, .. } if is_range => {
                        let width = explicit_range_len(index).unwrap_or(0);
                        if width == 1 {
                            elem.as_ref().clone()
                        } else {
                            Ty::Array {
                                elem: elem.clone(),
                                family: family.clone(),
                                len: width,
                            }
                        }
                    }
                    Ty::Array { elem, len, .. } if !is_range => {
                        if signed_lit(index).is_some_and(|i| i < 0 || i as u64 >= *len as u64) {
                            Ty::Error
                        } else {
                            elem.as_ref().clone()
                        }
                    }
                    _ => {
                        let Some(owner) = self.type_kind_name(&base_ty) else {
                            return Ty::Error;
                        };
                        let input = if is_range {
                            Some("Range".to_string())
                        } else {
                            self.type_kind_name(&self.type_of(index, sym))
                        };
                        let output =
                            self.index_sigs
                                .get(&("Index".to_string(), owner))
                                .and_then(|sigs| {
                                    sigs.iter()
                                        .find(|(i, _)| i.is_none() || i.as_ref() == input.as_ref())
                                        .and_then(|(_, output)| output.as_deref())
                                });
                        output
                            .map(|name| self.ty_from_head(name))
                            .unwrap_or(Ty::Error)
                    }
                }
            }
            // Conversion expressions type as their target (spec 3.17):
            // `unsigned[16](x)`, `signed[8](x)`, `integer(x)`, `resize(x, n)`.
            Expr::Call { callee, args, .. } => match callee.as_ref() {
                Expr::Index { base, index, .. } => {
                    let head = match base.as_ref() {
                        Expr::Path(p) if p.segments.len() == 1 => p.segments[0].text.as_str(),
                        _ => "",
                    };
                    let w = signed_lit(index).unwrap_or(0).max(0) as u32;
                    match self.vector_families.get(head) {
                        Some(_) => Ty::Array {
                            elem: Box::new(self.ty_from_head("Logic")),
                            family: Some(head.to_string()),
                            len: w,
                        },
                        None => Ty::Error,
                    }
                }
                Expr::Path(p) if p.segments.len() == 1 => match p.segments[0].text.as_str() {
                    // A named struct/enum: a `From` conversion, typed as the
                    // target (fn calls and kernel conversions fall through).
                    name if name != "integer"
                        && name != "resize"
                        && match self.path_ty(p) {
                            Ty::Named(id) => self
                                .resolved
                                .def(id)
                                .is_some_and(|d| matches!(d.kind, DefKind::Struct | DefKind::Enum)),
                            _ => false,
                        } =>
                    {
                        self.path_ty(p)
                    }
                    "integer" => Ty::Integer,
                    "Char" => self.ty_from_head("Char"),
                    // resize keeps the argument's family at the new width.
                    "resize" => {
                        let w = args.get(1).and_then(signed_lit).unwrap_or(0).max(0) as u32;
                        let family = match args.first().map(|a| self.type_of(a, sym)) {
                            Some(Ty::Array { family, .. }) => family,
                            _ => None,
                        };
                        Ty::Array {
                            elem: Box::new(self.ty_from_head("Logic")),
                            family,
                            len: w,
                        }
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
            Expr::Field { .. } => Ty::Error,
        }
    }

    /// The type-head name used to key impl methods: a named type's def name,
    /// a kernel type's spelling, or the nominal family of an indexed array.
    fn ty_head(&self, t: &Ty) -> Option<String> {
        Some(match t {
            Ty::Named(id) => self.resolved.def(*id)?.name.clone(),
            Ty::Real => "real".to_string(),
            Ty::Integer => "integer".to_string(),
            Ty::Char => "Char".to_string(),
            Ty::Array {
                family: Some(name), ..
            } => name.clone(),
            _ => return None,
        })
    }

    fn ty_from_head(&self, name: &str) -> Ty {
        match name {
            "integer" => Ty::Integer,
            "real" => Ty::Real,
            "Char" => Ty::Char,
            name if self.is_vector_family(name) => Ty::Array {
                elem: Box::new(self.ty_from_head("Logic")),
                family: Some(name.to_string()),
                len: 0,
            },
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
            Ty::Array {
                family: Some(name), ..
            } => Some(name),
            _ => None,
        }
    }

    /// A constant initializer must lie inside a value-range-constrained
    /// numeric type (`let b: integer<0..255> = 300;` is an error). Literal
    /// bounds only; named ranges and dynamic values are runtime checks later.
    /// The declared bounds of a ranged numeric (`integer<left..right>`), resolving
    /// one alias hop (`using Byte = integer<0..255>`). `None` for every other
    /// type.
    fn declared_range(&self, decl_ty: &Type) -> Option<(i64, i64)> {
        let resolved;
        let t = match decl_ty {
            Type::Path(p) if p.segments.len() == 1 => match self.aliases.get(&p.segments[0].text) {
                Some(a) => {
                    resolved = a.clone();
                    &resolved
                }
                None => decl_ty,
            },
            _ => decl_ty,
        };
        let Type::Generic { base, args, .. } = t else {
            return None;
        };
        let Type::Path(p) = base.as_ref() else {
            return None;
        };
        if p.segments.last().map(|s| s.text.as_str()) != Some("integer") {
            return None;
        }
        let [GenericArg::Positional(Expr::Range { lo, hi, .. })] = args.as_slice() else {
            return None;
        };
        Some((signed_lit(lo)?, signed_lit(hi)?))
    }

    /// `y = 50` where `y: integer<0..10>`. The initializer form was checked,
    /// the assignment form was not — and the value wraps to the storage width
    /// (50 -> 2), so the runtime range assert saw an in-range value and the
    /// violation vanished.
    fn check_assign_range(
        &mut self,
        target: &Expr,
        value: &Expr,
        ranged: &HashMap<String, (i64, i64)>,
    ) {
        let Some(name) = target_root_name(target) else {
            return;
        };
        let Some(&(lo, hi)) = ranged.get(&name) else {
            return;
        };
        let Some(v) = Self::const_literal(value) else {
            return;
        };
        if v < lo || v > hi {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(value),
                format!("value {v} is outside the range {lo}..{hi}"),
            );
        }
    }

    fn check_value_range(&mut self, decl_ty: &Type, value: &Expr) {
        // Resolve one alias hop (`using Byte = integer<0..255>`).
        let resolved;
        let t = match decl_ty {
            Type::Path(p) if p.segments.len() == 1 => match self.aliases.get(&p.segments[0].text) {
                Some(a) => {
                    resolved = a.clone();
                    &resolved
                }
                None => decl_ty,
            },
            _ => decl_ty,
        };
        let Type::Generic { base, args, .. } = t else {
            return;
        };
        let Type::Path(p) = base.as_ref() else { return };
        if p.segments.last().map(|s| s.text.as_str()) != Some("integer") {
            return;
        }
        let [GenericArg::Positional(Expr::Range { lo, hi, .. })] = args.as_slice() else {
            return;
        };
        let (Some(a), Some(b)) = (signed_lit(lo), signed_lit(hi)) else {
            return;
        };
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

    fn ast_ty(&self, t: &Type) -> Ty {
        match t {
            Type::Path(p) => self.path_ty(p),
            Type::Indexed { base, index, .. } => {
                // Unconstrained (`Char[]`): width 0 = "set at use".
                let width = index.as_deref().map(width_of).unwrap_or(0);
                match self.ast_ty(base) {
                    // The *first* index on a vector family sets its width
                    // (`unsigned[8]`). A *second* index makes an array of those
                    // vectors (`unsigned[8][4]` = 4 elements, each 8 wide).
                    Ty::Array {
                        elem,
                        len: 0,
                        family: Some(family),
                    } => Ty::Array {
                        elem,
                        len: width,
                        family: Some(family),
                    },
                    v @ Ty::Array {
                        family: Some(_), ..
                    } => Ty::Array {
                        elem: Box::new(v),
                        len: width,
                        family: None,
                    },
                    // An index on an *unconstrained* array fills its hole
                    // rather than nesting: `string[5]` is `Char[5]`, not
                    // `Char[0][5]` (`using string = Char[]`, std::text). The
                    // lowerer already did this; the checker rejected the form.
                    Ty::Array {
                        elem,
                        len: 0,
                        family: None,
                    } => Ty::Array {
                        elem,
                        len: width,
                        family: None,
                    },
                    other => Ty::Array {
                        elem: Box::new(other),
                        len: width,
                        family: None,
                    },
                }
            }
            Type::Generic { base, .. } => self.ast_ty(base),
            Type::View { view, .. } => self.path_ty(view),
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
                "integer" => Ty::Integer,
                "real" => Ty::Real,
                "Char" => Ty::Char,
                // Elaboration-time range constants (`const BYTE: range`);
                // opaque to value checking.
                "range" => Ty::Error,
                name => match self.aliases.get(name) {
                    // A cyclic alias has no type; `Error` also suppresses the
                    // follow-on diagnostics the cycle would otherwise cause.
                    Some(_) if !self.expanding.borrow_mut().insert(name.to_string()) => Ty::Error,
                    Some(t) => {
                        let t = t.clone();
                        let ty = self.ast_ty(&t);
                        self.expanding.borrow_mut().remove(name);
                        ty
                    }
                    // A bit-vector family (`struct F : Logic[]`): width applies
                    // via `F[N]` (ast_ty's Indexed).
                    None if self.is_vector_family(name) => Ty::Array {
                        elem: Box::new(self.ty_from_head("Logic")),
                        family: Some(name.to_string()),
                        len: 0,
                    },
                    None => self.named_ty(p.span),
                },
            }
        } else {
            self.named_ty(p.span)
        }
    }

    fn error(&mut self, code: &'static str, span: Span, msg: String) {
        self.sink
            .emit(Diagnostic::error(msg).with_code(code).at(span));
    }

    fn error_with_help(&mut self, code: &'static str, span: Span, msg: String, help: String) {
        self.sink
            .emit(Diagnostic::error(msg).with_code(code).at(span).help(help));
    }

    fn warn(&mut self, code: &'static str, span: Span, msg: String, help: &str) {
        self.sink.emit(
            Diagnostic::warning(msg)
                .with_code(code)
                .at(span)
                .help(help.to_string()),
        );
    }

    /// The enum name if `t` is a symbolic enum value (`Bit`/`Logic`/`Bool` or a
    /// user `enum`) — the types whose values are written as char/variant
    /// literals, not numbers. `None` for numerics (`unsigned`/`signed`/`integer`/
    /// `real`), `Char`, and non-enums.
    fn enum_operand_name(&self, t: &Ty) -> Option<String> {
        match t {
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
        Type::View { view, .. } => Some(view.span),
    }
}

fn type_head_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Path(p) => p.segments.first().map(|s| s.text.as_str()),
        Type::Generic { base, .. } | Type::Indexed { base, .. } => type_head_name(base),
        Type::View { view, .. } => view.segments.last().map(|i| i.text.as_str()),
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

/// The view/backing pair of an applied view type, for looking up per-leaf
/// directions.
fn view_key(ty: &Type, views: &HashMap<String, Type>) -> Option<String> {
    let key = match ty {
        Type::View { view, target, .. } => {
            format!("{}@{}", view.segments.last()?.text, type_head_name(target)?)
        }
        _ => type_head_name(ty)?.to_string(),
    };
    views.contains_key(&key).then_some(key)
}

fn declared_view_key(view: &ViewDecl) -> String {
    let target = type_head_name(&view.target).unwrap_or("<error>");
    format!("{}@{target}", view.name.text)
}

fn type_identity(ty: &Type) -> Option<String> {
    match ty {
        Type::View { view, target, .. } => Some(format!(
            "{}@{}",
            view.segments.last()?.text,
            type_head_name(target)?
        )),
        _ => type_head_name(ty).map(str::to_string),
    }
}

fn is_blanket_array_impl(im: &ImplDecl) -> bool {
    let Type::Indexed {
        base, index: None, ..
    } = &im.target
    else {
        return false;
    };
    let Some(head) = type_head_name(base) else {
        return false;
    };
    im.params.params.iter().any(|param| param.name.text == head)
}

fn blanket_requirement(im: &ImplDecl) -> Option<String> {
    let Type::Indexed { base, .. } = &im.target else {
        return None;
    };
    let parameter = type_head_name(base)?;
    let bound = im
        .params
        .params
        .iter()
        .find(|candidate| candidate.name.text == parameter)?
        .bound
        .as_ref()?;
    match bound {
        Type::Generic { base, args, .. } if type_head_name(base) == Some("Operator") => {
            args.first().and_then(|argument| match argument {
                GenericArg::Positional(Expr::StrLit { text, .. }) => Some(text.clone()),
                _ => None,
            })
        }
        _ => type_head_name(bound).map(str::to_string),
    }
}

fn is_liftable_array_key(key: &str) -> bool {
    matches!(key, "Resolve" | "and" | "or" | "not")
}

/// Width of a bracketed type index when it is a literal (`unsigned[8]` -> 8);
/// otherwise `0`, meaning "parametric / not yet known".
fn width_of(index: &Expr) -> u32 {
    signed_lit(index)
        .and_then(|width| u32::try_from(width).ok())
        .unwrap_or(0)
}

fn is_comparison(op: &BinOp) -> bool {
    matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
    )
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
        (Real, Real) | (Char, Char) => true,
        // `integer` is the number kernel; it coerces to/from any bit vector
        // (a unsigned[8] accepts `42`, and a vector's value is an integer).
        (Integer, Integer) => true,
        (
            Integer,
            Array {
                family: Some(_), ..
            },
        )
        | (
            Array {
                family: Some(_), ..
            },
            Integer,
        ) => true,
        (Named(a), Named(b)) => a == b,
        // All indexed collections use the same shape. Nominal families do not
        // gate assignment; their behavior lives in trait implementations.
        (
            Array {
                elem: ea, len: la, ..
            },
            Array {
                elem: eb, len: lb, ..
            },
        ) => compatible(ea, eb) && (*la == 0 || *lb == 0 || la == lb),
        _ => false,
    }
}

/// When a string literal (`"c"`) is used where a character, logic scalar, or
/// bit vector is expected, explain that `"..."` is a *string* (a `Char` array)
/// and point at the right form: `'c'` for a single value, `b"..."` for a bit
/// vector. Assigning a string to a `Char` array is fine, so no hint there.
fn strlit_help(lhs: &Ty, value: &Expr) -> Option<String> {
    let Expr::StrLit { text, .. } = value else {
        return None;
    };
    match lhs {
        Ty::Char | Ty::Named(_) => Some(if text.chars().count() == 1 {
            format!("`\"{text}\"` is a string; for a single {} value use a character literal `'{text}'`", ty_name(lhs))
        } else {
            format!("`\"{text}\"` is a string (a `Char` array); a {} is one character, written `'c'`", ty_name(lhs))
        }),
        Ty::Array {
            family: Some(_),
            ..
        } => Some(format!(
            "`\"{text}\"` is a string; for a bit vector use a bit-string literal `b\"{text}\"` (binary) or `x\"...\"` (hex)"
        )),
        Ty::Array { .. } => None,
        _ => None,
    }
}

fn ty_name(t: &Ty) -> String {
    match t {
        Ty::Real => "real".to_string(),
        Ty::Integer => "integer".to_string(),
        Ty::Char => "Char".to_string(),
        Ty::Named(_) => "a named type".to_string(),
        Ty::Array {
            family: Some(name),
            len: 0,
            ..
        } => name.clone(),
        Ty::Array {
            family: Some(name),
            len,
            ..
        } => format!("{name}[{len}]"),
        Ty::Array { .. } => "an array".to_string(),
        Ty::Error => "<unknown>".to_string(),
    }
}

/// The value of an integer literal, allowing a leading unary minus.
fn signed_lit(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int { text, .. } => i64::try_from(unsigned_lit_text(text)?).ok(),
        Expr::Unary {
            op: UnOp::Neg, rhs, ..
        } => match rhs.as_ref() {
            // Permit the full signed domain, including `-0x8000_0000_0000_0000`,
            // whose unsigned magnitude cannot first pass through i64.
            Expr::Int { text, .. } => i64::try_from(-i128::from(unsigned_lit_text(text)?)).ok(),
            _ => signed_lit(rhs)?.checked_neg(),
        },
        _ => None,
    }
}

fn unsigned_lit_text(text: &str) -> Option<u64> {
    let text = text.replace('_', "");
    if let Some(digits) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(digits, 16).ok()
    } else if let Some(digits) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
        u64::from_str_radix(digits, 2).ok()
    } else {
        text.parse().ok()
    }
}

fn explicit_range_len(e: &Expr) -> Option<u32> {
    let Expr::Range { lo, hi, .. } = e else {
        return None;
    };
    let lo = signed_lit(lo)?;
    let hi = signed_lit(hi)?;
    u32::try_from((i128::from(lo) - i128::from(hi)).unsigned_abs())
        .ok()?
        .checked_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::FileId;

    const VEC: &str = "\n\
        enum Bit { '0', '1' }\n\
        enum Logic { '0', '1', 'Z', 'X', 'U', 'W', 'L', 'H', '-' }\n\
        enum Bool { false, true }\n\
        enum Ordering { Less, Equal, Greater }\n\
        trait Boolean { fn as_bool(self) -> Bool; }\n\
        trait Vector {}\n\
        trait ClockLike { fn rising(self) -> Bool; }\n\
        impl Boolean for Bit { fn as_bool(self) -> Bool { return true; } }\n\
        impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } }\n\
        impl ClockLike for Bit { fn rising(self) -> Bool { return false; } }\n\
        impl Operator<\"and\", Bool, Bool> for Bool { fn apply(self, rhs: Bool) -> Bool { return self; } }\n\
        impl Operator<\"or\", Bool, Bool> for Bool { fn apply(self, rhs: Bool) -> Bool { return self; } }\n\
        impl Operator<\"not\", Bool, Bool> for Bool { fn apply(self) -> Bool { return self; } }\n\
        impl Operator<\"and\", Bit, Bit> for Bit { fn apply(self, rhs: Bit) -> Bit { return self; } }\n\
        impl Operator<\"or\", Bit, Bit> for Bit { fn apply(self, rhs: Bit) -> Bit { return self; } }\n\
        impl Operator<\"not\", Bit, Bit> for Bit { fn apply(self) -> Bit { return self; } }\n\
        impl Operator<\"and\", Logic, Logic> for Logic { fn apply(self, rhs: Logic) -> Logic { return self; } }\n\
        impl Operator<\"or\", Logic, Logic> for Logic { fn apply(self, rhs: Logic) -> Logic { return self; } }\n\
        impl Operator<\"not\", Logic, Logic> for Logic { fn apply(self) -> Logic { return self; } }\n\
        impl<T: Operator<\"and\", T, T>> Operator<\"and\", T, T> for T[] { fn apply(self, rhs: T[]) -> T[] { return self; } }\n\
        impl<T: Operator<\"or\", T, T>> Operator<\"or\", T, T> for T[] { fn apply(self, rhs: T[]) -> T[] { return self; } }\n\
        impl<T: Operator<\"not\", T, T>> Operator<\"not\", T, T> for T[] { fn apply(self) -> T[] { return self; } }\n\
        impl Vector for unsigned {}\n\
        struct unsigned(Logic[]);\n\
        impl Operator<\"/\", unsigned, unsigned> for unsigned { fn apply(self, rhs: unsigned) -> unsigned { return self; } }\n\
        impl Operator<\"<=>\", unsigned, Ordering> for unsigned { fn apply(self, rhs: unsigned) -> Ordering { return Equal; } }\n\
        impl Vector for signed {}\n\
        struct signed(Logic[]);\n";

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

    #[test]
    fn typed_records_expression_types() {
        let src = format!(
            "module m;\nentity E {{ a: unsigned[8] in, y: Logic out, }}\n\
             impl E {{ y = a[0]; }}\n{VEC}"
        );
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
        let typed = check(std::slice::from_ref(&module), &resolved, &mut sink);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        let logic = resolved
            .defs()
            .iter()
            .position(|def| def.name == "Logic")
            .map(|index| Ty::Named(crate::resolve::DefId(index as u32)))
            .expect("Logic definition");
        assert!(
            typed.expr_types().values().any(|ty| *ty == logic),
            "the indexed vector element type is retained"
        );
    }

    #[test]
    fn numeric_separators_and_based_type_indices_are_checked_at_full_width() {
        let errors = check_src(
            "module m;\n\
             entity E {\n\
               a: unsigned[1_28] in,\n\
               b: Logic[0x3..0b0] in,\n\
               y: Logic out,\n\
             }\n\
             impl E { y = a[0x7f] and b[0b11]; }\n",
        );
        assert_eq!(
            errors, 0,
            "literal spelling must not turn valid widths or indices into unknown widths"
        );
    }

    #[test]
    fn loop_variables_shadow_outer_types_with_element_types() {
        let errors = check_src(
            "module m;\n\
             #[test] entity T {}\n\
             impl T {\n\
               let item: real = 3.5;\n\
               let bits: Bit[2] = \"10\";\n\
               for item in 0..1 {\n\
                 assert!(item >= 0, \"integer range item\");\n\
                 for item in bits {\n\
                   assert!(item == '0' or item == '1', \"Bit array item\");\n\
                 }\n\
                 assert!(item <= 1, \"integer item restored\");\n\
               }\n\
               assert!(item == 3.5, \"outer real restored\");\n\
             }\n",
        );
        assert_eq!(errors, 0, "each loop body needs its own value-type scope");
    }

    fn diag_codes(src: &str) -> Vec<String> {
        let src = format!("{src}{VEC}");
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        sink.diagnostics()
            .iter()
            .map(|d| format!("{:?}", d.code))
            .collect()
    }

    #[test]
    fn suspicious_logic_compare_warns_on_integer_literal() {
        let warns = |src: &str| diag_codes(src).iter().any(|c| c.contains("W-P008"));
        // Bit / Logic / enum vs a bare integer literal → W-P008.
        assert!(
            warns("module m;\nentity E { b: Bit in, y: Bit out, }\nimpl E { y = if b == 1 { '1' } else { '0' }; }\n"),
            "Bit == 1 should warn"
        );
        assert!(
            warns("module m;\nenum State { Idle, Run }\nentity E { y: Bit out, }\nimpl E { let s: State; y = if s == 0 { '1' } else { '0' }; }\n"),
            "enum == 0 should warn"
        );
        // Numeric vector vs integer, and Bit vs a value literal → no warning.
        assert!(
            !warns("module m;\nentity E { a: unsigned[8] in, y: Bit out, }\nimpl E { y = if a == 5 { '1' } else { '0' }; }\n"),
            "unsigned == 5 must not warn"
        );
        assert!(
            !warns("module m;\nentity E { b: Bit in, y: Bit out, }\nimpl E { y = if b == '1' { '1' } else { '0' }; }\n"),
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
             view Source for S { valid out, ready in }\n\
             entity P { bus: S Source }\n\
             impl P { bus.valid = '1'; bus.ready = '1'; }\n",
        );
        assert_eq!(bad, 1, "driving the `in` leaf bus.ready must error");

        // Driving only the `out` leaves is fine.
        let ok = check_src(
            "module m;\n\
             struct S { valid: Bit, ready: Bit, }\n\
             view Source for S { valid out, ready in }\n\
             entity P { bus: S Source, r: Bit out }\n\
             impl P { bus.valid = '1'; r = bus.ready; }\n",
        );
        assert_eq!(ok, 0, "driving out leaves + reading in leaves is fine");
    }

    #[test]
    fn views_overload_by_backing_struct_in_trait_impls() {
        let errors = check_src(
            "module m;\n\
             trait Send<T> { fn send(self, value: T); }\n\
             struct Stream { data: Bit, ready: Bit }\n\
             struct Queue { data: Bit, full: Bit }\n\
             view Source for Stream { data out, ready in }\n\
             view Source for Queue { data out, full in }\n\
             impl Send<Bit> for Stream Source {\n\
               fn send(self, value: Bit) { self.data = value; }\n\
             }\n\
             impl Send<Bit> for Queue Source {\n\
               fn send(self, value: Bit) { self.data = value; }\n\
             }\n\
             entity StreamProducer { bus: Stream Source }\n\
             entity QueueProducer { bus: Queue Source }\n",
        );
        assert_eq!(errors, 0, "the view/backing pair is the nominal identity");
    }

    #[test]
    fn method_return_type_propagates() {
        // A method returning `Logic` used directly as a condition must error
        // (Logic isn't Boolean), proving the return type flows into checks.
        let bad = "module m;\n\
            struct S { v: Logic, }\n\
            impl S { fn ready(self) -> Logic { return self.v; } }\n\
            entity E { o: Logic out }\n\
            impl E { let s: S; if s.ready() { o = '1'; } }\n";
        assert_eq!(
            check_src(bad),
            1,
            "Logic-returning method as a condition should error"
        );

        // A `Bool`-returning method is a valid condition — no error.
        let good = "module m;\n\
            struct S { v: Logic, }\n\
            impl S { fn ready(self) -> Bool { return true; } }\n\
            entity E { o: Logic out }\n\
            impl E { let s: S; if s.ready() { o = '1'; } }\n";
        assert_eq!(
            check_src(good),
            0,
            "Bool-returning method as a condition should pass"
        );
    }

    #[test]
    fn string_literal_gets_a_targeted_hint() {
        let sp = crate::diag::Span::new(FileId(0), 0..1);
        let s = |t: &str| Expr::StrLit {
            text: t.to_string(),
            span: sp,
        };
        // A named scalar points at the character literal.
        let h = strlit_help(&Ty::Named(crate::resolve::DefId(0)), &s("0")).unwrap();
        assert!(h.contains("'0'"), "{h}");
        // A bit vector points at the bit-string literal.
        let h = strlit_help(
            &Ty::Array {
                elem: Box::new(Ty::Named(crate::resolve::DefId(0))),
                family: Some("unsigned".to_string()),
                len: 4,
            },
            &s("0101"),
        )
        .unwrap();
        assert!(h.contains("b\"0101\""), "{h}");
        // Assigning a string to a Char array is correct — no hint.
        let str_ty = Ty::Array {
            elem: Box::new(Ty::Char),
            len: 2,
            family: None,
        };
        assert!(strlit_help(&str_ty, &s("hi")).is_none());
    }

    #[test]
    fn vector_names_its_real_family() {
        // A known family displays by name; anonymous vectors fall back to unsigned.
        let int8 = Ty::Array {
            elem: Box::new(Ty::Named(crate::resolve::DefId(0))),
            family: Some("signed".to_string()),
            len: 8,
        };
        assert_eq!(ty_name(&int8), "signed[8]");
        let byte = Ty::Array {
            elem: Box::new(Ty::Named(crate::resolve::DefId(0))),
            family: Some("Byte".to_string()),
            len: 0,
        };
        assert_eq!(ty_name(&byte), "Byte");
        let anon = Ty::Array {
            elem: Box::new(Ty::Named(crate::resolve::DefId(0))),
            family: Some("unsigned".to_string()),
            len: 4,
        };
        assert_eq!(ty_name(&anon), "unsigned[4]");
        // Width still ignores the family: unsigned[8] and signed[8] stay compatible.
        assert!(compatible(
            &int8,
            &Ty::Array {
                elem: Box::new(Ty::Named(crate::resolve::DefId(0))),
                family: Some("unsigned".to_string()),
                len: 8
            }
        ));
    }

    /// The number of warnings with a given code emitted while checking `src`.
    fn warnings(src: &str, code: &str) -> usize {
        let src = format!("{src}{VEC}");
        let src = src.as_str();
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), src, &mut sink);
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        sink.diagnostics()
            .iter()
            .filter(|d| d.code == Some(code))
            .count()
    }

    #[test]
    fn unreachable_match_arms_warn() {
        let base = "module m;\nenum State { Idle, Run, Done }\nentity E { y: Bit out, }\nimpl E {\n  let s: State;\n  match s {\n    ARMS\n  }\n}\n";
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
        let base = "module m;\nenum State { Idle, Run, Done }\nentity E { y: Bit out, }\nimpl E {\n  let s: State;\n  match s {\n    ARMS\n  }\n}\n";
        // Missing `Done` and no `_` -> one warning.
        assert_eq!(
            warnings(
                &base.replace(
                    "ARMS",
                    "State::Idle => { y = '0'; } State::Run => { y = '1'; }"
                ),
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
        let errors = check_src("module m;\nentity E { y: Bit out, }\nimpl E {\n  y = x'ddt;\n}\n");
        assert_eq!(errors, 1);
    }

    #[test]
    fn accepts_digital_sysattrs() {
        let errors = check_src(
            "module m;\nentity E { clk: Bit in, q: Bit out, }\nimpl E {\n  if clk.rising() {\n    q = clk'old;\n  }\n}\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn edge_detected_reset_warns() {
        let src = "module m;\nentity E { reset: Bit in, q: Bit out, }\n\
                   impl E { if reset.rising() { q = '0'; } }\n";
        assert_eq!(warnings(src, codes::SUSPICIOUS_RESET), 1);

        let level = "module m;\nentity E { clk: Bit in, reset: Bit in, q: Bit out, }\n\
                     impl E { if clk.rising() { if reset { q = '0'; } } }\n";
        assert_eq!(warnings(level, codes::SUSPICIOUS_RESET), 0);
    }

    #[test]
    fn rejects_write_to_input_port() {
        let errors = check_src(
            "module m;\nentity E { en: Bit in, y: Bit out, }\nimpl E {\n  en = '1';\n  y = en;\n}\n",
        );
        assert_eq!(errors, 1);
    }

    #[test]
    fn writing_output_is_fine() {
        let errors =
            check_src("module m;\nentity E { en: Bit in, y: Bit out, }\nimpl E {\n  y = en;\n}\n");
        assert_eq!(errors, 0);
    }

    #[test]
    fn rejects_write_to_plain_input_field_or_index() {
        // A field/index of a *plain* `in` port is read-only too.
        let errors = check_src(
            "module m;\nstruct P { x: Bit }\nentity E { a: Bit in, p: P in, y: Bit out, }\n\
             impl E {\n  a = '1';\n  p.x = '1';\n  y = a;\n}\n",
        );
        assert_eq!(errors, 2, "bare `a` and field `p.x` are both rejected");
    }

    #[test]
    fn bare_logic_condition_is_rejected() {
        let errors = check_src(
            "module m;\nentity E { rst: Logic in, y: Bit out, }\nimpl E {\n  if rst {\n    y = '0';\n  }\n}\n",
        );
        assert_eq!(errors, 1);
    }

    #[test]
    fn compared_logic_and_bit_conditions_are_fine() {
        // `rst == '1'` is a comparison (-> Bool); `en` is a Bit. Both valid.
        let errors = check_src(
            "module m;\nentity E { rst: Logic in, en: Bit in, y: Bit out, }\nimpl E {\n  if rst == '1' {\n    y = '0';\n  }\n  if en {\n    y = '1';\n  }\n}\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn attribute_on_wrong_target_is_rejected() {
        // `keep` is declared for `let, port`, not `entity`.
        let errors = check_src("module m;\n#[keep]\nentity E { y: Bit out, }\n");
        assert_eq!(errors, 1);
    }

    #[test]
    fn attribute_on_right_target_is_fine() {
        let errors = check_src("module m;\n#[top]\nentity E { y: Bit out, }\n");
        assert_eq!(errors, 0);
    }

    #[test]
    fn assigning_bool_to_a_bit_port_is_rejected() {
        let errors = check_src(
            "module m;\nentity E { en: Bit in, y: Bit out, }\nimpl E {\n  y = en == en;\n}\n",
        );
        // `en == en` is Bool; `y` is Bit.
        assert_eq!(errors, 1);
    }

    #[test]
    fn integer_and_logic_literals_are_polymorphic() {
        // signed literal -> any unsigned; '1' -> Bit or Logic. No conversions needed.
        let errors = check_src(
            "module m;\nentity E { count: unsigned[8] out, q: Bit out, clk: Bit out, }\nimpl E {\n  let value: unsigned[8] = 0;\n  count = value;\n  q = '1';\n  clk = '0';\n}\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn vector_newtype_forwards_matching_blanket_array_operator() {
        let errors = check_src(
            "module m;\n\
             impl<T: Operator<\"and\", T, T>> Operator<\"and\", T, T> for T[] {\n\
               fn apply(self, rhs: T[]) -> T[] { return self and rhs; }\n\
             }\n\
             struct Flags(Bit[]);\n\
             impl Vector for Flags {}\n\
             entity E { a: Flags[4] in, b: Flags[4] in, y: Flags[4] out }\n\
             impl E { y = a and b; }\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn vector_newtype_does_not_forward_unsatisfied_array_operator() {
        let errors = check_src(
            "module m;\n\
             enum Cell { Off, On }\n\
             impl<T: Operator<\"and\", T, T>> Operator<\"and\", T, T> for T[] {\n\
               fn apply(self, rhs: T[]) -> T[] { return self and rhs; }\n\
             }\n\
             struct Cells(Cell[]);\n\
             impl Vector for Cells {}\n\
             entity E { a: Cells[4] in, b: Cells[4] in, y: Cells[4] out }\n\
             impl E { y = a and b; }\n",
        );
        assert_eq!(errors, 1);
    }

    #[test]
    fn unsupported_blanket_array_operator_is_rejected_until_it_can_lower() {
        let errors = check_src(
            "module m;\n\
             #[precedence = 35]\n\
             impl<T: Operator<\"xor\", T, T>> Operator<\"xor\", T, T> for T[] {\n\
               fn apply(self, rhs: T[]) -> T[] { return self; }\n\
             }\n",
        );
        assert_eq!(errors, 1);
    }

    #[test]
    fn enum_assignment_uses_the_enum_type() {
        let errors = check_src(
            "module m;\nenum State { Idle, Run }\nentity E { s: State out, }\nimpl E {\n  s = State::Idle;\n}\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn bad_initializer_type_is_rejected() {
        let errors = check_src(
            "module m;\nentity E { y: Bit out, }\nimpl E {\n  let flag: Bool = 5;\n  y = '0';\n}\n",
        );
        assert_eq!(errors, 1);
    }

    #[test]
    fn attribute_value_type_is_checked() {
        // `name` expects a string; giving it an signed is an error.
        let bad = check_src("module m;\n#[name = 5]\nentity E { y: Bit out, }\n");
        assert_eq!(bad, 1);
        let good = check_src("module m;\n#[name = \"dut\"]\nentity E { y: Bit out, }\n");
        assert_eq!(good, 0);
    }

    #[test]
    fn operators_on_user_types_need_an_impl() {
        let base = "module m;\nstruct V { a: Bit }\nOPIMPL\nentity E { p: V in, q: V in, y: Bit out, }\nimpl E {\n  let r: V = p + q;\n  y = '0';\n}\n";
        // Without an impl, `+` on a struct is rejected.
        assert_eq!(check_src(&base.replace("OPIMPL\n", "")), 1);
        // With `impl Operator<"+", V, V> for V`, it is accepted.
        assert_eq!(
            check_src(&base.replace(
                "OPIMPL",
                "impl Operator<\"+\", V, V> for V {\n  fn apply(self, rhs: V) -> V {\n    return self;\n  }\n}"
            )),
            0
        );
    }

    #[test]
    fn suffix_traits_define_and_disambiguate_literals() {
        let time = "struct Time { fs: unsigned[48] }\nimpl Suffix<\"s\", integer> for Time {}\n";
        // A Suffix impl's symbol defines the literal's type: Time = 5s passes.
        assert_eq!(
            check_src(&format!(
                "module m;\n{time}entity E {{ y: Bit out, }}\nimpl E {{\n  let t: Time = 5s;\n  y = '0';\n}}\n"
            )),
            0
        );
        // Two types defining the same suffix is an ambiguity error (the
        // cascading init mismatch is separate).
        let score = "struct Score { p: unsigned[8] }\nimpl Suffix<\"s\", integer> for Score {}\n";
        let src = format!(
            "module m;\n{time}{score}entity E {{ y: Bit out, }}\nimpl E {{\n  let t: Time = 5s;\n  y = '0';\n}}\n"
        );
        assert_eq!(warnings(&src, codes::UNKNOWN_NAME), 1);
    }

    #[test]
    fn suffix_and_bitstring_literals_are_checked() {
        // Known unit suffixes and valid bit-strings pass.
        assert_eq!(
            check_src(
                "module m;\nentity E { y: unsigned[8] out, }\nimpl E {\n  let t: integer = 10ns;\n  let f: integer = 100MHz;\n  y = x\"AB\";\n}\n"
            ),
            0
        );
        // An unknown suffix is an error.
        assert_eq!(
            check_src("module m;\nentity E { y: Bit out, }\nimpl E {\n  let c: integer = 5i;\n  y = '0';\n}\n"),
            1
        );
        // Bad digits for the base are an error (`G` is not a hex digit).
        assert_eq!(
            check_src("module m;\nentity E { y: unsigned[8] out, }\nimpl E {\n  y = x\"1G\";\n}\n"),
            1
        );
        // An unknown prefix (no `impl Prefix` declares `q`) is an error.
        assert_eq!(
            check_src(
                "module m;\nentity E { y: unsigned[8] out, }\nimpl E {\n  y = q\"1010\";\n}\n"
            ),
            1
        );
    }

    #[test]
    fn user_type_opts_into_condition_via_boolean() {
        // Without an `impl Boolean for State`, `if state` is rejected.
        let without = check_src(
            "module m;\nenum State { Idle, Run }\nentity E { y: Bit out, }\nimpl E {\n  let state: State;\n  if state {\n    y = '1';\n  }\n}\n",
        );
        assert_eq!(without, 1);

        // With it, the enum becomes usable as a condition.
        let with = check_src(
            "module m;\nenum State { Idle, Run }\nimpl Boolean for State {\n  fn as_bool(self) -> Bool {\n    match self {\n      State::Idle => return false,\n      _ => return true,\n    }\n  }\n}\nentity E { y: Bit out, }\nimpl E {\n  let state: State;\n  if state {\n    y = '1';\n  }\n}\n",
        );
        assert_eq!(with, 0);
    }

    #[test]
    fn char_literal_defaults_to_char_but_takes_annotated_type() {
        // Bare: '0' is a Char.  Annotated / if-expr context: it takes the
        // target type (Bit/Logic), including through an if-expression.
        assert_eq!(
            check_src("module m;\nentity E { y: Bit out, }\nimpl E { y = '0'; }\n"),
            0,
            "'0' assigns to a Bit output"
        );
        assert_eq!(
            check_src("module m;\nentity E { y: Logic out, }\nimpl E { y = '1'; }\n"),
            0,
            "'1' assigns to a Logic output"
        );
        assert_eq!(
            check_src("module m;\nentity E { c: Bit in, y: Bit out, }\nimpl E { y = if c { '1' } else { '0' }; }\n"),
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
        assert!(matches!(
            ty("module m;\nimpl E { y = \"abc\"; }\n"),
            Ty::Array { .. }
        ));
        // `true`/`false` desugar to `Bool::true`/`Bool::false`, so std's `Bool`
        // The enum must be in scope for them to resolve as a named type.
        assert!(matches!(
            ty("module m;\nenum Bool { false, true }\nimpl E { y = true; }\n"),
            Ty::Named(_)
        ));
    }

    #[test]
    fn boolean_ops_reject_non_bit_types() {
        // `and`/`or`/`not` are boolean-per-bit: bit-derived / Boolean only.
        assert_eq!(
            check_src("module m;\nentity E { a: real in, b: real in, y: real out, }\nimpl E { y = a and b; }\n"),
            1,
            "`and` on real is rejected"
        );
        assert_eq!(
            check_src("module m;\nentity E { a: unsigned[8] in, b: unsigned[8] in, y: unsigned[8] out, }\nimpl E { y = a and b; }\n"),
            0,
            "`and` on a bit array is fine (per-bit, returns the array)"
        );
        // integer is a number, not bits — no boolean operators on it.
        assert_eq!(
            check_src("module m;\nentity E { a: integer in, b: integer in, y: integer out, }\nimpl E { y = a and b; }\n"),
            1,
            "`and` on integer variables is rejected"
        );
        // ...but a literal mask coerces to the bit operand's width.
        assert_eq!(
            check_src("module m;\nentity E { a: unsigned[8] in, y: unsigned[8] out, }\nimpl E { y = a and 15; }\n"),
            0,
            "`b and 15` (literal mask) is fine"
        );
        // comparison results are Bool, so boolean ops chain them.
        assert_eq!(
            check_src("module m;\nentity E { a: unsigned[8] in, b: unsigned[8] in, y: Bool out, }\nimpl E { y = (a > b) and (a != b); }\n"),
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
            impl Operator<\"and\", Right, Result> for Left {\n\
              fn apply(self, rhs: Right) -> Result { return Result::Yes; }\n\
            }\n\
            entity E { a: Left in, b: Right in, y: Result out }\n\
            impl E { y = a and b; }\n";
        assert_eq!(
            check_src(src),
            0,
            "the Output parameter types the expression"
        );
    }

    #[test]
    fn custom_operator_selects_input_and_output_templates() {
        let ok = "module m;\n\
            attr precedence: integer for impl;\n\
            trait Operator<op, I, O> { fn apply(self, rhs: I) -> O; }\n\
            enum Left { L } enum Right { R } enum Result { Yes }\n\
            #[precedence = 45]\n\
            impl Operator<\"merge\", Right, Result> for Left {\n\
              fn apply(self, rhs: Right) -> Result { return Result::Yes; }\n\
            }\n\
            entity E { a: Left in, b: Right in, y: Result out }\n\
            impl E { y = a merge b; }\n";
        assert_eq!(check_src(ok), 0);

        let bad = ok.replace("b: Right in", "b: Left in");
        assert_eq!(
            check_src(&bad),
            1,
            "the Input template participates in overload selection"
        );
    }

    /// A literal that cannot fit the operand width made the comparison mask it
    /// (600 -> 88 on a `unsigned[8]`), so the guard compared the wrong value and
    /// silently passed. The wrapped-expression case must still be allowed.
    #[test]
    fn out_of_range_comparison_literal_is_rejected() {
        let ent = "module m;\nentity E { q: unsigned[8] in, y: unsigned[8] out, }\nimpl E { y = ";
        assert_eq!(
            check_src(&format!("{ent}if q == 600 {{ 1 }} else {{ 0 }}; }}\n")),
            1,
            "600 cannot be a unsigned[8]"
        );
        assert_eq!(
            check_src(&format!("{ent}if q == 0 - 3 {{ 1 }} else {{ 0 }}; }}\n")),
            0,
            "a wrapped constant is a real 8-bit pattern (253)"
        );
        assert_eq!(
            check_src(&format!("{ent}if q == 255 {{ 1 }} else {{ 0 }}; }}\n")),
            0,
            "the top of the range still fits"
        );
        // The literal may sit on either side.
        assert_eq!(
            check_src(&format!("{ent}if 600 == q {{ 1 }} else {{ 0 }}; }}\n")),
            1,
            "flagged from the left too"
        );
    }

    /// Fit checking is advisory for expressions that exceed the narrow
    /// evaluator. Host overflow must not abort semantic analysis; later
    /// arbitrary-width lowering retains the expression.
    #[test]
    fn overflowing_conversion_constant_does_not_panic() {
        let src = "module m;\n\
            entity E { y: unsigned[64] out }\n\
            impl E { y = unsigned[64](9223372036854775807 + 1); }\n";
        let _ = check_src(src);
    }

    #[test]
    fn unrepresentable_type_layouts_are_rejected_before_lowering() {
        let range = "module m;\n\
            entity E { y: Logic[-9223372036854775807..9223372036854775807] out }\n\
            impl E {}\n";
        assert_eq!(check_src(range), 1, "the range length exceeds u32");

        let width = "module m;\n\
            entity E { y: unsigned[4294967296] out }\n\
            impl E {}\n";
        assert_eq!(check_src(width), 1, "the width exceeds u32");

        let negative = "module m;\n\
            entity E { y: unsigned[-1] out }\n\
            impl E {}\n";
        assert_eq!(check_src(negative), 1, "negative widths are invalid");
    }

    /// Two variants with the same explicit value are indistinguishable at
    /// runtime — `S::A == S::B` is true, and a waveform cannot tell them apart.
    #[test]
    fn colliding_enum_discriminants_are_reported() {
        assert_eq!(check_src("module m;\nenum S { A = 5, B = 5 }\n"), 1);
        assert_eq!(check_src("module m;\nenum S { A = 5, B = 6 }\n"), 0);
        // Implicit numbering cannot collide.
        assert_eq!(check_src("module m;\nenum S { A, B, C }\n"), 0);
    }

    /// A repeated field in a struct/connection literal silently kept one of
    /// the values; and a type in a diagnostic must be named, not described as
    /// "a named type".
    #[test]
    fn duplicate_literal_field_and_named_type_rendering() {
        let dup = "module m;\nstruct P { a: Bit, b: Bit }\nentity E { y: Bit out, }\n\
                   impl E { let q: P = { .a = '1', .a = '0' }; y = q.a; }\n";
        assert_eq!(check_src(dup), 1);

        let ok = "module m;\nstruct P { a: Bit, b: Bit }\nentity E { y: Bit out, }\n\
                  impl E { let q: P = { .a = '1', .b = '0' }; y = q.a; }\n";
        assert_eq!(check_src(ok), 0);

        // The bound diagnostic names the offending type.
        let bound = "module m;\nfn f<T: Operator>(a: T) -> T { return a; }\n\
                     struct Q { z: Bit }\nentity E { y: Bit out, }\n\
                     impl E { let q: Q; y = f(q).z; }\n";
        let mut sink = DiagnosticSink::new();
        let src = format!("{bound}{VEC}");
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        let modules = std::slice::from_ref(&module);
        let resolved = crate::resolve::resolve(modules, &mut sink);
        check(modules, &resolved, &mut sink);
        assert!(
            sink.diagnostics()
                .iter()
                .any(|d| d.message.contains("`Q` does not satisfy")),
            "should name `Q`: {:?}",
            sink.diagnostics()
                .iter()
                .map(|d| &d.message)
                .collect::<Vec<_>>()
        );
    }

    /// An unknown field or method lowered to `Unknown`: the driver silently
    /// carried no value, and `if clk.typo()` produced an unknown *condition*,
    /// quietly turning a clocked block combinational.
    #[test]
    fn unknown_field_and_method_are_reported() {
        let st =
            "module m;\nstruct P { a: Bit }\nimpl P { fn get(self) -> Bit { return self.a; } }\n\
                  entity E { y: Bit out }\nimpl E { let p: P; y = ";
        assert_eq!(
            check_src(&format!("{st}p.nosuch; }}\n")),
            1,
            "unknown field"
        );
        assert_eq!(check_src(&format!("{st}p.a; }}\n")), 0, "real field");
        // A method name reaches the field check through the call's callee.
        assert_eq!(
            check_src(&format!("{st}p.get(); }}\n")),
            0,
            "method, not a field"
        );
        assert_eq!(
            check_src(&format!("{st}p.nomethod(); }}\n")),
            1,
            "unknown method"
        );

        // A newtype's fields are its base's, so they count as present.
        let derived = "module m;\nstruct A { x: Bit }\nstruct B(A);\n\
                       entity E { o: Bit out }\nimpl E { let b: B; o = b.x; }\n";
        assert_eq!(check_src(derived), 0, "newtype field");
    }

    /// Spec 3.20 calls a trait a compile-time contract, but a partial impl
    /// used to pass. A method the trait gives a default body is optional —
    /// that is how the compiler-recognized traits (`Operator`, `Prefix`,
    /// `Suffix`) allow an empty impl.
    #[test]
    fn trait_impl_must_provide_the_required_methods() {
        let base =
            "module m;\ntrait Tr { fn f(self) -> Bit; fn g(self) -> Bit; }\nstruct S { x: Bit }\n";
        assert_eq!(
            check_src(&format!(
                "{base}impl Tr for S {{ fn f(self) -> Bit {{ return self.x; }} }}\n"
            )),
            1,
            "`g` is missing"
        );
        assert_eq!(
            check_src(&format!(
                "{base}impl Tr for S {{ fn f(self) -> Bit {{ return self.x; }} \
                 fn g(self) -> Bit {{ return self.x; }} }}\n"
            )),
            0,
            "complete"
        );
        // A defaulted method is optional.
        let defaulted = "module m;\ntrait D { fn f(self) -> Bit { return '0'; } }\n\
                         struct S { x: Bit }\nimpl D for S {}\n";
        assert_eq!(check_src(defaulted), 0);
    }

    /// A ranged numeric (spec 3.26) checked its `let` initializer but not an
    /// assignment — and the value wraps to the storage width (50 -> 2), so the
    /// runtime range assert saw an in-range value and the violation vanished.
    #[test]
    fn ranged_numeric_assignment_is_checked() {
        let ent = "module m;\nentity E { y: integer<0..10> out, }\nimpl E { y = ";
        assert_eq!(check_src(&format!("{ent}50; }}\n")), 1, "above the range");
        assert_eq!(
            check_src(&format!("{ent}0 - 1; }}\n")),
            1,
            "below the range"
        );
        assert_eq!(check_src(&format!("{ent}7; }}\n")), 0, "inside");
        assert_eq!(check_src(&format!("{ent}10; }}\n")), 0, "the top bound");
        // An impl-level ranged local is covered the same way.
        let local = "module m;\nentity E { y: integer<0..10> out, }\n\
                     impl E { let k: integer<0..10>; k = 99; y = k; }\n";
        assert_eq!(check_src(local), 1);
    }

    /// Nothing checked call arity: a short call to a module `fn` left a
    /// parameter unbound, and a wrong-arity `extern "C"` call handed garbage
    /// to real native code.
    #[test]
    fn call_arity_must_match_the_declaration() {
        let base = "module m;\nfn add2(a: integer, b: integer) -> integer { return a + b; }\n\
                    extern \"C\" { fn ext(a: integer, b: integer) -> integer; }\n\
                    entity E { a: unsigned[8] in, y: unsigned[8] out }\nimpl E { y = ";
        assert_eq!(check_src(&format!("{base}add2(a); }}\n")), 1, "too few");
        assert_eq!(
            check_src(&format!("{base}add2(a, a, a); }}\n")),
            1,
            "too many"
        );
        assert_eq!(check_src(&format!("{base}add2(a, a); }}\n")), 0, "exact");
        assert_eq!(
            check_src(&format!("{base}ext(a); }}\n")),
            1,
            "extern too few"
        );
        assert_eq!(
            check_src(&format!("{base}ext(a, a); }}\n")),
            0,
            "extern exact"
        );
        // A conversion is a call shape but not a declared fn.
        assert_eq!(
            check_src(&format!("{base}unsigned[8](a); }}\n")),
            0,
            "conversion"
        );
    }

    #[test]
    fn runtime_function_arity_is_checked() {
        let tb = |expression: &str| {
            format!(
                "module m;\n#[test] entity T {{}}\n\
                 impl T {{ {expression}; }}\n"
            )
        };
        assert_eq!(check_src(&tb("rand(1)")), 1, "rand is nullary");
        assert_eq!(check_src(&tb("uniform(1)")), 1, "uniform is nullary");
        assert_eq!(check_src(&tb("randint(1)")), 1, "randint needs two bounds");
        assert_eq!(check_src(&tb("randint(1, 2, 3)")), 1, "too many bounds");
        assert_eq!(check_src(&tb("rand()")), 0);
        assert_eq!(check_src(&tb("randint(1, 2)")), 0);
    }

    /// A miscounted `print!` silently rendered an empty slot or dropped an
    /// argument — the worst place for that is a testbench you are debugging.
    #[test]
    fn format_argument_count_must_match() {
        let tb = |body: &str| {
            format!(
                "module m;\n#[test] entity T {{}}\nimpl T {{ let a: unsigned[8] = 1; {body} }}\n"
            )
        };
        assert_eq!(check_src(&tb(r#"print!("{} {}", a);"#)), 1, "too few");
        assert_eq!(check_src(&tb(r#"print!("{}", a, a);"#)), 1, "too many");
        assert_eq!(
            check_src(&tb(r#"print!("none", a);"#)),
            1,
            "no placeholders"
        );
        assert_eq!(check_src(&tb(r#"print!("{} {}", a, a);"#)), 0, "exact");
        // `{{}}` is an escaped brace pair and consumes nothing.
        assert_eq!(
            check_src(&tb(r#"print!("{{}} {}", a);"#)),
            0,
            "escaped braces"
        );
        // `assert!` takes its format string second.
        assert_eq!(check_src(&tb(r#"assert!(a == 1, "ok {}", a);"#)), 0);
        assert_eq!(check_src(&tb(r#"assert!(a == 1, "ok {}");"#)), 1);
    }

    /// A derived enum shares its base's variant *definitions*, so `Mid::B` and
    /// `Base::B` resolve to the same def. Typing the value from the declaring
    /// enum made a newtype's own variants unassignable to it — `m = Mid::B`
    /// was rejected as "cannot assign Base to Mid". The name written at the
    /// use site decides.
    #[test]
    fn newtype_variant_has_the_named_enums_type() {
        let src = |body: &str| {
            format!("module m;\nenum Base {{ A, B }}\nenum Mid(Base);\nenum Top(Mid);\nentity E {{ m: Mid out, t: Top out, }}\nimpl E {{ {body} }}\n")
        };
        assert_eq!(
            check_src(&src("m = Mid::B; t = Top::A;")),
            0,
            "one and two hops"
        );
        // Still distinct types: the base's own variant needs a conversion.
        assert_eq!(
            check_src(&src("m = Base::B; t = Top::A;")),
            1,
            "a newtype is not its base"
        );
        assert_eq!(
            check_src(&src("m = Mid(Base::B); t = Top::A;")),
            0,
            "conversion is explicit"
        );
    }

    /// Exhaustiveness was only ever checked on match *statements*. In
    /// expression position a missing variant means there is no value to
    /// produce, yet it drew no diagnostic at all.
    #[test]
    fn match_expression_exhaustiveness_is_checked() {
        let src = |arms: &str| {
            format!("module m;\nenum Base {{ A, B, C }}\nentity E {{ sel: Base in, y: unsigned[8] out, }}\nimpl E {{ y = match sel {{ {arms} }}; }}\n")
        };
        assert_eq!(
            warnings(
                &src("Base::A => 1, Base::B => 2"),
                codes::NON_EXHAUSTIVE_MATCH
            ),
            1,
            "a missing variant in expression position"
        );
        assert_eq!(
            warnings(
                &src("Base::A => 1, Base::B => 2, Base::C => 3"),
                codes::NON_EXHAUSTIVE_MATCH
            ),
            0,
            "all variants named"
        );
        assert_eq!(
            warnings(&src("Base::A => 1, _ => 9"), codes::NON_EXHAUSTIVE_MATCH),
            0,
            "a wildcard covers the rest"
        );
    }

    #[test]
    fn signal_width_has_no_global_word_limit() {
        let at = |w: u32| {
            check_src(&format!(
                "module m;\nentity E {{ y: unsigned[{w}] out, }}\nimpl E {{ y = 1; }}\n"
            ))
        };
        assert_eq!(at(129), 0);
        assert_eq!(at(512), 0);
        assert_eq!(at(4096), 0);
    }

    /// The literal-fits-width bounds are computed in `i64`, which saturates
    /// right where bit vectors get interesting: `1i64 << 63` is `i64::MIN`, so
    /// `- 1` overflowed at width 63 and the negation overflowed at width 64.
    /// Any `a == 5` on a signal that wide panicked the compiler.
    #[test]
    fn literal_width_bounds_do_not_overflow_at_the_top_widths() {
        let cmp = |w: u32, v: &str| {
            check_src(&format!(
                "module m;\nentity E {{ a: unsigned[{w}] in, y: unsigned[8] out, }}\nimpl E {{ if a == {v} {{ y = 1; }} }}\n"
            ))
        };
        // Reaching these at all is the regression: they used to panic. `1`
        // is representable at every width, including 1 bit.
        for w in 1..=64 {
            assert_eq!(cmp(w, "1"), 0, "width {w} accepts a literal that fits");
        }
        // The bound is still a bound.
        assert_eq!(cmp(8, "255"), 0, "the largest 8-bit value fits");
        assert_eq!(cmp(8, "256"), 1, "one past it does not");
        assert_eq!(cmp(63, "9223372036854775807"), 0, "i64::MAX fits 63 bits");
    }

    /// An attribute the compiler does not implement used to pass every stage
    /// and lower to an `Unknown`, which surfaced only at codegen as "no engine
    /// can run this design" — naming a driver index, never the attribute. It
    /// is reported at the use site now.
    #[test]
    fn unknown_system_attribute_is_reported() {
        let attr = |a: &str| {
            check_src(&format!(
                "module m;\nentity E {{ x: unsigned[8] in, y: unsigned[8] out, }}\nimpl E {{ y = x'{a}; }}\n"
            ))
        };
        // Every implemented attribute still passes.
        for a in ["length", "high", "low", "left", "right"] {
            assert_eq!(attr(a), 0, "`'{a}` is implemented");
        }
        assert_eq!(attr("bogus"), 1, "an invented attribute is reported");
        // The edge helpers became ClockLike methods; they are not attributes.
        assert_eq!(attr("rising"), 1, "`'rising` is not an attribute");
    }

    /// `return` in an entity body was dropped by lowering without a word: an
    /// entity describes hardware that is always active, so there is nothing to
    /// return from. It stays legal inside a function.
    #[test]
    fn return_outside_a_function_is_reported() {
        let hw =
            check_src("module m;\nentity E { y: unsigned[8] out, }\nimpl E { y = 1; return; }\n");
        assert_eq!(hw, 1, "hardware statement position");
        let free_fn = check_src(
            "module m;\nfn f(x: unsigned[8]) -> unsigned[8] { return x + 1; }\n\
             entity E { y: unsigned[8] out }\nimpl E { y = f(1); }\n",
        );
        assert_eq!(free_fn, 0, "a free function may return");
        let method = check_src(
            "module m;\nstruct S { v: unsigned[8] }\n\
             impl S { fn get(self) -> unsigned[8] { return self.v; } }\n\
             entity E { y: unsigned[8] out }\nimpl E { let s: S = { .v = 3 }; y = s.get(); }\n",
        );
        assert_eq!(method, 0, "a method may return");
        let nested = check_src(
            "module m;\nfn f(x: unsigned[8]) -> unsigned[8] { if x == 0 { return 1; } return x; }\n\
             entity E { y: unsigned[8] out }\nimpl E { y = f(1); }\n",
        );
        assert_eq!(nested, 0, "including inside a nested block");
    }

    /// Stimulus in an entity body was dropped by lowering without a word —
    /// most dangerously `assert!`, which let a check written into a design
    /// silently never run.
    #[test]
    fn stimulus_outside_a_testbench_is_reported() {
        let hw = |body: &str| {
            check_src(&format!(
                "module m;\nentity E {{ y: unsigned[8] out, }}\nimpl E {{ y = 1; {body} }}\n"
            ))
        };
        assert_eq!(hw("await 1ns;"), 1, "await needs simulation time");
        assert_eq!(
            hw(r#"assert!(y == 1, "x");"#),
            1,
            "an assertion needs a run"
        );
        assert_eq!(hw(r#"print!("hi");"#), 1, "printing needs a run");
        assert_eq!(hw(""), 0, "plain hardware is unaffected");

        // All of it is exactly what a testbench is for.
        let tb = check_src(
            "module m;\n#[test] entity T {}\n\
             impl T { let y: unsigned[8] = 1; await 1ns; assert!(y == 1, \"x\"); print!(\"hi\"); }\n",
        );
        assert_eq!(tb, 0, "a testbench may drive and check");
    }

    /// `std::attrs` declares attributes no stage reads. They resolve and apply
    /// cleanly, so `#[name = "foo"]` looks like it renames the emitted entity
    /// and silently does nothing.
    #[test]
    fn attributes_with_no_effect_are_flagged() {
        let n = |src: &str| warnings(src, codes::UNIMPLEMENTED_ATTR);
        assert_eq!(
            n("module m;\n#[name = \"x\"]\nentity E { y: unsigned[8] out, }\nimpl E { y = 1; }\n"),
            1,
            "`name` is reserved, not implemented"
        );
        assert_eq!(
            n("module m;\n#[library = \"work\"]\nentity E { y: unsigned[8] out, }\nimpl E { y = 1; }\n"),
            1,
            "so is `library`"
        );
        // The implemented ones stay quiet.
        assert_eq!(
            n("module m;\n#[top]\nentity E { y: unsigned[8] out, }\nimpl E { y = 1; }\n"),
            0,
            "`top` is acted on"
        );
    }

    /// A bare name in pattern position lowered to a wildcard, because
    /// `arm_match_cond` treats any pattern it cannot lower as "matches
    /// anything". So `Idle => ...` (instead of `State::Idle`) silently
    /// swallowed the entire match, `_` arms included, with no diagnostic.
    #[test]
    fn bare_name_is_not_a_pattern() {
        let m = |arms: &str| {
            format!("module m;\nenum State {{ Idle, Run }}\nentity E {{ v: unsigned[8] in, r: unsigned[8] out, }}\nimpl E {{ match v {{ {arms} }} }}\n")
        };
        assert_eq!(
            warnings(
                &m("Idle => { r = 1; } _ => { r = 9; }"),
                codes::INVALID_PATTERN
            ),
            1,
            "a bare name is rejected, not treated as a catch-all"
        );
        // The forms the lowering does honour stay clean.
        assert_eq!(
            warnings(
                &m("State::Idle => { r = 1; } _ => { r = 9; }"),
                codes::INVALID_PATTERN
            ),
            0,
            "a qualified variant is valid"
        );
        assert_eq!(
            warnings(
                &m("0..9 => { r = 1; } _ => { r = 9; }"),
                codes::INVALID_PATTERN
            ),
            0,
            "ranges are valid"
        );
        // `|` alternatives are checked through, not just the outer pattern.
        assert_eq!(
            warnings(
                &m("State::Idle | Run => { r = 1; } _ => { r = 9; }"),
                codes::INVALID_PATTERN
            ),
            1,
            "a bad alternative inside `|` is caught"
        );
    }

    /// A bit pattern whose text is not a well-formed mask was invisible: IR
    /// lowering turned it into a wildcard (swallowing the arm and every arm
    /// after it) while the runner never matched it — silently wrong, and
    /// differently wrong per engine.
    #[test]
    fn malformed_bit_pattern_is_rejected() {
        let m = |arms: &str| {
            format!("module m;\nentity E {{ v: unsigned[8] in, r: unsigned[8] out, }}\nimpl E {{ match v {{ {arms} _ => {{ r = 9; }} }} }}\n")
        };
        for bad in ["\"2\"", "x\"G\"", "o\"8\""] {
            assert_eq!(
                warnings(
                    &m(&format!("{bad} => {{ r = 1; }}")),
                    codes::INVALID_PATTERN
                ),
                1,
                "{bad} should be rejected"
            );
        }
        for good in ["\"01--\"", "x\"A?\"", "o\"7?\"", "\"0000_11--\""] {
            assert_eq!(
                warnings(
                    &m(&format!("{good} => {{ r = 1; }}")),
                    codes::INVALID_PATTERN
                ),
                0,
                "{good} is a valid pattern"
            );
        }
    }

    /// A range arm wholly inside an earlier one can never match (first match
    /// wins) — the enum and wildcard cases were caught, ranges were not.
    #[test]
    fn unreachable_range_arm_warns_only_when_fully_covered() {
        let m = |arms: &str| {
            format!("module m;\nentity E {{ v: unsigned[8] in, r: unsigned[8] out, }}\nimpl E {{ match v {{ {arms} _ => {{ r = 9; }} }} }}\n")
        };
        assert_eq!(
            warnings(
                &m("0..9 => { r = 1; } 2..5 => { r = 2; }"),
                codes::UNREACHABLE_MATCH_ARM
            ),
            1,
            "fully covered"
        );
        assert_eq!(
            warnings(
                &m("0..9 => { r = 1; } 5 => { r = 2; }"),
                codes::UNREACHABLE_MATCH_ARM
            ),
            1,
            "a literal inside an earlier range"
        );
        assert_eq!(
            warnings(
                &m("0..9 => { r = 1; } 5..15 => { r = 2; }"),
                codes::UNREACHABLE_MATCH_ARM
            ),
            0,
            "a partial overlap is still reachable"
        );
        assert_eq!(
            warnings(
                &m("0..9 => { r = 1; } 10..20 => { r = 2; }"),
                codes::UNREACHABLE_MATCH_ARM
            ),
            0,
            "disjoint ranges"
        );
    }

    /// A constant bit index past the end of a packed vector lowered to
    /// `Unknown` and only failed later with a generic engine message.
    #[test]
    fn out_of_bounds_constant_index_is_rejected() {
        let ent = "module m;\nentity E { a: unsigned[8] in, y: unsigned[8] out, }\nimpl E { y = ";
        assert_eq!(
            check_src(&format!("{ent}a[9]; }}\n")),
            1,
            "bit 9 of a unsigned[8]"
        );
        assert_eq!(
            check_src(&format!("{ent}a[15..8]; }}\n")),
            2,
            "both slice bounds"
        );
        assert_eq!(
            check_src(
                "module m;\nentity E { a: unsigned[8] in, y: Logic out, }\n\
                 impl E { y = a[7]; }\n"
            ),
            0,
            "the top bit is in range"
        );
        assert_eq!(
            check_src(&format!("{ent}a[7..0]; }}\n")),
            0,
            "a full-width slice"
        );
        // A runtime index can't be checked statically and must stay allowed.
        let dynamic = "module m;\nentity E { a: unsigned[8] in, i: unsigned[8] in, y: Logic out, }\nimpl E { y = a[i]; }\n";
        assert_eq!(check_src(dynamic), 0);

        // An instance array is declared with a plain count, so it is 0-based
        // and checkable the same way.
        let inst = |i: u32| {
            format!(
                "module m;\nentity Sub {{ a: unsigned[8] in, y: unsigned[8] out, }}\nimpl Sub {{ y = a; }}\n\
                 entity E {{ a: unsigned[8] in, y: unsigned[8] out, }}\nimpl E {{ let s: Sub[4]; y = s[{i}].y; }}\n"
            )
        };
        assert_eq!(check_src(&inst(9)), 1, "instance 9 of a Sub[4]");
        assert_eq!(check_src(&inst(3)), 0, "the last instance is in range");
    }

    /// Hardware has no divide-by-zero trap, so a constant zero divisor just
    /// yielded 0 with no complaint.
    #[test]
    fn constant_zero_divisor_is_rejected() {
        let src = "module m;\nentity E { a: unsigned[8] in, y: unsigned[8] out, }\nimpl E { y = a / 0; }\n";
        assert_eq!(check_src(src), 1);
        let ok = "module m;\nentity E { a: unsigned[8] in, b: unsigned[8] in, y: unsigned[8] out, }\nimpl E { y = a / b; }\n";
        assert_eq!(check_src(ok), 0, "a runtime divisor is fine");
    }

    /// Assigning one target twice unconditionally makes the first dead — the
    /// later driver overrides within a context, so it silently did nothing.
    #[test]
    fn dead_assignment_warns_but_defaults_do_not() {
        let dead = "module m;\nentity E { y: unsigned[8] out, }\nimpl E {\n  y = 1;\n  y = 2;\n}\n";
        assert_eq!(warnings(dead, codes::DEAD_ASSIGNMENT), 1);

        // A conditional override is the normal `default then override` shape.
        let guarded = "module m;\nentity E { c: Bit in, y: unsigned[8] out, }\nimpl E {\n  y = 1;\n  if c == '1' {\n    y = 2;\n  }\n}\n";
        assert_eq!(warnings(guarded, codes::DEAD_ASSIGNMENT), 0);

        // Distinct targets are unrelated.
        let distinct = "module m;\nentity E { y: unsigned[8] out, z: unsigned[8] out, }\nimpl E {\n  y = 1;\n  z = 2;\n}\n";
        assert_eq!(warnings(distinct, codes::DEAD_ASSIGNMENT), 0);
    }

    #[test]
    fn reserved_operators_cannot_be_overloaded() {
        let header = "module m;\nattr precedence: integer for impl;\n\
            trait Operator<op, I, O> { fn apply(self, rhs: I) -> O; }\n\
            enum A { A0 }\n";
        // Grammar symbols (assignment, path, ranges) and the derived
        // comparisons cannot be claimed by an operator impl.
        for sym in ["=", "::", ".", "..", "<", "=="] {
            let src = format!(
                "{header}#[precedence = 5] impl Operator<\"{sym}\", A, A> for A {{ fn apply(self, rhs: A) -> A {{ return self; }} }}\n"
            );
            assert!(
                check_src(&src) >= 1,
                "reserved operator `{sym}` should error"
            );
        }
        // A genuine custom punctuation operator is accepted.
        let ok = format!(
            "{header}#[precedence = 5] impl Operator<\"^^\", A, A> for A {{ fn apply(self, rhs: A) -> A {{ return self; }} }}\n"
        );
        assert_eq!(check_src(&ok), 0);
    }

    #[test]
    fn every_formatted_macro_checks_arity() {
        let fixture =
            |call: &str| format!("module m;\n#[test] entity T {{}}\nimpl T {{ {call}; }}\n");
        for call in [
            "print!(\"{}\")",
            "assert!(true, \"{}\")",
            "warn!(true, \"{}\")",
        ] {
            assert_eq!(
                check_src(&fixture(call)),
                1,
                "`{call}` should diagnose its missing format argument"
            );
        }
        assert_eq!(
            check_src(&fixture("warn!(true, \"{}\", 1)")),
            0,
            "a matching warning format argument is valid"
        );
    }

    #[test]
    fn custom_operator_precedence_is_required_and_consistent() {
        let header = "module m;\nattr precedence: integer for impl;\n\
            trait Operator<op, I, O> { fn apply(self, rhs: I) -> O; }\n\
            enum A { A0 } enum B { B0 }\n";
        let missing = format!(
            "{header}impl Operator<\"join\", A, A> for A {{ fn apply(self, rhs: A) -> A {{ return self; }} }}\n"
        );
        assert_eq!(check_src(&missing), 1);

        let conflict = format!(
            "{header}\
             #[precedence = 40] impl Operator<\"join\", A, A> for A {{ fn apply(self, rhs: A) -> A {{ return self; }} }}\n\
             #[precedence = 30] impl Operator<\"join\", B, B> for B {{ fn apply(self, rhs: B) -> B {{ return self; }} }}\n"
        );
        assert_eq!(check_src(&conflict), 1);
    }

    /// The forms the newtype grammar admits. Extension is not among them —
    /// `struct B(A)` has nowhere to put a body — so that is the parser's to
    /// reject, not this stage's (see `syntax::parser`).
    #[test]
    fn struct_newtype_and_composition_are_the_two_shapes() {
        let newtype = check_src("module m;\nstruct A { x: Bit }\nstruct B(A);\n");
        assert_eq!(newtype, 0, "a newtype over another struct");
        let over_array = check_src("module m;\nstruct Word(Bit[]);\n");
        assert_eq!(over_array, 0, "a newtype over an array");
        let composed = check_src("module m;\nstruct A { x: Bit }\nstruct B { a: A, y: Bit }\n");
        assert_eq!(composed, 0, "composition builds the bigger type");
    }

    /// `enum B(A);` is a newtype over `A`'s variants, so `A` must be an enum.
    /// The old `enum S : unsigned[2]` storage annotation is gone: an enum's
    /// width is derived from its variants and discriminants.
    #[test]
    fn enum_newtype_base_must_be_an_enum() {
        let newtype = check_src("module m;\nenum A { X, Y }\nenum B(A);\n");
        assert_eq!(newtype, 0, "an enum base is a newtype");
        let non_enum = check_src("module m;\nenum S(unsigned[2]);\n");
        assert_eq!(non_enum, 1, "a non-enum base is not a derivation");
        let plain = check_src("module m;\nenum S { Idle = 0, Run = 1 }\n");
        assert_eq!(plain, 0, "the width comes from the variants");
    }
}
