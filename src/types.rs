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
    /// A declared function or method with no return type. Unlike `Error`, this
    /// is known and must be rejected anywhere a value is required.
    Void,
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
            Ty::Named(_) | Ty::Void | Ty::Error => None,
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
    let mut checker = Checker::new(sink, resolved, modules);
    checker.collect(modules);
    checker.check_struct_field_cycles();
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
    /// Declared labels of a packed vector or data array. Unlike `range`, this
    /// is an index domain rather than a numeric value constraint.
    index_bounds: Option<(i64, i64)>,
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
type GenericFnSignature = (Vec<Param>, Vec<Option<Type>>);
type MethodParams = Vec<Option<Type>>;
type TraitDefaultSignature = (String, Option<Type>, MethodParams, bool);
type InheritedMethodSignature = (String, String, Option<Type>, MethodParams, bool);
type ImplEnvironment = (PortDirs, HashMap<String, Ty>, HashMap<String, (i64, i64)>);

#[derive(Clone)]
struct MemberVisibility {
    is_pub: bool,
    owner: String,
    module: String,
    span: Span,
}

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
    /// Whether the walk is inside a `match` arm. An entity may be instantiated
    /// at the root of another entity's body, or inside a generate `for`/`if` —
    /// a `match` is neither, and elaboration never gathered instances from one.
    in_match_arm: std::cell::Cell<bool>,
    /// Generic parameter names in scope (an impl's binder, a fn's own). A
    /// parameter is never an entity instantiation even when an entity happens
    /// to share its name — elaboration excludes them the same way.
    type_params: std::cell::RefCell<HashSet<String>>,
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
    /// Trait name -> its defaulted methods (name -> declared return type). A
    /// trait method *with* a body is a default the impl may omit — which
    /// `trait_required` already allows — so the implementing type has to be
    /// able to call it.
    trait_defaults: HashMap<String, Vec<TraitDefaultSignature>>,
    /// Type identity -> the traits it implements, for that inheritance.
    trait_impls_by_type: HashMap<String, Vec<String>>,
    /// Trait name -> the methods an implementation must provide (those the
    /// trait declares without a default body). Spec 3.20: a trait is a
    /// compile-time contract, so a partial impl is an error.
    trait_required: HashMap<String, Vec<String>>,
    /// Trait name -> exported flag, owning module, and declaration span.
    /// Methods in a trait impl inherit this visibility.
    trait_visibility: HashMap<String, (bool, String, Span)>,
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
    /// Raw representation fields are private unless exported. Views are
    /// checked separately because applying one is an explicit interface.
    field_visibility: HashMap<(String, String), MemberVisibility>,
    /// Struct name -> each field's `(name, declared type head, span)`. A
    /// struct that transitively contains itself has no finite layout, and
    /// flattening one in elaboration recursed until the stack gave out with
    /// typecheck reporting nothing at all.
    struct_field_types: HashMap<String, Vec<(String, String, Span)>>,
    /// Struct name -> field name -> the field's *full* declared type. The map
    /// above keeps only the head name (`unsigned[16]` -> `unsigned`), which is
    /// enough to name a type and not enough to compare a width — so a field
    /// target typed as `Ty::Error` and the strict assignment-width rule had
    /// nothing to check.
    field_decl_types: HashMap<String, HashMap<String, Type>>,
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
    /// Free-function name -> its declared return type. A call expression used
    /// to type as `Error` even for a known declaration, suppressing checks in
    /// assignments, conditions, arguments, and enclosing expressions.
    fn_return_types: HashMap<String, Option<Type>>,
    /// Literal suffix -> the type names defining it via `impl Suffix<sym, _>
    /// for T` (more than one is an ambiguity error at the use site).
    suffix_types: HashMap<String, Vec<String>>,
    /// Bit-string prefix (`x`, `o`) -> the type names defining it via
    /// `impl Prefix<sym, _> for T` (spec 3.24). std declares which prefixes
    /// exist; the compiler evaluates the known radix ones intrinsically.
    prefix_types: HashMap<String, Vec<String>>,
    /// `using X = T;` aliases, resolved through when typing.
    aliases: HashMap<String, Type>,
    /// Indexed local -> its inclusive declared labels. Range direction is not
    /// retained in `Ty`: both `unsigned[15..8]` and `unsigned[8..15]` have
    /// length 8, while their valid labels are 8..15 rather than 0..7. The
    /// declaration is therefore authoritative for vectors and data arrays.
    array_bounds: std::cell::RefCell<HashMap<String, (i64, i64)>>,
    /// Aliases currently being expanded, so a cycle (`using A = B; using B =
    /// A`) is caught instead of recursing until the stack overflows.
    expanding: std::cell::RefCell<HashSet<String>>,
    /// (type head, method name) -> the method's declared return type, for
    /// typing method calls `recv.method(args)` (spec 3.20). Covers both
    /// inherent (`impl T`) and trait (`impl Tr for T`) impl methods.
    methods: HashMap<(String, String), Option<Type>>,
    /// `(type head, method name)` -> declared non-`self` parameter types.
    /// Method calls used to check only that a name existed, so wrong counts
    /// and raw-bit reinterpretations both passed semantic analysis.
    method_param_types: HashMap<(String, String), MethodParams>,
    /// Whether a collected method declares a `self` receiver. Instance and
    /// associated call syntax are distinct and cannot substitute for each
    /// other merely because the owner/name pair exists.
    method_has_self: HashMap<(String, String), bool>,
    method_visibility: HashMap<(String, String), MemberVisibility>,
    /// Named view -> per-field directions.
    view_dirs: HashMap<String, HashMap<String, Direction>>,
    /// Persistent Stage-4 facts keyed by the AST expression's stable span.
    expr_types: std::cell::RefCell<HashMap<Span, Ty>>,
    /// Concrete meaning of the `Self` type while checking one impl. The
    /// resolver correctly binds `Self` locally, but its synthetic definition
    /// is not the impl target and must not become a distinct nominal type.
    current_self_ty: std::cell::RefCell<Option<Ty>>,
    file_modules: HashMap<crate::diag::FileId, String>,
    entity_names: HashSet<String>,
}

impl<'a> Checker<'a> {
    fn new(sink: &'a mut DiagnosticSink, resolved: &'a Resolved, modules: &[Module]) -> Self {
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
        let file_modules: HashMap<crate::diag::FileId, String> = modules
            .iter()
            .map(|module| {
                let name = module
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.text.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                (module.span.file, name)
            })
            .collect();
        let entity_names = modules
            .iter()
            .flat_map(|module| module.items.iter())
            .filter_map(|item| match item {
                Item::Entity(entity) => Some(entity.name.text.clone()),
                _ => None,
            })
            .collect();
        let trait_visibility = modules
            .iter()
            .flat_map(|module| module.items.iter())
            .filter_map(|item| match item {
                Item::Trait(trait_) => Some((
                    trait_.name.text.clone(),
                    (
                        trait_.is_pub,
                        file_modules
                            .get(&trait_.span.file)
                            .cloned()
                            .unwrap_or_else(|| format!("<file:{}>", trait_.span.file.0)),
                        trait_.name.span,
                    ),
                )),
                _ => None,
            })
            .collect();
        Checker {
            test_entities: HashSet::new(),
            in_testbench: std::cell::Cell::new(false),
            in_fn_body: std::cell::Cell::new(false),
            in_match_arm: std::cell::Cell::new(false),
            type_params: std::cell::RefCell::new(HashSet::new()),
            sink,
            resolved,
            entities: HashMap::new(),
            attr_targets,
            attr_value_kinds,
            trait_impls: HashMap::new(),
            trait_defaults: HashMap::new(),
            trait_impls_by_type: HashMap::new(),
            trait_required: HashMap::new(),
            trait_visibility,
            operator_sigs: HashMap::new(),
            index_sigs: HashMap::new(),
            operator_precedence: HashMap::new(),
            enum_variants: HashMap::new(),
            own_variants: HashMap::new(),
            enum_bases: HashMap::new(),
            structs: HashMap::new(),
            field_visibility: HashMap::new(),
            struct_field_types: HashMap::new(),
            field_decl_types: HashMap::new(),
            array_bounds: std::cell::RefCell::new(HashMap::new()),
            views: HashMap::new(),
            vector_families: HashSet::new(),
            vector_elements: HashMap::new(),
            blanket_array_impls: HashMap::new(),
            generic_fns: HashMap::new(),
            fn_arity: HashMap::new(),
            fn_param_types: HashMap::new(),
            fn_return_types: HashMap::new(),
            suffix_types: HashMap::new(),
            prefix_types: HashMap::new(),
            aliases: HashMap::new(),
            expanding: std::cell::RefCell::new(HashSet::new()),
            methods: HashMap::new(),
            method_param_types: HashMap::new(),
            method_has_self: HashMap::new(),
            method_visibility: HashMap::new(),
            view_dirs: HashMap::new(),
            expr_types: std::cell::RefCell::new(HashMap::new()),
            current_self_ty: std::cell::RefCell::new(None),
            file_modules,
            entity_names,
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
        // A trait's defaulted methods belong to every type that implements it
        // and did not provide its own. Collection order is not guaranteed — a
        // trait may be declared after the impl — so this is a second pass.
        let mut inherited: Vec<InheritedMethodSignature> = Vec::new();
        for (ty, traits) in &self.trait_impls_by_type {
            for tr in traits {
                let Some(methods) = self.trait_defaults.get(tr) else {
                    continue;
                };
                for (name, ret, params, has_self) in methods {
                    inherited.push((
                        ty.clone(),
                        name.clone(),
                        ret.clone(),
                        params.clone(),
                        *has_self,
                    ));
                }
            }
        }
        for (ty, name, ret, params, has_self) in inherited {
            let key = (ty, name);
            self.methods.entry(key.clone()).or_insert(ret);
            self.method_param_types.entry(key.clone()).or_insert(params);
            self.method_has_self.entry(key).or_insert(has_self);
        }
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
                            index_bounds: self.declared_index_bounds(&p.ty),
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
                            let key = (ty.clone(), f.name.text.clone());
                            self.methods.insert(key.clone(), f.ret.clone());
                            self.method_param_types.insert(
                                key.clone(),
                                f.params
                                    .iter()
                                    .filter(|parameter| !parameter.is_self)
                                    .map(|parameter| parameter.ty.clone())
                                    .collect(),
                            );
                            self.method_has_self
                                .insert(key, f.params.iter().any(|parameter| parameter.is_self));
                            let trait_contract = im.trait_.as_ref().and_then(|path| {
                                path.segments
                                    .last()
                                    .and_then(|name| self.trait_visibility.get(&name.text).cloned())
                            });
                            self.method_visibility.insert(
                                (ty.clone(), f.name.text.clone()),
                                MemberVisibility {
                                    is_pub: trait_contract
                                        .as_ref()
                                        .map(|(is_pub, _, _)| *is_pub)
                                        .unwrap_or(f.is_pub || im.trait_.is_some()),
                                    owner: ty.clone(),
                                    module: trait_contract
                                        .as_ref()
                                        .map(|(_, module, _)| module.clone())
                                        .unwrap_or_else(|| self.module_of(im.span)),
                                    span: trait_contract
                                        .map(|(_, _, span)| span)
                                        .unwrap_or(f.name.span),
                                },
                            );
                        }
                    }
                }
                // Record `impl Trait for Type` so trait-driven checks (e.g.
                // conditions) can ask "does T implement Trait?".
                if let (Some(tr), Some(ty)) = (&im.trait_, type_identity(&im.target)) {
                    if let Some(name) = tr.segments.last() {
                        self.trait_impls_by_type
                            .entry(ty)
                            .or_default()
                            .push(name.text.clone());
                    }
                }
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
                self.trait_defaults.insert(
                    t.name.text.clone(),
                    t.items
                        .iter()
                        .filter(|f| f.body.is_some())
                        .map(|f| {
                            (
                                f.name.text.clone(),
                                f.ret.clone(),
                                f.params
                                    .iter()
                                    .filter(|parameter| !parameter.is_self)
                                    .map(|parameter| parameter.ty.clone())
                                    .collect(),
                                f.params.iter().any(|parameter| parameter.is_self),
                            )
                        })
                        .collect(),
                );
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
                    self.fn_param_types.insert(
                        f.name.text.clone(),
                        f.params
                            .iter()
                            .filter(|p| !p.is_self)
                            .map(|p| p.ty.clone())
                            .collect(),
                    );
                    self.fn_return_types
                        .insert(f.name.text.clone(), f.ret.clone());
                }
            }
            Item::Fn(f) if !f.generics.params.is_empty() => {
                self.fn_arity.insert(
                    f.name.text.clone(),
                    f.params.iter().filter(|p| !p.is_self).count(),
                );
                let params = f
                    .params
                    .iter()
                    .filter(|p| !p.is_self)
                    .map(|p| p.ty.clone())
                    .collect();
                self.generic_fns
                    .insert(f.name.text.clone(), (f.generics.params.clone(), params));
                self.fn_param_types.insert(
                    f.name.text.clone(),
                    f.params
                        .iter()
                        .filter(|p| !p.is_self)
                        .map(|p| p.ty.clone())
                        .collect(),
                );
                self.fn_return_types
                    .insert(f.name.text.clone(), f.ret.clone());
            }
            Item::Fn(f) => {
                self.fn_arity.insert(
                    f.name.text.clone(),
                    f.params.iter().filter(|p| !p.is_self).count(),
                );
                // Concrete functions can validate every parameter directly;
                // the generic arm above records the same shape and defers only
                // type-parameter substitution to each call site.
                self.fn_param_types.insert(
                    f.name.text.clone(),
                    f.params
                        .iter()
                        .filter(|p| !p.is_self)
                        .map(|p| p.ty.clone())
                        .collect(),
                );
                self.fn_return_types
                    .insert(f.name.text.clone(), f.ret.clone());
            }
            Item::Struct(st) => {
                let fields = st.fields.iter().map(|f| f.name.text.clone()).collect();
                self.structs
                    .insert(st.name.text.clone(), (st.base.clone(), fields));
                self.field_decl_types.insert(
                    st.name.text.clone(),
                    st.fields
                        .iter()
                        .map(|f| (f.name.text.clone(), f.ty.clone()))
                        .collect(),
                );
                let module = self.module_of(st.span);
                for field in &st.fields {
                    self.field_visibility.insert(
                        (st.name.text.clone(), field.name.text.clone()),
                        MemberVisibility {
                            is_pub: field.is_pub,
                            owner: st.name.text.clone(),
                            module: module.clone(),
                            span: field.name.span,
                        },
                    );
                }
                self.struct_field_types.insert(
                    st.name.text.clone(),
                    st.fields
                        .iter()
                        .filter_map(|f| {
                            Some((
                                f.name.text.clone(),
                                type_head_name(&f.ty)?.to_string(),
                                f.name.span,
                            ))
                        })
                        .collect(),
                );
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

    fn base_struct_fields_named(&self, head: &str) -> Vec<String> {
        self.base_struct_fields_named_at(head, &mut HashSet::new())
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
        let Some(head) = type_head_name(ty) else {
            return Vec::new();
        };
        self.base_struct_fields_named_at(head, seen)
    }

    fn base_struct_fields_named_at(&self, head: &str, seen: &mut HashSet<String>) -> Vec<String> {
        let mut out = Vec::new();
        if !seen.insert(head.to_string()) {
            return out;
        }
        if let Some((base, own)) = self.structs.get(head) {
            if let Some(base) = base.as_ref().and_then(type_head_name) {
                out.extend(self.base_struct_fields_named_at(base, seen));
            }
            out.extend(own.iter().cloned());
        }
        seen.remove(head);
        out
    }

    /// A struct field's declared type, following the derivation chain so a
    /// field inherited from a base struct types the same as its own.
    fn field_decl_ty(&self, head: &str, field: &str) -> Option<Type> {
        let mut current = head.to_string();
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            if !seen.insert(current.clone()) {
                return None;
            }
            if let Some(ty) = self
                .field_decl_types
                .get(&current)
                .and_then(|m| m.get(field))
            {
                return Some(ty.clone());
            }
            match self
                .structs
                .get(&current)
                .and_then(|(base, _)| base.clone())
            {
                Some(base) => current = type_head_name(&base)?.to_string(),
                None => return None,
            }
        }
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
                        let saved =
                            self.push_type_params(f.generics.params.iter().map(|p| &p.name.text));
                        // The std operator/literal hook traits use an empty
                        // body as an intrinsic placeholder. A real default
                        // body, however, is ordinary inlined code and must
                        // return on every path like any other function.
                        self.check_function_block(f, b, None, !b.stmts.is_empty());
                        *self.type_params.borrow_mut() = saved;
                    }
                }
            }
            Item::Fn(f) => {
                // A generic fn's body is verified at each call (it inlines),
                // where the concrete types are known; checking it abstractly
                // (operators on the opaque `T`) would wrongly reject it.
                if f.generics.params.is_empty() {
                    if let Some(b) = &f.body {
                        self.check_function_block(f, b, None, true);
                    }
                } else if let Some(body) = &f.body {
                    // Operator/type checks wait for monomorphization, but
                    // reaching the end without a value is independent of the
                    // concrete type arguments and is already invalid here.
                    let names = f
                        .params
                        .iter()
                        .filter_map(|parameter| {
                            Some((
                                parameter.name.as_ref()?.text.clone(),
                                parameter
                                    .ty
                                    .as_ref()
                                    .map(|ty| self.ast_ty(ty))
                                    .unwrap_or(Ty::Error),
                            ))
                        })
                        .collect();
                    self.check_function_fallthrough(f, body, &names);
                }
            }
            Item::ExternBlock { fns, .. } => {
                for function in fns {
                    self.check_extern_c_signature(function);
                }
            }
            // A struct newtype needs no check of its own: the form carries no
            // body, so there is nothing to validate past its base type.
            Item::Struct(_) => {}
            Item::View(v) => self.check_view(v),
            Item::Using(_) | Item::AttrDecl(_) => {}
        }
    }

    /// Validate the scalar ABI the current LLVM and generated-C backends
    /// implement. Accepting a broader source type is dangerous here: both
    /// backends otherwise lower every non-real value as one `uint64_t`, which
    /// silently truncates wide vectors and treats aggregate layouts as scalar
    /// words. Void calls in statement position are likewise not represented
    /// in hardware IR yet, so reject them instead of dropping their effects.
    fn check_extern_c_signature(&mut self, function: &FnDecl) {
        if !function.generics.params.is_empty() {
            self.error(
                codes::TYPE_MISMATCH,
                function.name.span,
                format!(
                    "extern C function `{}` cannot have generic parameters",
                    function.name.text
                ),
            );
        }
        for parameter in &function.params {
            let Some(ty) = &parameter.ty else {
                self.error(
                    codes::TYPE_MISMATCH,
                    parameter
                        .name
                        .as_ref()
                        .map_or(function.name.span, |name| name.span),
                    format!(
                        "extern C parameter in `{}` needs an explicit ABI type",
                        function.name.text
                    ),
                );
                continue;
            };
            self.check_extern_c_type(function, ty, "parameter");
        }
        match &function.ret {
            Some(ty) => self.check_extern_c_type(function, ty, "return type"),
            None => self.error_with_help(
                codes::TYPE_MISMATCH,
                function.name.span,
                format!(
                    "void extern C function `{}` is not supported yet",
                    function.name.text
                ),
                "extern calls currently need a scalar return value; statement-only C calls have no hardware IR representation"
                    .to_string(),
            ),
        }
    }

    fn check_extern_c_type(&mut self, function: &FnDecl, ty: &Type, position: &str) {
        let checked = self.ast_ty(ty);
        let supported = matches!(checked, Ty::Integer | Ty::Real)
            || matches!(
                checked,
                Ty::Array {
                    len: 1..=64,
                    family: Some(_),
                    ..
                }
            );
        if supported || checked == Ty::Error {
            return;
        }
        let detail = match checked {
            Ty::Array {
                len,
                family: Some(_),
                ..
            } if len > 64 => {
                format!("packed value is {len} bits, but the current C ABI carries one 64-bit word")
            }
            Ty::Array { .. } | Ty::Named(_) => {
                "aggregate and nominal values have no C layout mapping".to_string()
            }
            Ty::Char => "Char has no declared C character ABI".to_string(),
            Ty::Void => "void is not a value ABI type".to_string(),
            Ty::Integer | Ty::Real | Ty::Error => unreachable!(),
        };
        self.error_with_help(
            codes::TYPE_MISMATCH,
            type_head_span(ty).unwrap_or(function.name.span),
            format!(
                "unsupported extern C {position} `{}` in `{}`: {detail}",
                crate::syntax::pretty::type_str(ty),
                function.name.text
            ),
            "use `real`, `integer`, or a packed numeric type of at most 64 bits; wrap other C signatures in a scalar C adapter"
                .to_string(),
        );
    }

    /// Validate constant layout bounds before elaboration/lowering tries to
    /// flatten them. Symbolic widths are checked after substitution; here we
    /// catch source constants that cannot fit the compiler's `u32` layout
    /// representation and would otherwise truncate or attempt an impossible
    /// allocation during error recovery.
    /// A struct that contains itself, directly or through other structs, has
    /// no finite layout. Elaboration flattens a struct into leaf signals, so
    /// one of these recursed until the process aborted with no diagnostic —
    /// `struct A { f: B } struct B { f: A }` was enough.
    fn check_struct_field_cycles(&mut self) {
        let mut names: Vec<String> = self.struct_field_types.keys().cloned().collect();
        names.sort();
        let mut found = Vec::new();
        for name in &names {
            if let Some((field, span, through)) = self.self_containing(name) {
                found.push((name.clone(), field, span, through));
            }
        }
        for (name, field, span, through) in found {
            let path = if through == name {
                format!("`{name}` contains itself")
            } else {
                format!("`{name}` contains itself through `{through}`")
            };
            self.error_with_help(
                codes::TYPE_MISMATCH,
                span,
                format!("{path}, so it has no finite layout"),
                format!(
                    "field `{field}` would have to hold another `{name}`, without end; \
                     hardware has no indirection to break the cycle"
                ),
            );
        }
    }

    /// The field that closes a containment cycle back to `start`, with the
    /// struct it goes through. Breadth-first so the shortest cycle is found.
    fn self_containing(&self, start: &str) -> Option<(String, Span, String)> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: Vec<(String, String, Span, String)> = self
            .struct_field_types
            .get(start)?
            .iter()
            .map(|(f, head, span)| (head.clone(), f.clone(), *span, head.clone()))
            .collect();
        while !queue.is_empty() {
            let mut next = Vec::new();
            for (current, field, span, through) in queue {
                if current == start {
                    return Some((field, span, through));
                }
                if !seen.insert(current.clone()) {
                    continue;
                }
                if let Some(fields) = self.struct_field_types.get(&current) {
                    for (_, head, _) in fields {
                        next.push((head.clone(), field.clone(), span, through.clone()));
                    }
                }
            }
            queue = next;
        }
        None
    }

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
            Type::Generic { base, args, .. } => {
                self.check_type_layout(base);
                for argument in args {
                    match argument {
                        GenericArg::PositionalType(ty) | GenericArg::NamedType { ty, .. } => {
                            self.check_type_layout(ty);
                        }
                        GenericArg::Positional(_) | GenericArg::Named { .. } => {}
                    }
                }
            }
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
            } else if let Some(visibility) = self.field_visibility_for(target, &f.name.text) {
                // A view is allowed to make private storage part of an
                // interface only when it is declared by that storage's own
                // module. Otherwise a foreign module could publish private
                // representation simply by wrapping it in a view.
                if !visibility.is_pub && self.module_of(view.span) != visibility.module {
                    self.sink.emit(
                        Diagnostic::error(format!(
                            "view `{}` cannot expose private field `{target}.{}` from another module",
                            view.name.text, f.name.text
                        ))
                        .with_code(codes::PRIVATE_MEMBER)
                        .at(f.name.span)
                        .label(visibility.span, "private field declared here")
                        .help("declare the view in the struct's module, or make the backing field `pub`"),
                    );
                }
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
    /// driver silently carried no value. Struct receivers are checked, as are
    /// bus ports through their view's backing struct; an instance port
    /// (`dut.y`) and anything else unresolvable stay silent, and the check
    /// walks the derivation chain so an inherited field counts as present.
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
        // A bus port (`bus: Stream Source`) types as the *view*, which owns no
        // fields, so the walk below found no struct and returned silently —
        // leaving every field access through a bus unchecked.
        let through_view = self.view_backing(&head);
        let head = through_view.clone().unwrap_or(head);
        // The backing struct's own methods are callable through the bus, and
        // reach here as field nodes just as the view's own methods do.
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
                // Applying a view is the explicit structural interface for
                // its backing storage. It exposes the fields it names without
                // making raw `Struct.field` access public everywhere.
                if through_view.is_none() {
                    self.check_field_visibility(&name, field);
                }
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

    fn check_field_visibility(&mut self, owner: &str, field: &Ident) {
        let Some(visibility) = self
            .field_visibility
            .get(&(owner.to_string(), field.text.clone()))
            .cloned()
        else {
            return;
        };
        if visibility.is_pub || self.member_access_allowed(&visibility, field.span) {
            return;
        }
        self.sink.emit(
            Diagnostic::error(format!(
                "field `{}.{}` is private to module `{}`",
                owner, field.text, visibility.module
            ))
            .with_code(codes::PRIVATE_MEMBER)
            .at(field.span)
            .label(visibility.span, "declared private here")
            .help(format!(
                "mark the field `pub`, or expose the operation through a `pub fn` in `impl {}`",
                visibility.owner
            )),
        );
    }

    /// A method call whose name no impl provides for the receiver's type.
    /// It used to lower to `Unknown` — silently producing a driver with an
    /// unknown value, or (worse) an unknown *condition*, so an `if
    /// clk.typo()` block quietly became combinational.
    ///
    /// Deliberately conservative: only a receiver whose type head is known
    /// *and* which has at least one method recorded is checked, so a type
    /// whose methods this stage never collected can't false-positive.
    fn check_method_call(&mut self, callee: &Expr, args: &[Expr], sym: &HashMap<String, Ty>) {
        let Expr::Field { base, field, span } = callee else {
            return;
        };
        let recv = self.type_of(base, sym);
        let Some(head) = self.ty_head(&recv) else {
            return;
        };
        let key = (head.clone(), field.text.clone());
        if self.methods.contains_key(&key) {
            if !self.check_method_visibility(&key, field.span) {
                return;
            }
            if !self.method_has_self.get(&key).copied().unwrap_or(false) {
                self.error_with_help(
                    codes::INVALID_METHOD_CALL,
                    *span,
                    format!(
                        "associated function `{}::{}` has no `self` receiver",
                        head, field.text
                    ),
                    format!("call it as `{}::{}(...)`", head, field.text),
                );
                return;
            }
            self.check_collected_method_args(&head, &field.text, *span, args, sym);
            return;
        }
        // A view receiver may use inherent methods of its backing struct.
        if let Some(backing) = self.view_backing(&head) {
            let backing_key = (backing.clone(), field.text.clone());
            if self.methods.contains_key(&backing_key) {
                if !self.check_method_visibility(&backing_key, field.span) {
                    return;
                }
                if !self
                    .method_has_self
                    .get(&backing_key)
                    .copied()
                    .unwrap_or(false)
                {
                    self.error_with_help(
                        codes::INVALID_METHOD_CALL,
                        *span,
                        format!(
                            "associated function `{}::{}` has no `self` receiver",
                            backing, field.text
                        ),
                        format!("call it as `{}::{}(...)`", backing, field.text),
                    );
                    return;
                }
                self.check_collected_method_args(&backing, &field.text, *span, args, sym);
                return;
            }
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

    /// Check `Type::function(args)` against the same collected impl signature
    /// as receiver syntax, while enforcing that this declaration has no
    /// `self` parameter.
    fn check_associated_call(&mut self, callee: &Expr, args: &[Expr], sym: &HashMap<String, Ty>) {
        let Expr::Path(path) = callee else { return };
        if path.segments.len() < 2 {
            return;
        }
        let owner = &path.segments[path.segments.len() - 2].text;
        let name = &path.segments[path.segments.len() - 1].text;
        if name == "new" && self.is_conversion_name(owner) {
            if !args.is_empty() {
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(callee),
                    format!(
                        "`{owner}::new` takes no arguments, but {} were given",
                        args.len()
                    ),
                );
            }
            return;
        }
        let key = (owner.clone(), name.clone());
        if !self.methods.contains_key(&key) {
            return;
        }
        if !self.check_method_visibility(&key, expr_span(callee)) {
            return;
        }
        if self.method_has_self.get(&key).copied().unwrap_or(false) {
            self.error_with_help(
                codes::INVALID_METHOD_CALL,
                expr_span(callee),
                format!("method `{owner}.{name}` needs a `self` receiver"),
                format!("call it on a `{owner}` value, e.g. `value.{name}(...)`"),
            );
            return;
        }
        self.check_collected_method_args(owner, name, expr_span(callee), args, sym);
    }

    fn check_method_visibility(&mut self, key: &(String, String), use_span: Span) -> bool {
        let Some(visibility) = self.method_visibility.get(key).cloned() else {
            return true;
        };
        if visibility.is_pub || self.member_access_allowed(&visibility, use_span) {
            return true;
        }
        self.sink.emit(
            Diagnostic::error(format!(
                "method `{}::{}` is private to module `{}`",
                key.0, key.1, visibility.module
            ))
            .with_code(codes::PRIVATE_MEMBER)
            .at(use_span)
            .label(visibility.span, "declared private here")
            .help("mark the inherent method `pub` to include it in the type's API"),
        );
        false
    }

    fn member_access_allowed(&self, visibility: &MemberVisibility, use_span: Span) -> bool {
        self.module_of(use_span) == visibility.module
    }

    fn check_collected_method_args(
        &mut self,
        owner: &str,
        name: &str,
        span: Span,
        args: &[Expr],
        sym: &HashMap<String, Ty>,
    ) {
        let key = (owner.to_string(), name.to_string());
        let Some(params) = self.method_param_types.get(&key).cloned() else {
            return;
        };
        if args.len() != params.len() {
            self.error(
                codes::TYPE_MISMATCH,
                span,
                format!(
                    "`{owner}::{name}` takes {} argument(s) but {} were given",
                    params.len(),
                    args.len()
                ),
            );
            return;
        }
        let owner_ty = self.ty_from_head(owner);
        for (argument, declared) in args.iter().zip(params.iter()) {
            let Some(declared) = declared else { continue };
            let expected = self.ast_ty_for_owner(declared, &owner_ty);
            if self.check_struct_literal_for_ty(&expected, argument, sym)
                || matches!(expected, Ty::Error)
                || self.assignable(&expected, argument, sym)
            {
                continue;
            }
            let actual = self.type_of(argument, sym);
            if matches!(actual, Ty::Error) {
                continue;
            }
            self.error_with_help(
                codes::TYPE_MISMATCH,
                expr_span(argument),
                format!(
                    "cannot pass {} to the {} parameter of `{owner}::{name}`",
                    self.ty_display(&actual),
                    self.ty_display(&expected)
                ),
                format!(
                    "wrap it in a conversion, e.g. `{}(...)`",
                    self.ty_display(&expected)
                ),
            );
        }
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
        let saved_params = self.push_type_params(im.params.params.iter().map(|p| &p.name.text));
        let concrete_self = self.ast_ty(self_ty(im));
        let saved_self = self.current_self_ty.replace(Some(concrete_self));
        let backing = type_head_name(self_ty(im)).unwrap_or("<error>").to_string();
        for item in &im.items {
            let ImplItem::Fn(function) = item else {
                continue;
            };
            if function.is_pub && im.trait_.is_some() {
                self.error_with_help(
                    codes::PRIVATE_MEMBER,
                    function.name.span,
                    "trait implementation methods inherit the trait's visibility".to_string(),
                    "remove `pub` from this method".to_string(),
                );
            }
            if function.is_pub && self.entity_names.contains(&backing) {
                self.error_with_help(
                    codes::PRIVATE_MEMBER,
                    function.name.span,
                    format!(
                        "entity method `{backing}::{}` cannot be public yet",
                        function.name.text
                    ),
                    "expose behavior through ports; cross-hierarchy method calls do not yet have defined hardware semantics".to_string(),
                );
            }
        }
        self.check_impl_inner(im);
        self.current_self_ty.replace(saved_self);
        *self.type_params.borrow_mut() = saved_params;
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
        let mut declared_locals: HashSet<String> = HashSet::new();
        for item in &im.items {
            match item {
                ImplItem::Const(c) => {
                    self.check_const_not_entity(c);
                    self.check_expr(&c.value, &sym);
                }
                ImplItem::Let(l) => {
                    self.require_let_annotation(l);
                    self.check_struct_literal_fields(l, &sym);
                    self.check_signal_reset_value(l);
                    // Two `let`s of one name in the same body: the second
                    // silently shadowed a scalar, and for a struct produced C
                    // with the field locals defined twice, which failed at
                    // link with a clang error naming a mangled symbol.
                    if !declared_locals.insert(l.name.text.clone()) {
                        self.error_with_help(
                            codes::DUPLICATE_ITEM,
                            l.name.span,
                            format!("`{}` is declared more than once here", l.name.text),
                            "each `let` in a body introduces a new name; rename one \
                             of them, or assign to the first instead"
                                .to_string(),
                        );
                    }
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
                        let saved =
                            self.push_type_params(f.generics.params.iter().map(|p| &p.name.text));
                        // A method inlines into the body it is called from,
                        // so everything that body may not drive, it may not
                        // drive either: the entity's `in` ports and its
                        // `const`s, plus the `in` leaves of a view's role.
                        // Method bodies were checked with no restrictions at
                        // all, so each of those was accepted inside a method
                        // and rejected three lines away, written inline.
                        let mut body_dirs = self.self_view_dirs(&im.target);
                        body_dirs.illegal.extend(dirs.illegal.iter().cloned());
                        body_dirs
                            .plain_in_roots
                            .extend(dirs.plain_in_roots.iter().cloned());
                        body_dirs.consts.extend(dirs.consts.iter().cloned());
                        let mut body_ranged = ranged.clone();
                        // A parameter shadows the impl-level name it repeats,
                        // so `fn twice(a: unsigned[8]) { a = a + a; }` writes
                        // its own argument, not the entity's `in` port `a`.
                        let params: HashSet<&str> = f
                            .params
                            .iter()
                            .filter_map(|p| p.name.as_ref())
                            .map(|n| n.text.as_str())
                            .collect();
                        let shadowed = |name: &String| {
                            params.contains(name.split(['.', '[']).next().unwrap_or(name))
                        };
                        body_dirs.illegal.retain(|n| !shadowed(n));
                        body_dirs.plain_in_roots.retain(|n| !shadowed(n));
                        body_dirs.consts.retain(|n| !shadowed(n));
                        body_ranged.retain(|n, _| !shadowed(n));
                        // Types for what the function itself declares: its
                        // parameters, and `self` as the impl's target. Without
                        // them the body had no types at all, so the strict
                        // assignment-width rule never fired inside a method —
                        // `self.data = wide` silently truncated a 16-bit
                        // argument into an 8-bit field.
                        let mut body_sym: HashMap<String, Ty> = HashMap::new();
                        let mut body_index_bounds: HashMap<String, (i64, i64)> =
                            self.array_bounds.borrow().clone();
                        for param in &f.params {
                            if param.is_self {
                                body_sym.insert("self".to_string(), self.ast_ty(self_ty(im)));
                            } else if let (Some(n), Some(t)) = (&param.name, &param.ty) {
                                body_sym.insert(n.text.clone(), self.ast_ty(t));
                                if let Some(range) = self.declared_range(t) {
                                    body_ranged.insert(n.text.clone(), range);
                                }
                                body_index_bounds.remove(&n.text);
                                if let Some(range) = self.declared_index_bounds(t) {
                                    body_index_bounds.insert(n.text.clone(), range);
                                }
                            }
                        }
                        let expected = f.ret.as_ref().map(|ty| self.ast_ty(ty));
                        self.check_function_fallthrough(f, b, &body_sym);
                        self.check_block_with(
                            b,
                            &body_dirs,
                            &body_ranged,
                            &body_sym,
                            &body_index_bounds,
                            expected.as_ref(),
                        );
                        *self.type_params.borrow_mut() = saved;
                    }
                }
                ImplItem::ModeField { .. } => {}
                ImplItem::Stmt(s) => self.check_stmt(s, &dirs, &sym, &ranged, None),
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
        let mut consts: HashSet<String> = HashSet::new();
        let mut plain_in_roots = HashSet::new();
        let mut sym = HashMap::new();
        let mut ranged: HashMap<String, (i64, i64)> = HashMap::new();
        // Names are impl-local, so this starts empty for each one.
        self.array_bounds.borrow_mut().clear();
        if im.trait_.is_none() {
            if let Some(ports) = type_head_name(&im.target).and_then(|n| self.entities.get(n)) {
                for p in ports {
                    sym.insert(p.name.clone(), p.ty.clone());
                    if let Some(r) = p.range {
                        ranged.insert(p.name.clone(), r);
                    }
                    if let Some(bounds) = p.index_bounds {
                        self.array_bounds
                            .borrow_mut()
                            .insert(p.name.clone(), bounds);
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
                    if let Some(b) = l.ty.as_ref().and_then(|t| self.declared_index_bounds(t)) {
                        self.array_bounds
                            .borrow_mut()
                            .insert(l.name.text.clone(), b);
                    }
                }
                ImplItem::Const(c) => {
                    sym.insert(c.name.text.clone(), self.ast_ty(&c.ty));
                    consts.insert(c.name.text.clone());
                }
                _ => {}
            }
        }
        (
            PortDirs {
                illegal,
                plain_in_roots,
                consts,
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
        // Testbench assignments are sequential stimulus, not declarative
        // drivers. The native harness settles after every connected-signal
        // write, so even adjacent `clk = '1'; clk = '0';` assignments can be
        // observed by an edge-triggered process. Applying the hardware
        // source-order override rule here is therefore a false positive.
        if self.in_testbench.get() {
            return;
        }
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

    /// Check a free function or trait-default body with its declared value
    /// parameters in scope. Previously these bodies were checked against an
    /// empty symbol table, so every parameter expression became `Ty::Error`
    /// and suppressed the very type diagnostics Stage 4 was meant to provide.
    fn check_function_block(
        &mut self,
        function: &FnDecl,
        body: &Block,
        self_ty: Option<&Type>,
        require_complete_return: bool,
    ) {
        let mut names = HashMap::new();
        let mut ranged = HashMap::new();
        let mut index_bounds = HashMap::new();
        for parameter in &function.params {
            if parameter.is_self {
                names.insert(
                    "self".to_string(),
                    self_ty.map(|ty| self.ast_ty(ty)).unwrap_or(Ty::Error),
                );
            } else if let (Some(name), Some(ty)) = (&parameter.name, &parameter.ty) {
                names.insert(name.text.clone(), self.ast_ty(ty));
                if let Some(range) = self.declared_range(ty) {
                    ranged.insert(name.text.clone(), range);
                }
                if let Some(range) = self.declared_index_bounds(ty) {
                    index_bounds.insert(name.text.clone(), range);
                }
            }
        }
        let expected = function.ret.as_ref().map(|ty| self.ast_ty(ty));
        if require_complete_return {
            self.check_function_fallthrough(function, body, &names);
        }
        self.check_block_with(
            body,
            &PortDirs::default(),
            &ranged,
            &names,
            &index_bounds,
            expected.as_ref(),
        );
    }

    /// A value-returning function is an expression once inlined, so every
    /// reachable path must produce a value. Letting one fall off the end made
    /// lowering return `None`; callers then failed later as an opaque unknown
    /// driver instead of receiving a diagnostic at the function declaration.
    fn check_function_fallthrough(
        &mut self,
        function: &FnDecl,
        body: &Block,
        names: &HashMap<String, Ty>,
    ) {
        let Some(ret) = &function.ret else { return };
        if self.block_guarantees_return(body, names) {
            return;
        }
        let expected = crate::syntax::pretty::type_str(ret);
        self.error_with_help(
            codes::TYPE_MISMATCH,
            function.name.span,
            format!(
                "function `{}` can reach the end without returning {}",
                function.name.text, expected
            ),
            "return a value on every branch, or remove the declared return type".to_string(),
        );
    }

    fn block_guarantees_return(&self, body: &Block, outer: &HashMap<String, Ty>) -> bool {
        // Locals are block-scoped from the start during resolution. Mirror
        // that here so a `match` on a local can prove its domain exhaustive.
        let mut names = outer.clone();
        for statement in &body.stmts {
            if let Stmt::Let(declaration) = statement {
                names.insert(
                    declaration.name.text.clone(),
                    declaration
                        .ty
                        .as_ref()
                        .map(|ty| self.ast_ty(ty))
                        .unwrap_or(Ty::Error),
                );
            }
        }
        body.stmts
            .iter()
            .any(|statement| self.statement_guarantees_return(statement, &names))
    }

    fn statement_guarantees_return(&self, statement: &Stmt, names: &HashMap<String, Ty>) -> bool {
        match statement {
            Stmt::Return { .. } => true,
            Stmt::If(if_) => {
                self.block_guarantees_return(&if_.then, names)
                    && match if_.else_.as_deref() {
                        Some(ElseBranch::Block(block)) => {
                            self.block_guarantees_return(block, names)
                        }
                        Some(ElseBranch::If(inner)) => self.if_guarantees_return(inner, names),
                        None => false,
                    }
            }
            Stmt::Match(match_) => {
                !match_.arms.is_empty()
                    && match_
                        .arms
                        .iter()
                        .all(|arm| self.block_guarantees_return(&arm.body, names))
                    && self.match_is_exhaustive(&match_.scrutinee, &match_.arms, names)
            }
            Stmt::Let(_) | Stmt::Assign { .. } | Stmt::For { .. } | Stmt::Expr(_) => false,
        }
    }

    fn if_guarantees_return(&self, if_: &IfStmt, names: &HashMap<String, Ty>) -> bool {
        self.block_guarantees_return(&if_.then, names)
            && match if_.else_.as_deref() {
                Some(ElseBranch::Block(block)) => self.block_guarantees_return(block, names),
                Some(ElseBranch::If(inner)) => self.if_guarantees_return(inner, names),
                None => false,
            }
    }

    fn match_is_exhaustive(
        &self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        names: &HashMap<String, Ty>,
    ) -> bool {
        // A wildcard (including one inside an or-pattern) is sufficient for
        // every scrutinee type.
        if arms.iter().any(|arm| pattern_covers(&arm.pattern).1) {
            return true;
        }
        match self.type_of(scrutinee, names) {
            Ty::Named(id) => {
                let Some(enum_name) = self.resolved.def(id).map(|def| &def.name) else {
                    return false;
                };
                let Some(variants) = self.enum_variants.get(enum_name) else {
                    return false;
                };
                let covered: HashSet<String> = arms
                    .iter()
                    .flat_map(|arm| pattern_covers(&arm.pattern).0)
                    .collect();
                variants.iter().all(|variant| covered.contains(variant))
            }
            ty => self.numeric_match_is_exhaustive(&ty, arms),
        }
    }

    fn numeric_match_is_exhaustive(&self, ty: &Ty, arms: &[MatchArm]) -> bool {
        let Some((lo, hi)) = self.numeric_domain(ty) else {
            return false;
        };
        let mut covered = Vec::new();
        for arm in arms {
            if !collect_pattern_ranges(&arm.pattern, &mut covered) {
                return false;
            }
        }
        covered.sort_unstable();
        let mut frontier = lo;
        for (start, end) in covered {
            if start > frontier {
                return false;
            }
            frontier = frontier.max(end.saturating_add(1));
            if frontier > hi {
                return true;
            }
        }
        frontier > hi
    }

    /// A method body whose `self` carries directions — an impl on a view
    /// (`impl Stream StreamSource`) — must respect them. Writing an `in` leaf
    /// is rejected inline (`bus.ready = '1'` is `E-P004`), but method bodies
    /// were checked with no directions at all, so the same write hidden in
    /// `fn bad(self) { self.ready = '1'; }` was accepted *and driven*, which
    /// defeats the point of a view.
    fn check_block_with(
        &mut self,
        b: &Block,
        view_dirs: &PortDirs,
        bounds: &HashMap<String, (i64, i64)>,
        names: &HashMap<String, Ty>,
        index_bounds: &HashMap<String, (i64, i64)>,
        expected_return: Option<&Ty>,
    ) {
        // Every caller of this is a function body — a trait method, a free
        // function, or an impl method — so `return` is legal inside it.
        let saved = self.in_fn_body.replace(true);
        let saved_index_bounds = self.array_bounds.replace(index_bounds.clone());
        self.check_stmt_sequence(&b.stmts, view_dirs, names, bounds, expected_return);
        self.array_bounds.replace(saved_index_bounds);
        self.in_fn_body.set(saved);
    }

    /// Check one lexical statement sequence with every block-local declaration
    /// in scope, matching resolution's block semantics. Each nested block gets
    /// a cloned environment; its locals shadow outer names but do not leak out.
    fn check_stmt_sequence(
        &mut self,
        stmts: &[Stmt],
        outer_dirs: &PortDirs,
        outer_names: &HashMap<String, Ty>,
        outer_ranges: &HashMap<String, (i64, i64)>,
        expected_return: Option<&Ty>,
    ) {
        let saved_index_bounds = self.array_bounds.borrow().clone();
        let mut dirs = outer_dirs.clone();
        let mut names = outer_names.clone();
        let mut ranges = outer_ranges.clone();
        let mut locals = HashSet::new();

        // Resolution binds every local for the whole block before resolving
        // expressions, so collect their declared types first as well. A second
        // pass fills unconstrained array lengths from initializers once every
        // local name is known.
        for statement in stmts {
            let Stmt::Let(declaration) = statement else {
                continue;
            };
            if !locals.insert(declaration.name.text.clone()) {
                self.error_with_help(
                    codes::DUPLICATE_ITEM,
                    declaration.name.span,
                    format!(
                        "`{}` is declared more than once in this block",
                        declaration.name.text
                    ),
                    "rename one local, or assign to the first declaration instead".to_string(),
                );
            }
            let ty = declaration
                .ty
                .as_ref()
                .map(|ty| self.ast_ty(ty))
                .unwrap_or(Ty::Error);
            names.insert(declaration.name.text.clone(), ty);
            if let Some(range) = declaration
                .ty
                .as_ref()
                .and_then(|ty| self.declared_range(ty))
            {
                ranges.insert(declaration.name.text.clone(), range);
            } else {
                ranges.remove(&declaration.name.text);
            }
            self.array_bounds
                .borrow_mut()
                .remove(&declaration.name.text);
            if let Some(range) = declaration
                .ty
                .as_ref()
                .and_then(|ty| self.declared_index_bounds(ty))
            {
                self.array_bounds
                    .borrow_mut()
                    .insert(declaration.name.text.clone(), range);
            }

            let root = declaration.name.text.as_str();
            let shadowed = |candidate: &String| {
                candidate.split(['.', '[']).next().unwrap_or(candidate) == root
            };
            dirs.illegal.retain(|name| !shadowed(name));
            dirs.plain_in_roots.retain(|name| !shadowed(name));
            dirs.consts.retain(|name| !shadowed(name));
        }
        for statement in stmts {
            let Stmt::Let(declaration) = statement else {
                continue;
            };
            let Some(Ty::Array { len: 0, .. }) = names.get(&declaration.name.text) else {
                continue;
            };
            let inferred = match declaration.value.as_ref() {
                Some(Expr::StrLit { text, .. }) => {
                    u32::try_from(text.chars().count()).unwrap_or(u32::MAX)
                }
                Some(Expr::Array { elems, .. }) => u32::try_from(elems.len()).unwrap_or(u32::MAX),
                Some(value) => match self.type_of(value, &names) {
                    Ty::Array { len, .. } => len,
                    _ => 0,
                },
                None => 0,
            };
            if inferred != 0 {
                if let Some(Ty::Array { len, .. }) = names.get_mut(&declaration.name.text) {
                    *len = inferred;
                }
            }
        }

        self.lint_dead_assignments(stmts.iter());
        for statement in stmts {
            self.check_stmt(statement, &dirs, &names, &ranges, expected_return);
        }
        self.array_bounds.replace(saved_index_bounds);
    }

    fn check_stmt(
        &mut self,
        s: &Stmt,
        dirs: &PortDirs,
        sym: &HashMap<String, Ty>,
        ranged: &HashMap<String, (i64, i64)>,
        expected_return: Option<&Ty>,
    ) {
        match s {
            Stmt::Let(l) => {
                self.check_instance_placement(l);
                self.require_let_annotation(l);
                self.check_struct_literal_fields(l, sym);
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
            Stmt::If(i) => self.check_if(i, dirs, sym, ranged, expected_return),
            Stmt::Match(m) => {
                self.check_match_exhaustive(m, sym);
                self.check_unreachable_arms(&m.arms);
                for arm in &m.arms {
                    self.check_pattern_form(&arm.pattern);
                }
                self.check_expr(&m.scrutinee, sym);
                let saved = self.in_match_arm.replace(true);
                for arm in &m.arms {
                    self.check_stmt_sequence(&arm.body.stmts, dirs, sym, ranged, expected_return);
                }
                self.in_match_arm.set(saved);
            }
            Stmt::For {
                var, range, body, ..
            } => {
                self.check_expr(range, sym);
                let loop_ty = match range {
                    Expr::Range { lo, hi, .. } => {
                        self.check_index_value(lo, sym, "range bound");
                        self.check_index_value(hi, sym, "range bound");
                        Ty::Integer
                    }
                    // `check_expr` already reports that a partial range needs
                    // an indexed receiver; do not add a second iterable error.
                    Expr::PartialRange { .. } => Ty::Error,
                    _ => match self.type_of(range, sym) {
                        Ty::Array { elem, .. } => *elem,
                        Ty::Error => Ty::Error,
                        found => {
                            self.error_with_help(
                                codes::TYPE_MISMATCH,
                                expr_span(range),
                                format!(
                                    "a `for` loop needs a range or array, found {}",
                                    self.ty_display(&found)
                                ),
                                "use `left..right`, or iterate an array value".to_string(),
                            );
                            Ty::Error
                        }
                    },
                };
                let mut loop_sym = sym.clone();
                loop_sym.insert(var.text.clone(), loop_ty);
                self.check_stmt_sequence(&body.stmts, dirs, &loop_sym, ranged, expected_return);
            }
            Stmt::Expr(e) => {
                self.check_no_effect(e);
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
                } else {
                    if let (Some(expected), Some(value)) = (expected_return, value) {
                        if self.check_struct_literal_for_ty(expected, value, sym) {
                            return;
                        }
                    }
                    match (expected_return, value) {
                        (Some(expected), Some(value))
                            if !matches!(expected, Ty::Error)
                                && !self.assignable(expected, value, sym) =>
                        {
                            let actual = self.type_of(value, sym);
                            self.error_with_help(
                                codes::TYPE_MISMATCH,
                                expr_span(value),
                                format!(
                                    "cannot return {} from a function declared to return {}",
                                    self.ty_display(&actual),
                                    self.ty_display(expected)
                                ),
                                format!(
                                    "return a {}, or convert the value explicitly",
                                    self.ty_display(expected)
                                ),
                            );
                        }
                        (Some(expected), None) if !matches!(expected, Ty::Error) => {
                            self.error(
                                codes::TYPE_MISMATCH,
                                *span,
                                format!(
                                    "this function must return a {} value",
                                    self.ty_display(expected)
                                ),
                            );
                        }
                        (None, Some(value)) => {
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr_span(value),
                                "this function has no declared return type".to_string(),
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// A statement expression that is not a call cannot do anything: siox has
    /// no side-effecting operators, so `x;` and `a + 1;` compute a value and
    /// discard it. Lowering's catch-all dropped every non-call shape without a
    /// word, so a misspelled name compiled clean — and `continue;`, a Rust
    /// habit siox does not have, looked accepted while a `for` body ran every
    /// iteration anyway. (`break;` was at least a parse error.)
    fn check_no_effect(&mut self, e: &Expr) {
        if matches!(e, Expr::Call { .. }) {
            return;
        }
        let loop_keyword = matches!(e, Expr::Path(p) if p.segments.len() == 1
            && matches!(p.segments[0].text.as_str(), "continue" | "break"));
        let help = if loop_keyword {
            "siox has no loop control — a `for` is unrolled at elaboration, so \
             every iteration exists in the hardware. Guard the body with an \
             `if` instead"
        } else {
            "a statement has an effect only if it assigns (`y = ...`) or calls \
             something (`assert!(...)`, `s.send(v)`)"
        };
        self.error_with_help(
            codes::NO_EFFECT_STATEMENT,
            crate::syntax::ast::expr_span(e),
            "this statement has no effect".to_string(),
            help.to_string(),
        );
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
        expected_return: Option<&Ty>,
    ) {
        self.check_condition(&i.cond, sym);
        self.check_expr(&i.cond, sym);
        self.check_stmt_sequence(&i.then.stmts, dirs, sym, ranged, expected_return);
        match i.else_.as_deref() {
            Some(ElseBranch::Block(b)) => {
                self.check_stmt_sequence(&b.stmts, dirs, sym, ranged, expected_return)
            }
            Some(ElseBranch::If(inner)) => self.check_if(inner, dirs, sym, ranged, expected_return),
            None => {}
        }
    }

    /// A condition's type must implement `Boolean` (spec 3.16, generalized).
    /// `Bit`/`Bool` have built-in impls; user types opt in with `impl Boolean
    /// for T`; `Logic` has none, so it still requires an explicit comparison.
    /// An unknown (`Error`) condition type is skipped to avoid false positives.
    fn check_condition(&mut self, cond: &Expr, sym: &HashMap<String, Ty>) {
        let ty = self.type_of(cond, sym);
        if matches!(ty, Ty::Void) {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(cond),
                "a procedure call has no value and cannot be used as a condition".to_string(),
            );
            return;
        }
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
    /// The inclusive value range a numeric scrutinee can hold, or `None` when
    /// it is unbounded (`integer`) or not numeric at all.
    fn numeric_domain(&self, ty: &Ty) -> Option<(i128, i128)> {
        let Ty::Array { len, family, .. } = ty else {
            return None;
        };
        let width = *len;
        // Beyond 127 bits the domain no longer fits the arithmetic here, and a
        // match over it could not be spelled out arm by arm anyway.
        if width == 0 || width > 127 {
            return None;
        }
        match family.as_deref() {
            Some("signed") => {
                let half = 1i128 << (width - 1);
                Some((-half, half - 1))
            }
            Some("unsigned") => Some((0, (1i128 << width) - 1)),
            _ => None,
        }
    }

    /// Warn when a match on a numeric value leaves part of its domain
    /// uncovered. The enum form of this has always been checked; the numeric
    /// form was not, so a missing case was silent — and lowering then had no
    /// base arm for it.
    fn check_numeric_arms_exhaustive(&mut self, ty: &Ty, arms: &[MatchArm], span: Span) {
        let Some((lo, hi)) = self.numeric_domain(ty) else {
            return;
        };
        // Collect covered intervals. A bit pattern carries don't-cares, whose
        // coverage is not an interval, so its presence makes the answer
        // unknown and the check steps aside rather than guessing.
        let mut covered: Vec<(i128, i128)> = Vec::new();
        for arm in arms {
            if !collect_pattern_ranges(&arm.pattern, &mut covered) {
                return;
            }
        }
        covered.sort_unstable();
        // Walk the domain, consuming intervals that touch or overlap the
        // frontier. The first interval starting beyond it opens the gap; if
        // none does, the gap runs to the top of the domain.
        let mut frontier = lo;
        let mut gap_end = hi;
        for (start, end) in covered {
            if start > frontier {
                gap_end = (start - 1).min(hi);
                break;
            }
            frontier = frontier.max(end.saturating_add(1));
            if frontier > hi {
                return;
            }
        }
        if frontier > hi {
            return;
        }
        let missing = if frontier == gap_end {
            format!("`{frontier}`")
        } else {
            format!("`{frontier}..{gap_end}`")
        };
        self.sink.emit(
            Diagnostic::warning(format!("non-exhaustive match: {missing} is not covered"))
                .with_code(codes::NON_EXHAUSTIVE_MATCH)
                .at(span)
                .help("add the missing arms, or a `_` wildcard"),
        );
    }

    /// A signal's initializer is its **reset value** (spec 3.4), so it has to
    /// be a constant. A runtime expression there was silently dropped — the
    /// signal simply kept its default, and `let v: unsigned[8] = if c { 7 }
    /// else { 9 };` read 0 with nothing said. A testbench `let` is sequential
    /// storage, where a computed initial value is meaningful and is evaluated.
    fn check_signal_reset_value(&mut self, l: &LetDecl) {
        if self.in_testbench.get() {
            return;
        }
        let Some(value) = &l.value else { return };
        // An instance's connection block, a struct/array literal and a
        // constant expression are all fine; anything that reads a signal is
        // not, because there is no time at which a reset value could sample it.
        if !matches!(value, Expr::IfExpr { .. } | Expr::Match { .. }) {
            return;
        }
        self.error_with_help(
            codes::TYPE_MISMATCH,
            expr_span(value),
            format!("`{}`'s initial value is not constant", l.name.text),
            "a signal's initializer is its reset value, so it must be constant; \
             drive it instead (`let x: T; x = <expr>;`)"
                .to_string(),
        );
    }

    fn check_pattern_domains(&mut self, ty: &Ty, arms: &[MatchArm]) {
        for arm in arms {
            self.check_pattern_domain(ty, &arm.pattern);
        }
    }

    fn check_pattern_domain(&mut self, ty: &Ty, pattern: &Pattern) {
        if matches!(ty, Ty::Error) {
            return;
        }
        match pattern {
            Pattern::Wildcard | Pattern::CharLit { .. } => {}
            Pattern::Or { alts, .. } => {
                for alternative in alts {
                    self.check_pattern_domain(ty, alternative);
                }
            }
            Pattern::Path(path) if path.segments.len() == 2 => {
                let qualifier = &path.segments[0];
                match self.enum_operand_name(ty) {
                    Some(expected) if qualifier.text == expected => {}
                    Some(expected) => self.error_with_help(
                        codes::TYPE_MISMATCH,
                        path.span,
                        format!(
                            "pattern `{}` belongs to enum `{}`, but the matched value is `{expected}`",
                            path.segments[1].text, qualifier.text
                        ),
                        format!("use a `{expected}::…` variant in this match"),
                    ),
                    None => self.error(
                        codes::TYPE_MISMATCH,
                        path.span,
                        format!(
                            "enum pattern `{}::{}` cannot match a {} value",
                            qualifier.text,
                            path.segments[1].text,
                            self.ty_display(ty)
                        ),
                    ),
                }
            }
            // Bare/deeper paths receive their spelling diagnostic from
            // `check_pattern_form`; avoid adding a dependent type error.
            Pattern::Path(_) => {}
            Pattern::Range { span, .. } => {
                let numeric = matches!(ty, Ty::Integer | Ty::Real)
                    || matches!(
                        ty,
                        Ty::Array {
                            family: Some(_),
                            ..
                        }
                    );
                if !numeric {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        format!(
                            "an integer pattern cannot match a {} value",
                            self.ty_display(ty)
                        ),
                    );
                }
            }
            Pattern::BitPattern { span, .. } => {
                if !matches!(
                    ty,
                    Ty::Array {
                        family: Some(_),
                        ..
                    }
                ) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        format!(
                            "a bit pattern needs a packed vector, found {}",
                            self.ty_display(ty)
                        ),
                    );
                }
            }
        }
    }

    /// A character pattern names a variant of a character-valued enum, so it
    /// is only meaningful against one. Expression position has always rejected
    /// the rest — `s == '0'` on a numeric or a `State` is "a character literal
    /// has no numeric identity" — but pattern position was checked by nobody
    /// when char patterns landed, and a character has no intrinsic value, so
    /// the arm compared two unrelated discriminants and *matched*:
    /// `match s { '0' => .. }` on `enum State { Idle, Run }` selected the arm,
    /// because `State::Idle` and `'0'` are both 0.
    fn check_char_patterns(&mut self, ty: &Ty, arms: &[MatchArm]) {
        let variants = match ty {
            Ty::Named(id) => self
                .resolved
                .def(*id)
                .map(|d| d.name.clone())
                .and_then(|name| self.enum_variants.get(&name).map(|v| (name, v.clone()))),
            _ => None,
        };
        let mut bad: Vec<(char, Span)> = Vec::new();
        for arm in arms {
            collect_char_patterns(&arm.pattern, &mut bad);
        }
        for (ch, span) in bad {
            match &variants {
                None => self.error(
                    codes::TYPE_MISMATCH,
                    span,
                    "a character literal has no numeric identity; convert it \
                     through an encoding table (std::text)"
                        .to_string(),
                ),
                Some((name, vs)) if !vs.iter().any(|v| v == &format!("'{ch}'")) => self
                    .error_with_help(
                        codes::INVALID_PATTERN,
                        span,
                        format!("`'{ch}'` is not a variant of enum `{name}`"),
                        format!(
                            "`{name}` names its variants without quotes; a character pattern \
                             only matches an enum declared with character literals, like `Logic`"
                        ),
                    ),
                Some(_) => {}
            }
        }
    }

    fn check_arms_exhaustive(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        span: Span,
        sym: &HashMap<String, Ty>,
    ) {
        let ty = self.type_of(scrutinee, sym);
        self.check_pattern_domains(&ty, arms);
        self.check_char_patterns(&ty, arms);
        let Ty::Named(id) = ty else {
            // A numeric scrutinee has a domain rather than a variant list.
            // Only enums were ever checked, so `match s { 0 => .. }` on an
            // `unsigned[2]` passed silently while the same hole over an enum
            // was reported.
            self.check_numeric_arms_exhaustive(&ty, arms, span);
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
            Pattern::Path(path) if path.segments.len() != 2 => {
                self.sink.emit(
                    Diagnostic::error("an enum pattern must be written as `Type::Variant`")
                        .with_code(codes::INVALID_PATTERN)
                        .at(path.span)
                        .help("import the enum type, then use exactly its type and variant names"),
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
    fn check_unreachable_arms(&mut self, arms: &[MatchArm]) {
        let mut after_wildcard = false;
        let mut seen: HashSet<String> = HashSet::new();
        // Inclusive integer ranges already matched (a bare literal is lo==hi).
        let mut ranges: Vec<(i64, i64)> = Vec::new();
        for arm in arms {
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
                    Pattern::CharLit { ch, .. } => {
                        let var = format!("'{ch}'");
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
    /// `unsigned` share `unsigned`). `Void`/`Error`/array types have no name.
    fn type_kind_name(&self, t: &Ty) -> Option<String> {
        match t {
            Ty::Integer => Some("integer".to_string()),
            Ty::Real => Some("real".to_string()),
            Ty::Char => Some("Char".to_string()),
            Ty::Named(id) => self.resolved.def(*id).map(|d| d.name.clone()),
            Ty::Array {
                family: Some(name), ..
            } => Some(name.clone()),
            Ty::Array { .. } | Ty::Void | Ty::Error => None,
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
        if let Some(root) = target_root_name(target) {
            if dirs.consts.contains(&root) {
                self.error_with_help(
                    codes::INVALID_ASSIGN_TARGET,
                    expr_span(target),
                    format!("cannot assign to `{root}`, which is a `const`"),
                    "a `const` is fixed at elaboration; declare it as a `let` \
                     if it needs to be driven"
                        .to_string(),
                );
                return;
            }
        }
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
                    Self::index_contract_type_matches(i, input.as_deref(), &owner)
                        && Self::index_contract_type_matches(v, value_ty.as_deref(), &owner)
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

    fn index_contract_type_matches(
        declared: &Option<String>,
        actual: Option<&str>,
        owner: &str,
    ) -> bool {
        declared.is_none()
            || declared.as_deref() == actual
            || (declared.as_deref() == Some("Self") && actual == Some(owner))
    }

    /// Spec 3.5: an attribute's value must match the type its declaration gives.
    fn check_attr_value(&mut self, a: &Attr) {
        let name = a
            .name
            .segments
            .last()
            .map(|s| s.text.as_str())
            .unwrap_or("");
        let Some(value) = &a.value else {
            // A declared attribute with a value type needs one. A bare `#[speed]`
            // on `attr speed: integer` used to pass unexamined and was carried
            // through elaboration into `--emit tree` as `#[speed]`, so a
            // synthesis or constraint backend reading it found an attribute with
            // no number in it. `Bool` is exempt: a bare flag reads as `true`, the
            // way the marker attributes (`#[top]`, `#[test]`) do.
            match self.attr_value_kinds.get(name).copied() {
                Some(AttrValueTy::Integer) => self.error_with_help(
                    codes::INVALID_ATTR_VALUE_TYPE,
                    a.name.span,
                    format!("attribute `{name}` needs an integer value"),
                    format!(
                        "write `#[{name} = <n>]`; it is declared with a value type, \
                             so a bare `#[{name}]` carries nothing to the backend"
                    ),
                ),
                Some(AttrValueTy::Str) => self.error_with_help(
                    codes::INVALID_ATTR_VALUE_TYPE,
                    a.name.span,
                    format!("attribute `{name}` needs a string value"),
                    format!(
                        "write `#[{name} = \"…\"]`; it is declared with a value type, \
                             so a bare `#[{name}]` carries nothing to the backend"
                    ),
                ),
                _ => {}
            }
            return;
        };
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
        // `read<T>` constructs either one `T`, a fixed array of `T`, or a
        // string. Its dynamic/context-sized result deliberately has no
        // standalone `Ty`, so validate it against the declared destination
        // here rather than letting `Ty::Error` silently accept every shape.
        if let Some(requested_type) = read_call_type(value) {
            let requested = self.ast_ty(requested_type);
            let text = matches!(
                requested,
                Ty::Array {
                    ref elem,
                    family: None,
                    ..
                } if matches!(elem.as_ref(), Ty::Char)
            );
            let compatible = if text {
                matches!(
                    lhs,
                    Ty::Array {
                        ref elem,
                        family: None,
                        ..
                    } if matches!(elem.as_ref(), Ty::Char)
                )
            } else {
                lhs == requested
                    || matches!(
                        lhs,
                        Ty::Array {
                            ref elem,
                            family: None,
                            ..
                        } if elem.as_ref() == &requested
                    )
            };
            if !compatible {
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(value),
                    format!(
                        "`read<{}>` cannot initialize {}; declare one value or an array of the requested type",
                        crate::syntax::pretty::type_str(requested_type),
                        self.ty_display(&lhs)
                    ),
                );
            }
            return;
        }
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
        if self.check_struct_literal_for_ty(&lhs, value, sym) {
            return;
        }
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

    /// Enforce a generic fn's type contracts at the call site (spec: generic
    /// bounds). Each type parameter is inferred from value parameters whose
    /// declared type names it. Repeated uses must agree, and a bound `T: Tr`
    /// requires the inferred type to satisfy `Tr`. Fns inline, so the call
    /// *is* the monomorphization — checking here gives an early, clear error
    /// instead of a post-inline one.
    fn check_generic_bounds(&mut self, callee: &Expr, args: &[Expr], sym: &HashMap<String, Ty>) {
        let Expr::Path(p) = callee else { return };
        let Some(name) = p.segments.last() else {
            return;
        };
        if p.segments.len() >= 2 {
            let owner = &p.segments[p.segments.len() - 2].text;
            if self
                .methods
                .contains_key(&(owner.clone(), name.text.clone()))
            {
                return;
            }
        }
        let Some((generics, params)) = self.generic_fns.get(&name.text).cloned() else {
            return;
        };
        for gp in &generics {
            let occurrences: Vec<&Expr> = params
                .iter()
                .zip(args)
                .filter_map(|(declared, argument)| {
                    declared
                        .as_ref()
                        .filter(|ty| Self::is_direct_type_param(ty, &gp.name.text))
                        .map(|_| argument)
                })
                .collect();

            // Infer from the first non-literal value. Integer/character/bit
            // literals are contextual and may adopt that inferred type.
            let inferred_expr = occurrences
                .iter()
                .copied()
                .find(|argument| !Self::is_contextual_literal(argument))
                .or_else(|| occurrences.first().copied());
            let inferred = inferred_expr.map(|argument| self.type_of(argument, sym));

            if let Some(expected) = inferred.as_ref().filter(|ty| !matches!(ty, Ty::Error)) {
                for argument in &occurrences {
                    let actual = self.type_of(argument, sym);
                    if matches!(actual, Ty::Error)
                        || if Self::is_contextual_literal(argument) {
                            self.assignable(expected, argument, sym)
                        } else {
                            &actual == expected
                        }
                    {
                        continue;
                    }
                    self.error_with_help(
                        codes::TYPE_MISMATCH,
                        expr_span(argument),
                        format!(
                            "generic parameter `{}` was inferred as {}, but this argument is {}",
                            gp.name.text,
                            self.ty_display(expected),
                            self.ty_display(&actual)
                        ),
                        format!(
                            "pass one consistent type for every `{}` parameter, or convert this argument explicitly",
                            gp.name.text
                        ),
                    );
                }
            }

            let Some(bound) = &gp.bound else { continue };
            let Some(trait_name) = type_head_name(bound) else {
                continue;
            };
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

    fn is_direct_type_param(ty: &Type, name: &str) -> bool {
        matches!(ty, Type::Path(path) if path.segments.len() == 1 && path.segments[0].text == name)
    }

    fn is_contextual_literal(expression: &Expr) -> bool {
        matches!(
            expression,
            Expr::Int { text, .. } if !text.contains('.')
        ) || matches!(
            expression,
            Expr::CharLit { .. } | Expr::BitStrLit { .. } | Expr::StrLit { .. }
        )
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
                    Expr::Path(p) => p
                        .segments
                        .last()
                        .map(|name| name.text.as_str())
                        .unwrap_or(""),
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
            Expr::Path(p) if p.segments.last().is_some_and(|name| name.text == "resize") => {
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

    /// `T()` is explicit default construction and `T(value)` is conversion;
    /// no type constructor accepts more than one value. Several lowerers read
    /// only `args.first()`, so extra arguments otherwise vanished silently.
    fn check_conversion_arity(&mut self, callee: &Expr, args: &[Expr]) {
        if args.len() <= 1 {
            return;
        }
        let is_conversion = match callee {
            Expr::Path(path) => {
                let Some(name) = path.segments.last().map(|segment| &segment.text) else {
                    return;
                };
                let associated = path.segments.len() >= 2
                    && self.methods.contains_key(&(
                        path.segments[path.segments.len() - 2].text.clone(),
                        name.clone(),
                    ));
                !associated && !self.fn_arity.contains_key(name) && self.is_conversion_name(name)
            }
            Expr::Index { base, .. } => {
                matches!(base.as_ref(), Expr::Path(path) if path.segments.last()
                    .is_some_and(|name| self.is_conversion_name(&name.text)))
            }
            _ => false,
        };
        if is_conversion {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(callee),
                format!(
                    "a type constructor takes zero or one argument, but {} were given",
                    args.len()
                ),
            );
        }
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

    /// Resolve a declared free-function return at its call site. Concrete
    /// returns are direct; a bare generic return (`-> T`) takes the type
    /// inferred from the corresponding value parameter.
    fn free_call_return_type(&self, name: &str, args: &[Expr], sym: &HashMap<String, Ty>) -> Ty {
        let Some(declared) = self.fn_return_types.get(name) else {
            return self.runtime_call_return_type(name);
        };
        let Some(declared) = declared else {
            return Ty::Void;
        };
        if let Type::Path(path) = declared {
            if path.segments.len() == 1 {
                let parameter_name = &path.segments[0].text;
                if let Some((generics, params)) = self.generic_fns.get(name) {
                    let is_generic = generics
                        .iter()
                        .any(|parameter| parameter.name.text == *parameter_name);
                    if is_generic {
                        return params
                            .iter()
                            .zip(args)
                            .find_map(|(parameter, argument)| {
                                parameter
                                    .as_ref()
                                    .filter(|ty| Self::is_direct_type_param(ty, parameter_name))
                                    .map(|_| self.type_of(argument, sym))
                            })
                            .unwrap_or(Ty::Error);
                    }
                }
            }
        }
        self.ast_ty(declared)
    }

    /// Return contracts for compiler/runtime-provided functions which have no
    /// source `FnDecl`. Context-sized file reads deliberately remain unknown;
    /// their initializer target determines the result layout.
    fn runtime_call_return_type(&self, name: &str) -> Ty {
        match name {
            "rand" | "randint" => Ty::Integer,
            "uniform" => Ty::Real,
            "exists" => self.ty_from_head("Bool"),
            "seed" | "print" | "assert" | "warn" | "await" | "wait" | "tick" | "clock" | "stop"
            | "finish" => Ty::Void,
            "read" | "resize" => Ty::Error,
            _ => Ty::Error,
        }
    }

    /// A call to a *declared* function must pass one argument per parameter.
    /// Nothing checked this: a short call left a parameter unbound, and a short
    /// `extern "C"` call passed a garbage argument to real native code.
    /// Conversions, method calls and runtime-provided std functions have no
    /// declaration here and are skipped.
    fn check_call_arity(&mut self, callee: &Expr, args: &[Expr], sym: &HashMap<String, Ty>) {
        let Expr::Path(p) = callee else { return };
        let Some(name) = p.segments.last() else {
            return;
        };
        if p.segments.len() >= 2 {
            let owner = &p.segments[p.segments.len() - 2].text;
            if self
                .methods
                .contains_key(&(owner.clone(), name.text.clone()))
                || (name.text == "new" && self.is_conversion_name(owner))
            {
                return;
            }
        }
        let Some(&want) = self.fn_arity.get(&name.text) else {
            // No declaration here is normal for a conversion (`unsigned[8](x)`,
            // `Logic(b)`) and for the runtime-provided std functions, and a
            // mistake for anything else — but the two were indistinguishable,
            // so a misspelled or unimported call passed every stage and failed
            // in the backend as "unsupported call `abs` in testbench
            // expression", blaming the emitter for a missing `using`.
            if !self.callee_is_declared(&name.text) {
                self.error_with_help(
                    codes::UNKNOWN_NAME,
                    expr_span(callee),
                    format!("unknown function `{}`", name.text),
                    "declare it, or import it with `using` — a std function needs \
                     its module (`using std::math::{abs};`)"
                        .to_string(),
                );
            }
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
        self.check_call_arg_types(&name.text, args, sym);
    }

    /// Each argument must be assignable to its parameter, by the same rule an
    /// assignment uses. Only the count was checked, so a `real` handed to an
    /// `integer` parameter passed its f64 bits through as an integer, and a
    /// `signed[8]` passed its raw bit pattern — `abs(-5)` returned 251.
    fn check_call_arg_types(&mut self, name: &str, args: &[Expr], sym: &HashMap<String, Ty>) {
        let Some(params) = self.fn_param_types.get(name).cloned() else {
            return;
        };
        for (arg, pty) in args.iter().zip(params.iter()) {
            let Some(pty) = pty else { continue };
            let want = self.ast_ty(pty);
            if self.check_struct_literal_for_ty(&want, arg, sym) {
                continue;
            }
            if want == Ty::Error || self.assignable(&want, arg, sym) {
                continue;
            }
            let got = self.type_of(arg, sym);
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

    /// Compiler/runtime-provided functions have no source `FnDecl`, so retain
    /// their complete public contract here: arity, argument domains, macro
    /// spelling, and removed migration forms. This keeps malformed calls from
    /// surviving until C harness generation.
    fn check_runtime_call_contract(
        &mut self,
        callee: &Expr,
        type_args: &[Type],
        args: &[Expr],
        bang: bool,
        sym: &HashMap<String, Ty>,
    ) {
        let Expr::Path(path) = callee else { return };
        let Some(name) = path.segments.last() else {
            return;
        };
        if path.segments.len() >= 2 {
            let owner = &path.segments[path.segments.len() - 2].text;
            if self
                .methods
                .contains_key(&(owner.clone(), name.text.clone()))
                || (name.text == "new" && self.is_conversion_name(owner))
            {
                return;
            }
        }
        let function = name.text.as_str();
        // A source declaration shadows a runtime primitive with the same
        // leaf name (`fn read<Bus>(bus)` is common in protocol traits).
        if self.fn_arity.contains_key(function) {
            if !type_args.is_empty() {
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(callee),
                    "explicit call type arguments are currently reserved for `read<T>`".to_string(),
                );
            }
            return;
        }
        if function == "read" {
            if type_args.len() != 1 {
                self.error_with_help(
                    codes::TYPE_MISMATCH,
                    expr_span(callee),
                    "`read` needs exactly one constructed type".to_string(),
                    "write `read<string>(\"text.txt\")` for UTF-8 or `read<integer>(\"data.bin\")` for binary"
                        .to_string(),
                );
            } else {
                let requested = self.ast_ty(&type_args[0]);
                let supported = matches!(requested, Ty::Integer)
                    || matches!(
                        requested,
                        Ty::Array {
                            ref elem,
                            family: None,
                            ..
                        } if matches!(elem.as_ref(), Ty::Char)
                    )
                    || matches!(
                        requested,
                        Ty::Array {
                            len,
                            family: Some(_),
                            ..
                        } if len > 0
                    );
                if !supported {
                    self.error(
                        codes::TYPE_MISMATCH,
                        type_head_span(&type_args[0]).unwrap_or(expr_span(callee)),
                        "`read<T>` needs `string`, `integer`, or a sized packed numeric type constructible from `integer`"
                            .to_string(),
                    );
                }
            }
        } else if !type_args.is_empty() {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(callee),
                "explicit call type arguments are currently reserved for `read<T>`".to_string(),
            );
        }
        if matches!(function, "tick" | "clock") {
            let replacement = if function == "tick" {
                "drive the clock and `await` each half-period explicitly"
            } else {
                "write `clk = not clk after <half-period>;`"
            };
            self.error_with_help(
                codes::UNKNOWN_NAME,
                expr_span(callee),
                format!("`{function}()` was removed"),
                replacement.to_string(),
            );
            return;
        }

        let exact = match function {
            "rand" | "uniform" | "stop" | "finish" => Some(0),
            "seed" | "exists" | "read" | "await" => Some(1),
            "randint" | "resize" => Some(2),
            _ => None,
        };
        if let Some(expected) = exact {
            if args.len() != expected {
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(callee),
                    format!(
                        "`{function}` takes {expected} argument(s) but {} were given",
                        args.len()
                    ),
                );
                return;
            }
        } else if matches!(function, "print" | "assert" | "warn") && args.is_empty() {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(callee),
                format!("`{function}!` needs at least one argument"),
            );
            return;
        } else if !matches!(
            function,
            "print"
                | "assert"
                | "warn"
                | "rand"
                | "uniform"
                | "stop"
                | "finish"
                | "seed"
                | "exists"
                | "read"
                | "await"
                | "randint"
                | "resize"
        ) {
            return;
        }

        if matches!(function, "print" | "assert" | "warn") && !bang {
            self.error_with_help(
                codes::TYPE_MISMATCH,
                expr_span(callee),
                format!("`{function}` is a macro-like compiler primitive"),
                format!("write `{function}!(...)`"),
            );
            return;
        }

        match function {
            "seed" | "randint" => {
                for argument in args {
                    if !self.assignable(&Ty::Integer, argument, sym) {
                        self.error(
                            codes::TYPE_MISMATCH,
                            expr_span(argument),
                            format!("`{function}` expects integer argument(s)"),
                        );
                    }
                }
            }
            "exists" | "read" => {
                let Some(argument) = args.first() else { return };
                if !matches!(argument, Expr::StrLit { .. }) {
                    self.error_with_help(
                        codes::TYPE_MISMATCH,
                        expr_span(argument),
                        format!("`{function}` needs a literal file path"),
                        format!("write `{function}(\"path/to/file\")`"),
                    );
                }
            }
            "print" => {
                let Some(format) = args.first() else { return };
                if !matches!(format, Expr::StrLit { .. }) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        expr_span(format),
                        "`print!` needs a string-literal format".to_string(),
                    );
                }
            }
            "assert" | "warn" => {
                let Some(condition) = args.first() else {
                    return;
                };
                self.check_condition(condition, sym);
                if let Some(message) = args.get(1) {
                    if !matches!(message, Expr::StrLit { .. }) {
                        self.error(
                            codes::TYPE_MISMATCH,
                            expr_span(message),
                            format!("`{function}!` message must be a string literal"),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether a bare call name is something the language declares somewhere:
    /// a `fn`, a type used as a conversion, or one of the runtime-provided std
    /// functions that never reach `fn_arity`. Anything else does not exist.
    fn callee_is_declared(&self, name: &str) -> bool {
        // Runtime-provided (std::rand, std::fs) — declared by the runtime, not
        // by a `fn`. Kept beside `check_runtime_call_arity`'s list.
        if matches!(
            name,
            "rand" | "uniform" | "randint" | "seed" | "exists" | "read"
        ) {
            return true;
        }
        // Primitives the compiler implements rather than std declaring: the
        // stimulus and reporting forms, simulation control (`stop`/`finish`,
        // spec 3.24), and the width builtin. Kept in step with the emitter's
        // own match — anything it handles by name belongs here.
        if matches!(
            name,
            "print"
                | "assert"
                | "warn"
                | "await"
                | "wait"
                | "tick"
                | "clock"
                | "stop"
                | "finish"
                | "resize"
        ) {
            return true;
        }
        // A conversion names a value type. Entities are instantiated with a
        // struct literal and are never callable values.
        if self.is_conversion_name(name) {
            return true;
        }
        // A method reached through UFCS-ish sugar, or a trait method the
        // receiver supplies: if any type implements a method of this name it
        // is not an unknown *function*.
        self.methods.keys().any(|(_, m)| m == name)
    }

    fn is_conversion_name(&self, name: &str) -> bool {
        if matches!(name, "integer" | "real" | "Char" | "string")
            || self.is_vector_family(name)
            || self.structs.contains_key(name)
            || self.enum_variants.contains_key(name)
        {
            return true;
        }
        let Some(alias) = self.aliases.get(name) else {
            return false;
        };
        self.resolve_alias_type(alias)
            .as_ref()
            .and_then(type_head_name)
            .is_some_and(|head| !self.entities.contains_key(head))
    }

    /// The write restrictions `self` carries inside an impl on a view: each
    /// leaf the role declares `in` is an input there, so `self.<leaf>` cannot
    /// be driven. Empty for any other impl target, where `self` is plain data.
    fn self_view_dirs(&self, target: &Type) -> PortDirs {
        let mut dirs = PortDirs::default();
        let Some(key) = type_identity(target) else {
            return dirs;
        };
        let Some(leaves) = self.view_dirs.get(&key) else {
            return dirs;
        };
        for (field, dir) in leaves {
            if *dir == Direction::In {
                dirs.illegal.insert(format!("self.{field}"));
            }
        }
        dirs
    }

    /// Bring `names` into scope as generic parameters, returning the previous
    /// set for the caller to restore.
    fn push_type_params<'n>(&self, names: impl Iterator<Item = &'n String>) -> HashSet<String> {
        let mut scope = self.type_params.borrow_mut();
        let saved = scope.clone();
        scope.extend(names.cloned());
        saved
    }

    /// An entity may be instantiated only at the root layer of another
    /// entity's body, or inside a generate `for`/`if`. A `match` arm and a
    /// function body are neither.
    ///
    /// Both used to be accepted and then quietly dropped: elaboration gathers
    /// instances from the root, from a generate-`for` and from a generate-`if`
    /// and from nothing else, so an instance in a `match` arm simply never
    /// existed — the design compiled and ran without it. A function was worse,
    /// failing much later with "the driver for `y` contains an Unknown", which
    /// names neither the function nor the instantiation.
    ///
    /// Instantiating from a function would also mean a function could bring a
    /// process into being, which only an entity may do.
    fn check_instance_placement(&mut self, l: &LetDecl) {
        let Some(head) = l.ty.as_ref().and_then(type_head_name) else {
            return;
        };
        if !self.entities.contains_key(head) || self.type_params.borrow().contains(head) {
            return;
        }
        let context = if self.in_fn_body.get() {
            "a function"
        } else if self.in_match_arm.get() {
            "a `match` arm"
        } else {
            return;
        };
        self.error_with_help(
            codes::INSTANCE_PLACEMENT,
            l.span,
            format!("an entity cannot be instantiated in {context}"),
            "hardware is structural: instantiate at the top of an entity body, \
             or inside a generate `for`/`if` whose condition folds to a \
             constant. A `match` on a signal selects a value at run time, and \
             cannot bring an instance into being"
                .to_string(),
        );
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

    fn is_integer_like_index(ty: &Ty) -> bool {
        matches!(ty, Ty::Integer | Ty::Error)
            || matches!(
                ty,
                Ty::Array {
                    family: Some(_),
                    ..
                }
            )
    }

    fn check_index_value(&mut self, value: &Expr, sym: &HashMap<String, Ty>, description: &str) {
        let ty = self.type_of(value, sym);
        if Self::is_integer_like_index(&ty) {
            return;
        }
        self.error_with_help(
            codes::TYPE_MISMATCH,
            expr_span(value),
            format!(
                "{description} must be an integer or packed numeric value, found {}",
                self.ty_display(&ty)
            ),
            "convert it explicitly to `integer` or a packed numeric type".to_string(),
        );
    }

    /// Validate intrinsic array subscripts and every range endpoint. A custom
    /// scalar `Index<I, _>` may choose another input type, but `Range` itself
    /// always stores integer left/right bounds.
    fn check_index_operand(&mut self, base: &Expr, index: &Expr, sym: &HashMap<String, Ty>) {
        match index {
            Expr::Range { lo, hi, .. } => {
                self.check_index_value(lo, sym, "range bound");
                self.check_index_value(hi, sym, "range bound");
            }
            Expr::PartialRange { lo, hi, .. } => {
                if let Some(lo) = lo {
                    self.check_index_value(lo, sym, "range bound");
                }
                if let Some(hi) = hi {
                    self.check_index_value(hi, sym, "range bound");
                }
            }
            _ if matches!(self.type_of(base, sym), Ty::Array { .. }) => {
                self.check_index_value(index, sym, "array index");
            }
            _ => {}
        }
    }

    /// A constant bit index or slice outside a packed vector's width has no
    /// hardware meaning — it lowered to `Unknown` and surfaced much later as a
    /// generic "no engine can run this design". Both packed vectors and data
    /// arrays use their declared labels; a width/count spelling falls back to
    /// `0..len-1`.
    fn check_index_bounds(&mut self, base: &Expr, index: &Expr, sym: &HashMap<String, Ty>) {
        // What we can bound-check, and how to name it. A packed vector or data
        // array may carry nonzero declared labels. An instance array (`let s:
        // Sub[4]`) is always declared with a plain count and remains 0-based.
        let (lo, hi, noun) = match self.type_of(base, sym) {
            Ty::Array {
                len,
                family: Some(_),
                ..
            } => declared_bounds_of(base, &self.array_bounds)
                .map(|(lo, hi)| (lo, hi, "bit"))
                .unwrap_or((0, len as i64 - 1, "bit")),
            Ty::Array {
                elem,
                len,
                family: None,
            } if self.is_entity_ty(&elem) => (0, len as i64 - 1, "instance"),
            // A data array's bounds come from its declaration, not its
            // length: `Logic[15..8]` is indexed `8..15`.
            Ty::Array { family: None, .. } => match declared_bounds_of(base, &self.array_bounds) {
                Some((lo, hi)) => (lo, hi, "element"),
                None => return,
            },
            _ => return,
        };
        if hi < lo {
            return; // parametric: not known yet
        }
        let len = hi - lo + 1;
        let mut check = |v: i64, e: &Expr| {
            if v < lo || v > hi {
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(e),
                    match noun {
                        "bit" => {
                            format!("bit {v} is outside `{lo}..{hi}` of this {len}-bit vector")
                        }
                        "instance" => format!(
                            "instance {v} is outside `{lo}..{hi}` of this {len}-instance array"
                        ),
                        _ => {
                            format!("index {v} is outside `{lo}..{hi}` of this {len}-element array")
                        }
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
                    .any(|(i, _)| Self::index_contract_type_matches(i, input.as_deref(), &owner))
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

    /// Whether two expressions can inhabit one value domain without an
    /// explicit conversion. This is symmetric because contextual literals may
    /// take the other side's type, and integer/real choose the real domain.
    fn value_expressions_compatible(
        &self,
        left: &Expr,
        right: &Expr,
        sym: &HashMap<String, Ty>,
    ) -> bool {
        let left_ty = self.type_of(left, sym);
        let right_ty = self.type_of(right, sym);
        if matches!(left_ty, Ty::Error) || matches!(right_ty, Ty::Error) {
            return true;
        }
        if compatible(&left_ty, &right_ty) || compatible(&right_ty, &left_ty) {
            return true;
        }
        if matches!(
            (&left_ty, &right_ty),
            (Ty::Integer, Ty::Real) | (Ty::Real, Ty::Integer)
        ) {
            return true;
        }
        self.assignable(&right_ty, left, sym) || self.assignable(&left_ty, right, sym)
    }

    fn value_domain_anchor<'b>(
        &self,
        values: &[&'b Expr],
        sym: &HashMap<String, Ty>,
    ) -> Option<&'b Expr> {
        values
            .iter()
            .copied()
            .find(|value| {
                !Self::is_contextual_literal(value)
                    && !matches!(self.type_of(value, sym), Ty::Error)
            })
            .or_else(|| values.first().copied())
    }

    fn values_share_domain(&self, values: &[&Expr], sym: &HashMap<String, Ty>) -> bool {
        let Some(anchor) = self.value_domain_anchor(values, sym) else {
            return false;
        };
        values
            .iter()
            .all(|value| self.value_expressions_compatible(anchor, value, sym))
    }

    /// Infer the value domain selected by conditional arms rather than simply
    /// copying the first arm. In particular, integer/real joins are real and
    /// contextual literals take the concrete arm's type.
    fn joined_value_type(&self, values: &[&Expr], sym: &HashMap<String, Ty>) -> Ty {
        if !self.values_share_domain(values, sym) {
            return Ty::Error;
        }
        let types: Vec<Ty> = values
            .iter()
            .map(|value| self.type_of(value, sym))
            .collect();
        if types.iter().any(|ty| matches!(ty, Ty::Real))
            && types.iter().all(|ty| matches!(ty, Ty::Integer | Ty::Real))
        {
            return Ty::Real;
        }
        self.value_domain_anchor(values, sym)
            .map(|value| self.type_of(value, sym))
            .unwrap_or(Ty::Error)
    }

    /// Comparisons may inspect two differently constrained arrays without
    /// assigning either into the other. Their element domains must agree, but
    /// unequal lengths are meaningful (`"abc" == "abcd"` is simply false)
    /// and must not be treated as an assignment-width error.
    fn comparison_domains_compatible(
        &self,
        left: &Expr,
        right: &Expr,
        sym: &HashMap<String, Ty>,
    ) -> bool {
        if self.value_expressions_compatible(left, right, sym) {
            return true;
        }
        match (self.type_of(left, sym), self.type_of(right, sym)) {
            (Ty::Array { elem: left, .. }, Ty::Array { elem: right, .. }) => {
                compatible(&left, &right) || compatible(&right, &left)
            }
            _ => false,
        }
    }

    /// Find the output of the operator overload selected by these operands.
    /// `Self` is the impl owner, not a wildcard: it only matches the owner's
    /// type. A contextual literal may also take that type at the call site.
    fn operator_output(
        &self,
        symbol: &str,
        left: &Ty,
        right: &Ty,
        coerces_to_owner: bool,
    ) -> Option<Option<String>> {
        let owner = self.ty_head(left)?;
        let input = self.ty_head(right)?;
        self.operator_sigs
            .get(&(symbol.to_string(), owner.clone()))
            .and_then(|signatures| {
                signatures.iter().find(|(declared, _)| {
                    declared.as_deref() == Some(input.as_str())
                        || (declared.as_deref() == Some("Self") && input == owner)
                        || (coerces_to_owner
                            && (declared.as_deref() == Some("Self")
                                || declared.as_deref() == Some(owner.as_str())))
                })
            })
            .map(|(_, output)| output.clone())
            .or_else(|| {
                // A field-less Vector newtype inherits a blanket array
                // operator from its element. Keep this fallback separate from
                // ordinary impl ownership: an unrelated overload for the same
                // owner must not make every right-hand type match.
                let same_domain = input == owner || coerces_to_owner;
                let element = self.vector_elements.get(&owner)?;
                let requirement = self.blanket_array_impls.get(symbol)?;
                (same_domain
                    && self
                        .trait_impls
                        .get(requirement)
                        .is_some_and(|types| types.contains(element)))
                .then_some(Some(owner))
            })
    }

    fn operator_accepts(
        &self,
        symbol: &str,
        left: &Ty,
        right: &Ty,
        coerces_to_owner: bool,
    ) -> bool {
        self.operator_output(symbol, left, right, coerces_to_owner)
            .is_some()
    }

    fn operator_accepts_expr(
        &self,
        symbol: &str,
        left: &Ty,
        right: &Ty,
        rhs: &Expr,
        sym: &HashMap<String, Ty>,
    ) -> bool {
        let contextual_rhs = Self::is_contextual_literal(rhs) && self.assignable(left, rhs, sym);
        let numeric_kernel_coercion = matches!(
            (left, right),
            (
                Ty::Array {
                    family: Some(_),
                    ..
                },
                Ty::Integer
            )
        );
        self.operator_accepts(
            symbol,
            left,
            right,
            contextual_rhs || numeric_kernel_coercion,
        )
    }

    fn check_comparison_operands(
        &mut self,
        op: &BinOp,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
        sym: &HashMap<String, Ty>,
    ) -> bool {
        // Keep the existing focused character/numeric error and enum/integer
        // suspicious-comparison warning as the sole diagnostics for those
        // deliberately recognized source forms.
        let char_numeric = [(lhs, rhs), (rhs, lhs)].iter().any(|(literal, other)| {
            matches!(literal, Expr::CharLit { .. })
                && matches!(
                    self.type_of(other, sym),
                    Ty::Array {
                        family: Some(_),
                        ..
                    } | Ty::Integer
                        | Ty::Real
                )
        });
        let enum_integer = [(lhs, rhs), (rhs, lhs)].iter().any(|(literal, other)| {
            matches!(literal, Expr::Int { text, .. } if !text.contains('.'))
                && self.enum_operand_name(&self.type_of(other, sym)).is_some()
        });
        // Equality against an enum discriminant is deliberately a lint (the
        // warning below points users to a variant). Ordering has no analogous
        // intrinsic meaning and must continue through normal `<=>` checking.
        let suspicious_enum_equality = enum_integer && matches!(op, BinOp::Eq | BinOp::Ne);
        if char_numeric || suspicious_enum_equality {
            return true;
        }
        if self.comparison_domains_compatible(lhs, rhs, sym) {
            return false;
        }
        let left = self.type_of(lhs, sym);
        let right = self.type_of(rhs, sym);
        if self.operator_accepts_expr("<=>", &left, &right, rhs, sym) {
            return false;
        }
        self.error_with_help(
            codes::TYPE_MISMATCH,
            span,
            format!(
                "cannot compare {} and {} with `{}`",
                self.ty_display(&left),
                self.ty_display(&right),
                crate::syntax::pretty::bin_op(op)
            ),
            "convert one operand to the other's type, or implement `Operator<\"<=>\", Input, Ordering>`"
                .to_string(),
        );
        true
    }

    fn check_intrinsic_binary_operands(
        &mut self,
        op: &BinOp,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
        sym: &HashMap<String, Ty>,
    ) {
        if !matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Shl | BinOp::Shr
        ) {
            return;
        }
        let left = self.type_of(lhs, sym);
        let right = self.type_of(rhs, sym);
        if matches!(left, Ty::Error) || matches!(right, Ty::Error) {
            return;
        }
        let symbol = crate::syntax::pretty::bin_op(op);
        if self.operator_accepts_expr(symbol, &left, &right, rhs, sym) {
            return;
        }
        // Nominal left operands are diagnosed by the trait-specific check
        // below, which can offer the exact impl spelling without duplicating
        // this intrinsic-domain diagnostic.
        if self.named_operand_name(lhs, sym).is_some() {
            return;
        }
        let packed = |ty: &Ty| {
            matches!(
                ty,
                Ty::Array {
                    family: Some(_),
                    ..
                }
            )
        };
        let valid = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                matches!(
                    (&left, &right),
                    (Ty::Integer, Ty::Integer)
                        | (Ty::Integer, Ty::Real)
                        | (Ty::Real, Ty::Integer)
                        | (Ty::Real, Ty::Real)
                ) || (matches!(left, Ty::Integer) && packed(&right))
                    || (packed(&left) && matches!(right, Ty::Integer))
            }
            BinOp::Shl | BinOp::Shr => {
                matches!((&left, &right), (Ty::Integer, Ty::Integer))
                    || (matches!(left, Ty::Integer) && packed(&right))
                    || (packed(&left) && matches!(right, Ty::Integer))
            }
            _ => true,
        };
        if !valid {
            self.error_with_help(
                codes::TYPE_MISMATCH,
                span,
                format!(
                    "cannot apply `{symbol}` to {} and {}",
                    self.ty_display(&left),
                    self.ty_display(&right)
                ),
                format!(
                    "use numeric operands, convert explicitly, or implement `Operator<\"{symbol}\", Input, Output>`"
                ),
            );
        }
    }

    fn assignable(&self, lhs: &Ty, value: &Expr, sym: &HashMap<String, Ty>) -> bool {
        match value {
            // A decimal literal is already `real`; narrowing it to integer or
            // packed bits requires an explicit conversion.
            Expr::Int { text, .. } if text.contains('.') => {
                matches!(lhs, Ty::Real | Ty::Error)
            }
            // An integer literal also initialises `real` (`.re = 10` is 10.0)
            // and is contextual for packed numeric families.
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
                // The expression walk owns an incompatible-branch error. Once
                // it has done so, do not blame the enclosing assignment too.
                !self.value_expressions_compatible(then, els, sym)
                    || (self.assignable(lhs, then, sym) && self.assignable(lhs, els, sym))
            }
            // A match expression has the assignment context of its consumer
            // on every arm, just like an if-expression has it on both
            // branches. Looking only at `type_of` used the first arm and let a
            // later incompatible value be reinterpreted silently.
            Expr::Match { arms, .. } => {
                let values: Vec<&Expr> = arms.iter().filter_map(MatchArm::value_expr).collect();
                let common = self.values_share_domain(&values, sym);
                !values.is_empty()
                    && (!common || values.iter().all(|value| self.assignable(lhs, value, sym)))
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
            // Kernel integers widen exactly into the real domain. This is the
            // same promotion used by mixed real arithmetic and by std's
            // `Complex::from(integer)`; the reverse direction remains an
            // explicit `integer(value)` conversion because it truncates.
            _ if matches!(lhs, Ty::Real) && matches!(self.type_of(value, sym), Ty::Integer) => true,
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
                    // The whole construct is unavailable in this phase. Do
                    // not descend into its receiver and turn one rejected
                    // analogue expression into unrelated value/type errors.
                    return;
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
                if matches!(self.type_of(scrutinee, sym), Ty::Void) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        expr_span(scrutinee),
                        "a procedure call has no value and cannot be matched".to_string(),
                    );
                    return;
                }
                // An expression must yield a value for every case it can meet,
                // so a missing variant matters at least as much here as in a
                // statement — and this form was not checked at all.
                self.check_arms_exhaustive(scrutinee, arms, *span, sym);
                // Unreachability was statement-only for the same reason
                // exhaustiveness was: the two forms share `MatchArm` but not
                // the code that walks it.
                self.check_unreachable_arms(arms);
                for arm in arms {
                    self.check_pattern_form(&arm.pattern);
                    if let Some(v) = arm.value_expr() {
                        self.check_expr(v, sym);
                    }
                }
                let values: Vec<&Expr> = arms.iter().filter_map(MatchArm::value_expr).collect();
                let anchor = self.value_domain_anchor(&values, sym);
                if let Some(anchor) = anchor {
                    for value in values {
                        if !self.value_expressions_compatible(anchor, value, sym) {
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr_span(value),
                                format!(
                                    "match arms yield incompatible types: {} and {}",
                                    self.ty_display(&self.type_of(anchor, sym)),
                                    self.ty_display(&self.type_of(value, sym))
                                ),
                            );
                        }
                    }
                }
            }
            Expr::Field { base, field, .. } => {
                self.check_expr(base, sym);
                if matches!(self.type_of(base, sym), Ty::Void) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        expr_span(base),
                        "a procedure call has no value and therefore has no fields".to_string(),
                    );
                    return;
                }
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
                if matches!(self.type_of(base, sym), Ty::Void)
                    || matches!(self.type_of(index, sym), Ty::Void)
                {
                    self.error(
                        codes::TYPE_MISMATCH,
                        expr_span(e),
                        "a procedure call has no value and cannot be indexed".to_string(),
                    );
                    return;
                }
                self.check_index_operand(base, index, sym);
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
                if matches!(self.type_of(rhs, sym), Ty::Void) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        "a procedure call has no value for a unary operator".to_string(),
                    );
                    return;
                }
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
                if !self.value_expressions_compatible(then, els, sym) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        expr_span(els),
                        format!(
                            "`if` branches yield incompatible types: {} and {}",
                            self.ty_display(&self.type_of(then, sym)),
                            self.ty_display(&self.type_of(els, sym))
                        ),
                    );
                }
            }
            Expr::Binary { op, lhs, rhs, span } => {
                self.check_expr(lhs, sym);
                self.check_expr(rhs, sym);
                if matches!(self.type_of(lhs, sym), Ty::Void)
                    || matches!(self.type_of(rhs, sym), Ty::Void)
                {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        "a procedure call has no value for a binary operator".to_string(),
                    );
                    return;
                }
                let comparison_handled = if is_comparison(op) {
                    self.check_comparison_fit(lhs, rhs, sym);
                    self.check_comparison_operands(op, lhs, rhs, *span, sym)
                } else {
                    false
                };
                self.check_intrinsic_binary_operands(op, lhs, rhs, *span, sym);
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
                    // An integer literal on the right coerces to a
                    // Self-typed parameter, as it does for the symbolic
                    // operators — `a + 255` worked and `a xor 255` did not,
                    // purely because the textual half dispatches here.
                    let matching = self.operator_accepts_expr(symbol, &lhs_ty, &rhs_ty, rhs, sym);
                    // A plain array has no type head, so the exact
                    // `(symbol, owner)` lookup above cannot see the blanket
                    // `for T[]` impl that lifts the element's operator.
                    // `and`/`or` never reach here — they are built-in
                    // operators with their own array handling — which is why
                    // only the textual half of the logic family failed on
                    // arrays.
                    let lifted = is_liftable_array_key(symbol)
                        && self
                            .array_operand_element(&lhs_ty)
                            .zip(self.array_operand_element(&rhs_ty))
                            .is_some_and(|(left, right)| {
                                left == right && self.has_operator_impl(symbol, &left)
                            });
                    if !matching && !lifted {
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
                // A user struct/enum or nominal vector operand needs an exact
                // operator-trait overload (spec 3.25); overload resolution
                // includes the right operand, not merely the impl owner.
                // Equality on enums alone stays intrinsic (a discriminant
                // compare); structs derive it from `<=>` like ordering does.
                if !matches!(op, BinOp::Custom { .. }) {
                    if let Some(name) = self.named_operand_name(lhs, sym) {
                        let intrinsic_enum_equality = matches!(op_str, "==" | "!=")
                            && self.enum_operand_name(&self.type_of(lhs, sym)).is_some();
                        let intrinsic_vector_operator = (is_comparison(op)
                            || matches!(
                                op,
                                BinOp::Add
                                    | BinOp::Sub
                                    | BinOp::Mul
                                    | BinOp::Div
                                    | BinOp::Shl
                                    | BinOp::Shr
                            ))
                            && self.is_packed_vector_newtype(&name);
                        if !intrinsic_enum_equality && !intrinsic_vector_operator {
                            let operator = if is_comparison(op) { "<=>" } else { op_str };
                            let left = self.type_of(lhs, sym);
                            let right = self.type_of(rhs, sym);
                            // `Error` is the recovery type for an expression this
                            // pass cannot yet model (generic constants, composite
                            // field calls, and similar forms). Never reinterpret
                            // it as a known mismatching overload argument.
                            let accepts = matches!(right, Ty::Error)
                                || self.operator_accepts_expr(operator, &left, &right, rhs, sym);
                            if !accepts && !comparison_handled {
                                self.error(
                                    codes::TYPE_MISMATCH,
                                    *span,
                                    format!(
                                        "no `{op_str}` operator for `{name}` with a right operand of type `{}`",
                                        self.ty_display(&right)
                                    ),
                                );
                            }
                        }
                    }
                }
            }
            Expr::Call {
                callee,
                type_args,
                args,
                bang,
                span,
            } => {
                // A method callee is a `Field` node, but its name is a method,
                // not a field — check the receiver and let `check_method_exists`
                // judge the name, so one mistake yields one diagnostic.
                match callee.as_ref() {
                    Expr::Field { base, .. } => self.check_expr(base, sym),
                    // A bare path in callee position is checked by
                    // `check_known_call` below. Treating it as an ordinary
                    // value here would double-report unknown functions and
                    // reject runtime/compiler-provided primitives that have
                    // no source declaration.
                    Expr::Path(_) => {}
                    _ => self.check_expr(callee, sym),
                }
                for a in args {
                    self.check_expr(a, sym);
                }
                if *bang {
                    self.check_format_arity(callee, args);
                } else if matches!(callee.as_ref(), Expr::Field { .. }) {
                    self.check_method_call(callee, args, sym);
                } else if matches!(callee.as_ref(), Expr::Path(path) if path.segments.len() > 1) {
                    self.check_associated_call(callee, args, sym);
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
                self.check_conversion_arity(callee, args);
                self.check_generic_bounds(callee, args, sym);
                self.check_call_arity(callee, args, sym);
                self.check_runtime_call_contract(callee, type_args, args, *bang, sym);
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
                // The alphabet follows from the radix rather than being listed
                // again: a digit is well-formed exactly when `to_digit`
                // accepts it there.
                if !crate::syntax::is_radix_prefix(*base) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        format!("bit-string prefix `{base}` has no compiler evaluation yet"),
                    );
                    return;
                }
                let radix = crate::syntax::radix_of(*base);
                // `_` separates digits (`x"AB_CD"`) as it does in `0xAB_CD`;
                // a literal of nothing but separators is still empty.
                let significant: String = crate::syntax::radix_digits(digits).collect();
                let ok = !significant.is_empty() && significant.chars().all(|c| c.is_digit(radix));
                let kind = if radix == 16 { "hex" } else { "octal" };
                if !ok {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        format!("invalid {kind} bit-string literal `{base}\"{digits}\"`"),
                    );
                }
            }
            Expr::Path(path) => {
                let local = path
                    .segments
                    .first()
                    .is_some_and(|name| path.segments.len() == 1 && sym.contains_key(&name.text));
                // Enum variants may be used unqualified (`return Equal;`).
                // Resolution intentionally leaves those to type context, so
                // they are known values even without a span -> DefId entry.
                let unqualified_variant = path.segments.first().is_some_and(|name| {
                    path.segments.len() == 1
                        && self
                            .enum_variants
                            .values()
                            .any(|variants| variants.contains(&name.text))
                });
                if !local && !unqualified_variant && self.resolved.resolved(path.span).is_none() {
                    self.error(
                        codes::UNKNOWN_NAME,
                        path.span,
                        format!(
                            "unknown value `{}`",
                            path.segments
                                .iter()
                                .map(|segment| segment.text.as_str())
                                .collect::<Vec<_>>()
                                .join("::")
                        ),
                    );
                }
            }
            Expr::Int { .. } | Expr::CharLit { .. } | Expr::StrLit { .. } => {}
        }
    }

    /// Whether a system event/history attribute can observe this type.
    /// Named values are classified from their resolved declaration instead of
    /// being accepted blindly (in particular, entity instances are not data).
    fn is_digital_ty(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Void | Ty::Error => false,
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
            Expr::IfExpr { then, els, .. } => self.joined_value_type(&[then, els], sym),
            Expr::Match { arms, .. } => {
                let values: Vec<&Expr> = arms.iter().filter_map(MatchArm::value_expr).collect();
                self.joined_value_type(&values, sym)
            }
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
                if !crate::syntax::is_radix_prefix(*base) {
                    return Ty::Error;
                }
                let bits = crate::syntax::bits_per_digit(*base);
                Ty::Array {
                    elem: Box::new(self.ty_from_head("Logic")),
                    family: Some("unsigned".to_string()),
                    len: crate::syntax::radix_digits(digits).count() as u32 * bits,
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
                let contextual_rhs =
                    Self::is_contextual_literal(rhs) && self.assignable(&lhs_ty, rhs, sym);
                let numeric_kernel_coercion = matches!(
                    (&lhs_ty, &rhs_ty),
                    (
                        Ty::Array {
                            family: Some(_),
                            ..
                        },
                        Ty::Integer
                    )
                );
                if let Some(Some(output)) = self.operator_output(
                    op_str,
                    &lhs_ty,
                    &rhs_ty,
                    contextual_rhs || numeric_kernel_coercion,
                ) {
                    if let Some(owner) = self.ty_head(&lhs_ty) {
                        if output == "Self" || output == owner {
                            return lhs_ty;
                        }
                        if self.ty_head(&rhs_ty).as_deref() == Some(output.as_str()) {
                            return rhs_ty;
                        }
                        return self.ty_from_head(&output);
                    }
                }
                // Runtime lowering promotes either ordering of mixed
                // integer/real arithmetic to f64. This applies only to
                // arithmetic: a real shift count (or logical/custom operator)
                // does not turn the whole expression into a real value.
                if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div)
                    && (matches!(lhs_ty, Ty::Real) || matches!(rhs_ty, Ty::Real))
                {
                    return Ty::Real;
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
                        let output = self
                            .index_sigs
                            .get(&("Index".to_string(), owner.clone()))
                            .and_then(|sigs| {
                                sigs.iter()
                                    .find(|(i, _)| {
                                        Self::index_contract_type_matches(
                                            i,
                                            input.as_deref(),
                                            &owner,
                                        )
                                    })
                                    .and_then(|(_, output)| output.as_deref())
                            });
                        match output {
                            Some("Self") => base_ty,
                            Some(name) if input.as_deref() == Some(name) => {
                                self.type_of(index, sym)
                            }
                            Some(name) => self.ty_from_head(name),
                            None => Ty::Error,
                        }
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
                    name => self.free_call_return_type(name, args, sym),
                },
                Expr::Path(path) if path.segments.len() >= 2 => {
                    let owner = &path.segments[path.segments.len() - 2].text;
                    let name = &path.segments[path.segments.len() - 1].text;
                    match self.methods.get(&(owner.clone(), name.clone())) {
                        Some(Some(ret)) => self.ast_ty_for_owner(ret, &self.ty_from_head(owner)),
                        Some(None) => Ty::Void,
                        None if name == "new" && self.is_conversion_name(owner) => {
                            self.ty_from_head(owner)
                        }
                        None => self.free_call_return_type(name, args, sym),
                    }
                }
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
                        Some(Some(ret)) => self.ast_ty_for_owner(ret, &recv),
                        Some(None) => Ty::Void,
                        None => Ty::Error,
                    }
                }
                _ => Ty::Error,
            },
            // A data field access (`p.data`, `self.data`): the field's declared
            // type. This was `Ty::Error`, which suppresses every check that
            // consults it — so the strict assignment-width rule had nothing to
            // compare and `self.data = wide` truncated a 16-bit value into an
            // 8-bit field in silence.
            Expr::Field { base, field, .. } => {
                let recv = self.type_of(base, sym);
                match self
                    .ty_head(&recv)
                    .and_then(|head| self.field_decl_ty(&head, &field.text))
                {
                    Some(ty) => self.ast_ty(&ty),
                    None => Ty::Error,
                }
            }
        }
    }

    /// The type-head name used to key impl methods: a named type's def name,
    /// a kernel type's spelling, or the nominal family of an indexed array.
    /// The struct behind a view, given the view's bare name. `views` is keyed
    /// by the `(view, backing)` pair, so a bare name resolves only when one
    /// view carries it; an ambiguous name is left alone rather than guessed.
    fn view_backing(&self, name: &str) -> Option<String> {
        let prefix = format!("{name}@");
        let mut targets = self.views.keys().filter_map(|k| k.strip_prefix(&prefix));
        let first = targets.next()?;
        targets.next().is_none().then(|| first.to_string())
    }

    fn field_visibility_for(&self, head: &str, field: &str) -> Option<MemberVisibility> {
        let mut current = head.to_string();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current.clone()) {
                return None;
            }
            if let Some(visibility) = self
                .field_visibility
                .get(&(current.clone(), field.to_string()))
            {
                return Some(visibility.clone());
            }
            current = self
                .structs
                .get(&current)
                .and_then(|(base, _)| base.as_ref())
                .and_then(type_head_name)?
                .to_string();
        }
    }

    fn module_of(&self, span: Span) -> String {
        self.file_modules
            .get(&span.file)
            .cloned()
            .unwrap_or_else(|| format!("<file:{}>", span.file.0))
    }

    /// The element type head of a *plain* array operand (`Logic[3]` ->
    /// `Logic`). A nominal vector family has its own head and is handled by
    /// the ordinary lookup.
    fn array_operand_element(&self, t: &Ty) -> Option<String> {
        match t {
            Ty::Array {
                elem, family: None, ..
            } => self.ty_head(elem),
            _ => None,
        }
    }

    /// Whether `owner` implements the operator `symbol` on itself.
    fn has_operator_impl(&self, symbol: &str, owner: &str) -> bool {
        self.operator_sigs
            .get(&(symbol.to_string(), owner.to_string()))
            .is_some_and(|sigs| {
                sigs.iter().any(|(declared, _)| {
                    declared.as_deref() == Some(owner) || declared.as_deref() == Some("Self")
                })
            })
    }

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

    /// Whether a Vector family is represented by one packed word sequence
    /// rather than flattened aggregate fields. These newtypes retain the
    /// backend's intrinsic arithmetic fallback; a multi-field struct that
    /// merely opts into `Vector` does not.
    fn is_packed_vector_newtype(&self, name: &str) -> bool {
        self.vector_families.contains(name)
            && self
                .structs
                .get(name)
                .is_some_and(|(_, fields)| fields.is_empty())
    }

    /// A constant initializer must lie inside a value-range-constrained
    /// numeric type (`let b: integer<0..255> = 300;` is an error). Literal
    /// bounds only; named ranges and dynamic values are runtime checks later.
    /// The declared bounds of a ranged numeric (`integer<left..right>`),
    /// resolving any alias chain (`using Byte = integer<0..255>; using Octet =
    /// Byte`). `None` for every other type.
    fn declared_range(&self, decl_ty: &Type) -> Option<(i64, i64)> {
        let resolved = self.resolve_alias_type(decl_ty)?;
        let t = &resolved;
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

    /// Field names and values in a struct literal, against the declared type.
    /// Instance construction reuses `Construct`, so this only fires when the
    /// target names a known struct — an entity is not in `structs`.
    fn check_struct_literal_fields(&mut self, l: &LetDecl, sym: &HashMap<String, Ty>) {
        let Some(ty) = &l.ty else { return };
        let Some(value) = &l.value else { return };
        self.check_struct_literal_value(ty, value, sym);
    }

    fn check_struct_literal_value(
        &mut self,
        declared: &Type,
        value: &Expr,
        sym: &HashMap<String, Ty>,
    ) {
        let Some(ty) = self.resolve_alias_type(declared) else {
            return;
        };
        let Some(head) = type_head_name(&ty) else {
            return;
        };
        if !self.structs.contains_key(head) {
            return;
        }
        let head = head.to_string();
        let expected = self.ast_ty(&ty);
        self.check_struct_literal_for_head(&expected, &head, value, sym);
    }

    /// Validate a contextual struct literal when only the consumer's semantic
    /// type is available (assignments, returns, and call arguments). Returns
    /// true when this was a struct-literal context, so the caller does not also
    /// emit a generic mismatch for the literal's intentionally context-free
    /// `Ty::Error`.
    fn check_struct_literal_for_ty(
        &mut self,
        expected: &Ty,
        value: &Expr,
        sym: &HashMap<String, Ty>,
    ) -> bool {
        if !matches!(value, Expr::Construct { .. } | Expr::Concat { .. }) {
            return false;
        }
        let Ty::Named(id) = expected else {
            return false;
        };
        let Some(head) = self
            .resolved
            .def(*id)
            .map(|definition| definition.name.clone())
        else {
            return false;
        };
        if !self.structs.contains_key(&head) {
            return false;
        }
        self.check_struct_literal_for_head(expected, &head, value, sym);
        true
    }

    fn check_struct_literal_for_head(
        &mut self,
        expected: &Ty,
        head: &str,
        value: &Expr,
        sym: &HashMap<String, Ty>,
    ) {
        let fields = self.base_struct_fields_named(head);
        if fields.is_empty() {
            return;
        }

        if let Some(private) = fields.iter().find_map(|field| {
            let visibility = self.field_visibility_for(head, field)?;
            (!visibility.is_pub && !self.member_access_allowed(&visibility, expr_span(value)))
                .then_some(visibility)
        }) {
            self.sink.emit(
                Diagnostic::error(format!(
                    "cannot construct `{head}` here because it has private fields"
                ))
                .with_code(codes::PRIVATE_MEMBER)
                .at(expr_span(value))
                .label(private.span, "private field declared here")
                .help(format!(
                    "construct it in `impl {head}` and expose a `pub` constructor, or make its representation fields `pub`"
                )),
            );
            return;
        }

        let (args, spread, span) = match value {
            Expr::Construct {
                ty: explicit,
                args,
                spread,
                span,
            } => {
                // `let a: A = B { ... }` is a type mismatch even though both
                // sides are constructions. `check_init` deliberately leaves
                // name-less construction to this contextual checker.
                if let Some(actual) = explicit {
                    let actual = self.ast_ty(actual);
                    if !compatible(expected, &actual) {
                        self.error(
                            codes::TYPE_MISMATCH,
                            *span,
                            format!(
                                "cannot construct {} where {} is required",
                                self.ty_display(&actual),
                                self.ty_display(expected)
                            ),
                        );
                        return;
                    }
                }
                (args.as_slice(), spread.as_deref(), *span)
            }
            // A name-less positional literal (`{ a, b }`) is represented as
            // concatenation until its declared struct type supplies context.
            Expr::Concat { parts, span } => {
                if parts.len() > fields.len() {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        format!(
                            "literal for `{head}` has {} values but only {} fields",
                            parts.len(),
                            fields.len()
                        ),
                    );
                }
                for (field, value) in fields.iter().zip(parts) {
                    self.check_struct_field_value(head, field, value, sym);
                }
                return;
            }
            _ => return,
        };

        if let Some(base) = spread {
            if !self.assignable(expected, base, sym) {
                let actual = self.type_of(base, sym);
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(base),
                    format!(
                        "struct spread for `{head}` has type {}, expected {}",
                        self.ty_display(&actual),
                        self.ty_display(expected)
                    ),
                );
            }
        }

        let mut seen: HashSet<String> = HashSet::new();
        let mut positional = false;
        for (position, c) in args.iter().enumerate() {
            match &c.field {
                // A misspelled name was dropped whole: the field kept its
                // default and the literal still type-checked.
                Some(f) if !fields.iter().any(|n| n == &f.text) => self.error_with_help(
                    codes::TYPE_MISMATCH,
                    f.span,
                    format!("struct `{head}` has no field `{}`", f.text),
                    format!("`{head}` has: {}", fields.join(", ")),
                ),
                Some(f) => {
                    seen.insert(f.text.clone());
                    if let Some(value) = &c.value {
                        self.check_struct_field_value(head, &f.text, value, sym);
                    }
                }
                None => {
                    positional = true;
                    if let (Some(field), Some(value)) = (fields.get(position), &c.value) {
                        self.check_struct_field_value(head, field, value, sym);
                    }
                }
            }
        }
        if positional && args.len() > fields.len() {
            self.error(
                codes::TYPE_MISMATCH,
                span,
                format!(
                    "literal for `{head}` has {} values but only {} fields",
                    args.len(),
                    fields.len()
                ),
            );
        }
        // A spread supplies the rest, and the positional form is bound by
        // ordinal elsewhere.
        if spread.is_some() || positional {
            return;
        }
        let missing: Vec<&str> = fields
            .iter()
            .filter(|f| !seen.contains(*f))
            .map(|f| f.as_str())
            .collect();
        if !missing.is_empty() {
            self.sink.emit(
                Diagnostic::warning(format!(
                    "literal for `{head}` leaves {} at the default value",
                    missing
                        .iter()
                        .map(|f| format!("`{f}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .with_code(codes::INCOMPLETE_STRUCT_LITERAL)
                .at(span)
                .help("give every field a value, or copy the rest with `{ ..base, .x = v }`"),
            );
        }
    }

    fn check_struct_field_value(
        &mut self,
        owner: &str,
        field: &str,
        value: &Expr,
        sym: &HashMap<String, Ty>,
    ) {
        let Some(declared) = self.field_decl_ty(owner, field) else {
            return;
        };
        let expected = self.ast_ty(&declared);

        // A name-less nested literal gets its type from this field. Recurse so
        // every nested leaf is contextualized, instead of accepting it merely
        // because an unknown expression type suppresses compatibility checks.
        let nested_struct = self
            .resolve_alias_type(&declared)
            .and_then(|ty| type_head_name(&ty).map(str::to_string))
            .is_some_and(|head| self.structs.contains_key(&head));
        if nested_struct && matches!(value, Expr::Construct { .. } | Expr::Concat { .. }) {
            self.check_struct_literal_value(&declared, value, sym);
            return;
        }

        if !matches!(expected, Ty::Error) && !self.assignable(&expected, value, sym) {
            let actual = self.type_of(value, sym);
            self.error_with_help(
                codes::TYPE_MISMATCH,
                expr_span(value),
                format!(
                    "cannot initialize `{owner}.{field}` ({}) with {}",
                    self.ty_display(&expected),
                    self.ty_display(&actual)
                ),
                format!(
                    "convert the value explicitly to {}",
                    self.ty_display(&expected)
                ),
            );
        }
    }

    /// The inclusive labels of a packed vector or data-array declaration:
    /// `unsigned[15..8]` is `(8, 15)`, while `unsigned[8]` and
    /// `unsigned[8][4]` are `(0, 7)` and `(0, 3)`. Instance arrays and
    /// parametric bounds return `None`.
    fn declared_index_bounds(&self, decl_ty: &Type) -> Option<(i64, i64)> {
        let resolved = self.resolve_alias_type(decl_ty)?;
        let Type::Indexed {
            index: Some(ix), ..
        } = &resolved
        else {
            return None;
        };
        match self.ast_ty(&resolved) {
            Ty::Array { elem, .. } if !self.is_entity_ty(&elem) => {}
            _ => return None,
        }
        match ix.as_ref() {
            Expr::Range { lo, hi, .. } => {
                let (a, b) = (signed_lit(lo)?, signed_lit(hi)?);
                Some((a.min(b), a.max(b)))
            }
            other => {
                let n = Self::const_literal(other)?;
                (n > 0).then_some((0, n - 1))
            }
        }
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
        let Some(resolved) = self.resolve_alias_type(decl_ty) else {
            return;
        };
        let t = &resolved;
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

    /// Interpret a collected method signature at a call site. Signatures are
    /// stored as source `Type`s, so a direct `Self` must be rebound to the
    /// receiver/associated owner rather than the impl-local resolver symbol.
    fn ast_ty_for_owner(&self, t: &Type, owner: &Ty) -> Ty {
        if matches!(t, Type::Path(path) if path.segments.len() == 1 && path.segments[0].text == "Self")
        {
            owner.clone()
        } else {
            self.ast_ty(t)
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
                "Self" => self
                    .current_self_ty
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| self.named_ty(p.span)),
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

    /// Resolve a `using` alias chain transitively. Cycles stop the walk and
    /// return `None` so callers can suppress follow-on diagnostics.
    fn resolve_alias_type(&self, ty: &Type) -> Option<Type> {
        let mut current = ty.clone();
        let mut seen = HashSet::new();
        loop {
            let Type::Path(p) = &current else {
                return Some(current);
            };
            if p.segments.len() != 1 {
                return Some(current);
            }
            let name = p.segments[0].text.clone();
            if !seen.insert(name.clone()) {
                return None;
            }
            let Some(alias) = self.aliases.get(&name) else {
                return Some(current);
            };
            current = alias.clone();
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
/// Append the value ranges a numeric pattern covers. Returns false when the
/// pattern's coverage is not expressible as intervals — a bit pattern's
/// don't-care bits scatter across the domain — so the caller can step aside
/// instead of reporting a hole it cannot see.
fn collect_pattern_ranges(p: &Pattern, out: &mut Vec<(i128, i128)>) -> bool {
    match p {
        Pattern::Wildcard => {
            out.push((i128::MIN, i128::MAX));
            true
        }
        Pattern::Range { lo, hi, .. } => {
            let (lo, hi) = (i128::from(*lo), i128::from(*hi));
            // `3..0` is written descending in some sources; a range covers the
            // same values either way.
            out.push((lo.min(hi), lo.max(hi)));
            true
        }
        Pattern::Or { alts, .. } => alts.iter().all(|a| collect_pattern_ranges(a, out)),
        // An enum path against a numeric scrutinee is a type error reported
        // elsewhere; a bit pattern is not an interval.
        Pattern::Path(_) | Pattern::BitPattern { .. } | Pattern::CharLit { .. } => false,
    }
}

/// Every character pattern in a pattern tree, flattening or-patterns.
fn collect_char_patterns(p: &Pattern, out: &mut Vec<(char, Span)>) {
    match p {
        Pattern::CharLit { ch, span } => out.push((*ch, *span)),
        Pattern::Or { alts, .. } => {
            for a in alts {
                collect_char_patterns(a, out);
            }
        }
        _ => {}
    }
}

/// A pattern's covered enum-variant names and whether it contains a wildcard,
/// flattening or-patterns (`A | B` covers both; `A | _` is a wildcard).
fn pattern_covers(p: &Pattern) -> (Vec<String>, bool) {
    match p {
        Pattern::Wildcard => (Vec::new(), true),
        Pattern::Path(pp) if pp.segments.len() >= 2 => (vec![pp.segments[1].text.clone()], false),
        // A char-valued enum declares its variants as character literals
        // (`enum Logic { '0', '1', … }`), so the pattern names one directly.
        Pattern::CharLit { ch, .. } => (vec![format!("'{ch}'")], false),
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

/// Requested construction type of a `read<T>(path)` expression.
fn read_call_type(expression: &Expr) -> Option<&Type> {
    let Expr::Call {
        callee, type_args, ..
    } = expression
    else {
        return None;
    };
    let Expr::Path(path) = callee.as_ref() else {
        return None;
    };
    (path.segments.len() == 1 && path.segments[0].text == "read" && type_args.len() == 1)
        .then(|| &type_args[0])
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
#[derive(Clone, Default)]
struct PortDirs {
    /// Names whose write is illegal exactly: a bare `in` port, or an `in`
    /// bus-mode leaf (`bus.ready`).
    illegal: HashSet<String>,
    /// Plain (non-bus-mode) `in` ports — writing *any* field/index of one is
    /// illegal too (it has no writable parts).
    plain_in_roots: HashSet<String>,
    /// `const`s declared in this impl. A write to one is not storage at all,
    /// and reached the emitter as "unknown signal `K`" — a message naming
    /// something the author had in fact declared.
    consts: HashSet<String>,
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

/// The type `self` has inside an impl: the backing struct for a view-applied
/// target (`impl Bus BusOut` -> `Bus`), the target itself otherwise.
fn self_ty(im: &ImplDecl) -> &Type {
    match &im.target {
        Type::View { target, .. } => target,
        other => other,
    }
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

/// Keys whose element-wise array forwarding lowering can perform. std may
/// declare a blanket `for T[]` impl only for these; the list had `and`/`or`/
/// `not` and not the rest of the logic family, so `Logic[3] xor Logic[3]` had
/// no implementation at all while `and` on the same operands worked.
fn is_liftable_array_key(key: &str) -> bool {
    matches!(
        key,
        "Resolve" | "and" | "or" | "not" | "xor" | "nand" | "nor" | "xnor"
    )
}

/// Width of a bracketed type index when it is a literal (`unsigned[8]` -> 8);
/// otherwise `0`, meaning "parametric / not yet known".
fn width_of(index: &Expr) -> u32 {
    // A declared range states a length too: `Bit[3..0]` is four elements, in
    // either direction. Without this it measured 0, so a range-declared array
    // rejected an array-literal initializer as `Bit[0]` — while the same type
    // took a *string* literal, which is sized from the literal instead.
    if let Expr::Range { lo, hi, .. } = index {
        if let (Some(lo), Some(hi)) = (signed_lit(lo), signed_lit(hi)) {
            // A range wide enough to overflow is unrepresentable anyway; 0
            // keeps it "not yet known", which is what rejects it later.
            return hi
                .checked_sub(lo)
                .and_then(i64::checked_abs)
                .and_then(|len| len.checked_add(1))
                .and_then(|len| u32::try_from(len).ok())
                .unwrap_or(0);
        }
    }
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
        Ty::Void => "no value".to_string(),
        Ty::Error => "<unknown>".to_string(),
    }
}

/// The value of an integer literal, allowing a leading unary minus.
/// Only a plain local name can be looked up; a field or nested index has no
/// entry, and is skipped rather than guessed at.
fn declared_bounds_of(
    base: &Expr,
    bounds: &std::cell::RefCell<HashMap<String, (i64, i64)>>,
) -> Option<(i64, i64)> {
    let Expr::Path(p) = base else { return None };
    let [seg] = p.segments.as_slice() else {
        return None;
    };
    bounds.borrow().get(&seg.text).copied()
}

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
        impl Operator<\"+\", unsigned, unsigned> for unsigned { fn apply(self, rhs: unsigned) -> unsigned { return self; } }\n\
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

    fn check_modules(sources: &[(&str, FileId)]) -> DiagnosticSink {
        let mut sink = DiagnosticSink::new();
        let modules: Vec<Module> = sources
            .iter()
            .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
            .collect();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        check(&modules, &resolved, &mut sink);
        sink
    }

    #[test]
    fn private_struct_members_are_module_scoped_and_pub_crosses_the_boundary() {
        let provider = "module model;\n\
            pub struct Packet { hidden: integer, pub visible: integer }\n\
            impl Packet { fn secret(self) -> integer { return self.hidden; } pub fn get(self) -> integer { return self.hidden; } }\n";
        let consumer = "module user;\n\
            fn inspect(p: Packet) -> integer { return p.hidden + p.visible + p.secret() + p.get(); }\n";
        let sink = check_modules(&[(provider, FileId(0)), (consumer, FileId(1))]);
        let private = sink
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == Some(codes::PRIVATE_MEMBER))
            .count();
        assert_eq!(
            private, 2,
            "private field and method are rejected, public ones pass"
        );
    }

    #[test]
    fn an_applied_view_is_an_explicit_structural_interface() {
        let provider = "module bus;\n\
            pub struct Stream { data: integer }\n\
            pub view Source for Stream { data out }\n\
            pub entity Producer { bus: Stream Source }\n\
            impl Producer { bus.data = 1; }\n";
        let sink = check_modules(&[(provider, FileId(0))]);
        assert!(
            sink.diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.code != Some(codes::PRIVATE_MEMBER)),
            "the view deliberately exposes its backing field"
        );
    }

    #[test]
    fn public_entity_methods_wait_for_cross_hierarchy_call_semantics() {
        let sink = check_modules(&[(
            "module m;\npub entity Device { value: integer out }\nimpl Device { pub fn read(self) -> integer { return value; } }\n",
            FileId(0),
        )]);
        assert!(sink.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == Some(codes::PRIVATE_MEMBER)
                && diagnostic.message.contains("cannot be public yet")
        }));
    }

    #[test]
    fn a_private_trait_keeps_its_implementation_methods_private() {
        let provider = "module model;\n\
            pub struct Value(integer);\n\
            trait Hidden { fn reveal(self) -> integer; }\n\
            impl Hidden for Value { fn reveal(self) -> integer { return 1; } }\n";
        let consumer =
            "module user;\nfn inspect(value: Value) -> integer { return value.reveal(); }\n";
        let sink = check_modules(&[(provider, FileId(0)), (consumer, FileId(1))]);
        assert!(sink.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == Some(codes::PRIVATE_MEMBER) && diagnostic.message.contains("reveal")
        }));
    }

    #[test]
    fn a_view_does_not_publish_backing_struct_methods() {
        let provider = "module bus;\n\
            pub struct Stream { data: integer }\n\
            pub view Source for Stream { data out }\n\
            impl Stream { fn secret(self) -> integer { return self.data; } }\n\
            pub entity Producer { bus: Stream Source }\n";
        let consumer = "module user;\nimpl Producer { let seen: integer = bus.secret(); }\n";
        let sink = check_modules(&[(provider, FileId(0)), (consumer, FileId(1))]);
        assert!(sink.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == Some(codes::PRIVATE_MEMBER) && diagnostic.message.contains("secret")
        }));
    }

    #[test]
    fn a_foreign_view_cannot_publish_private_backing_fields() {
        let provider = "module bus;\npub struct Stream { data: integer }\n";
        let consumer =
            "module user;\npub view Source for Stream { data out }\npub entity Producer { bus: Stream Source }\n";
        let sink = check_modules(&[(provider, FileId(0)), (consumer, FileId(1))]);
        assert!(sink.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == Some(codes::PRIVATE_MEMBER)
                && diagnostic.message.contains("cannot expose private field")
        }));
    }

    /// Exhaustiveness was only ever computed over enum variants, so a hole in
    /// a numeric match was silent. Warnings do not count as errors here, so
    /// these assert on the diagnostic text.
    #[test]
    fn a_numeric_match_reports_the_range_it_leaves_out() {
        let cases = [
            ("0 => 5", "`1..3`"),
            ("0 | 1 => 5", "`2..3`"),
            ("0..2 => 5", "`3`"),
            ("1..3 => 5", "`0`"),
            ("0 => 5, 2..3 => 7", "`1`"),
        ];
        for (arms, expected) in cases {
            let src = format!(
                "module m;\nentity F {{ s: unsigned[2] in, z: unsigned[8] out }}\n\
                 impl F {{ z = match s {{ {arms} }}; }}\n{VEC}"
            );
            let mut sink = DiagnosticSink::new();
            let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
            let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
            check(std::slice::from_ref(&module), &resolved, &mut sink);
            let found = sink
                .diagnostics()
                .iter()
                .any(|d| d.message.contains("non-exhaustive") && d.message.contains(expected));
            assert!(
                found,
                "`{arms}` should report {expected}: {:?}",
                sink.diagnostics()
            );
        }
    }

    /// Covering the domain by alternation, by range, or value by value is as
    /// exhaustive as a `_`, and must not warn.
    #[test]
    fn a_numeric_match_covering_its_domain_is_quiet() {
        for arms in [
            "0 | 1 => 5, 2..3 => 7",
            "0..3 => 5",
            "0 => 5, 1 => 6, 2 => 7, 3 => 8",
            "0 => 5, _ => 7",
        ] {
            let src = format!(
                "module m;\nentity F {{ s: unsigned[2] in, z: unsigned[8] out }}\n\
                 impl F {{ z = match s {{ {arms} }}; }}\n{VEC}"
            );
            let mut sink = DiagnosticSink::new();
            let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
            let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
            check(std::slice::from_ref(&module), &resolved, &mut sink);
            let warned = sink
                .diagnostics()
                .iter()
                .any(|d| d.message.contains("non-exhaustive"));
            assert!(
                !warned,
                "`{arms}` covers its domain: {:?}",
                sink.diagnostics()
            );
        }
    }

    /// A call to a function nothing declares passed every stage and failed in
    /// the backend as "unsupported call `abs` in testbench expression",
    /// blaming the emitter for a missing `using`.
    #[test]
    fn an_undeclared_call_is_reported() {
        let errors = check_src(
            "module m;\n\
             entity E { a: unsigned[8] in, y: unsigned[8] out }\n\
             impl E { y = nosuchfn(a); }\n",
        );
        assert_eq!(errors, 1, "the unknown function is reported");
    }

    /// The categories that legitimately have no `fn` declaration: a declared
    /// function, a type used as a conversion, a runtime-provided std function,
    /// a compiler primitive, and the width builtin. The corpus caught
    /// `resize` and `finish` missing from this list.
    #[test]
    fn calls_without_a_declaration_are_not_all_mistakes() {
        let errors = check_src(
            "module m;\n\
             fn twice(x: unsigned[8]) -> unsigned[8] { return x + x; }\n\
             entity E { a: unsigned[8] in, y: unsigned[8] out }\n\
             impl E { y = twice(unsigned[8](resize(a, 8))); }\n\
             #[test] entity T {}\n\
             impl T {\n\
               let v: integer = randint(0, 3);\n\
               print!(\"{}\", v);\n\
               finish();\n\
             }\n",
        );
        assert_eq!(errors, 0, "none of these is an unknown function");
    }

    /// A bus port types as its *view*, which owns no fields, so the field
    /// check found no struct behind it and returned silently — every field
    /// access through a bus went unchecked, whether the entity was
    /// instantiated or not.
    #[test]
    fn a_bus_port_checks_fields_against_its_backing_struct() {
        let errors = check_src(
            "module m;\n\
             struct S { a: Bit, b: Bit }\n\
             view V for S { a out, b in }\n\
             entity E { bus: S V, q: Bit out }\n\
             impl E { q = bus.nosuch; }\n",
        );
        assert_eq!(errors, 1, "a missing field behind a view is reported");
    }

    /// The fields the view does declare still resolve, and so do methods on
    /// the backing struct — both reach the check as field nodes.
    #[test]
    fn a_bus_port_accepts_real_fields_and_struct_methods() {
        let errors = check_src(
            "module m;\n\
             struct S { a: Bit, b: Bit }\n\
             view V for S { a out, b in }\n\
             impl S { fn helper(self) -> Bit { return self.a; } }\n\
             entity E { bus: S V, q: Bit out, r: Bit out }\n\
             impl E { q = bus.a; r = bus.helper(); }\n",
        );
        assert_eq!(errors, 0, "declared fields and methods still resolve");
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
    fn chained_integer_aliases_still_enforce_value_ranges() {
        let errors = check_src(
            "module m;\n\
             using Small = integer<0..3>;\n\
             using Alias = Small;\n\
             entity E { ok: Bit out }\n\
             impl E { let value: Alias = 4; ok = '1'; }\n",
        );
        assert_eq!(errors, 1, "a chained alias must not bypass range checks");
    }

    #[test]
    fn chained_struct_aliases_still_validate_literals() {
        let errors = check_src(
            "module m;\n\
             struct S { a: Bit }\n\
             using A = S;\n\
             using B = A;\n\
             entity E { ok: Bit out }\n\
             impl E { let x: B = { .nosuch = '1' }; ok = '1'; }\n",
        );
        assert!(errors > 0, "a chained alias must not bypass struct checks");
    }

    #[test]
    fn free_function_parameters_and_locals_keep_their_declared_types() {
        let parameter = check_src(
            "module m;\n\
             fn bad(value: Logic) { if value { return; } }\n",
        );
        assert_eq!(
            parameter, 1,
            "a free-function parameter must participate in condition checking"
        );

        let local = check_src(
            "module m;\n\
             fn bad(value: Logic) -> unsigned[8] {\n\
               let copy: Logic = value;\n\
               return copy;\n\
             }\n",
        );
        assert_eq!(
            local, 1,
            "a block-local declaration must participate in return checking"
        );

        let unknown = check_src(
            "module m;\n\
             fn bad() -> Logic { return missing; }\n",
        );
        assert_eq!(
            unknown, 1,
            "an unknown value in a free-function body must not disappear as Ty::Error"
        );
    }

    #[test]
    fn return_values_match_the_function_signature() {
        let errors = check_src(
            "module m;\n\
             fn missing() -> unsigned[8] { return; }\n\
             fn unexpected() { return 1; }\n",
        );
        assert_eq!(
            errors, 2,
            "bare and valued returns must agree with the declared signature"
        );
    }

    #[test]
    fn local_function_arguments_use_their_declared_types() {
        let errors = check_src(
            "module m;\n\
             fn take_byte(value: unsigned[8]) {}\n\
             fn take_real(value: real) {}\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let r: real = 1.5;\n\
               let i: integer = 2;\n\
               take_byte(r);\n\
               take_byte(i + r);\n\
               take_real(i);\n\
               y = '0';\n\
             }\n",
        );
        assert_eq!(
            errors, 2,
            "local and mixed-real arguments must use their actual promoted types"
        );
    }

    #[test]
    fn decimal_literals_require_real_context_or_explicit_conversion() {
        let errors = check_src(
            "module m;\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let i: integer = 1.5;\n\
               let bits: unsigned[8] = 1.5;\n\
               let r: real = 1.5;\n\
               let converted: integer = integer(r);\n\
               y = '0';\n\
             }\n",
        );
        assert_eq!(
            errors, 2,
            "a decimal literal is real and narrowing remains explicit"
        );
    }

    #[test]
    fn comparisons_and_value_branches_need_compatible_types() {
        let tb = |body: &str| format!("module m;\n#[test] entity T {{}}\nimpl T {{ {body} }}\n");
        assert_eq!(
            check_src(&tb(
                "let r: real = 1.5; let text: Char[1] = \"x\"; let bad: Bool = r == text;"
            )),
            1,
            "comparison domains"
        );
        assert_eq!(
            check_src(&tb("if if true { true } else { 1 } {}")),
            1,
            "if-expression branches"
        );
        assert_eq!(
            check_src(&tb("let bad: Bool = if true { true } else { 1 };")),
            1,
            "an enclosing assignment does not duplicate the branch error"
        );
        assert_eq!(
            check_src(&tb(
                "if match true { Bool::true => true, Bool::false => 1 } {}"
            )),
            1,
            "match-expression arms"
        );
        assert_eq!(
            check_src(
                "module m;\nenum State { Idle, Run }\n#[test] entity T {}\n\
                 impl T { let state: State = State::Idle; let bad: Bool = state < 1; }\n"
            ),
            1,
            "enum ordering still requires a matching `<=>` implementation"
        );
        assert_eq!(
            check_src(&tb(
                "let r: real = 1.5; let promoted: real = if true { 1 } else { 1.5 }; let compared: Bool = 1 < r;"
            )),
            0,
            "integer-to-real promotion stays compatible"
        );
        assert_eq!(
            check_src(&tb("let bad: integer = (if true { 1 } else { 1.5 }) + 1;")),
            1,
            "an `if` join retains real promotion when nested"
        );
        assert_eq!(
            check_src(&tb(
                "let bad: integer = (match true { Bool::true => 1, Bool::false => 1.5 }) + 1;"
            )),
            1,
            "a match join retains real promotion when nested"
        );
        assert_eq!(
            check_src(&tb(
                "let short: Char[4] = \"siox\"; let different: Bool = short != \"sioxc\";"
            )),
            0,
            "array comparison does not require assignment-compatible lengths"
        );
    }

    #[test]
    fn intrinsic_arithmetic_requires_numeric_operands() {
        let errors = check_src(
            "module m;\n\
             #[test] entity T {}\n\
             impl T {\n\
               let r: real = 1.5;\n\
               let text: Char[1] = \"x\";\n\
               let bad_add: real = r + text;\n\
               let bad_sub: Char = 'a' - 'b';\n\
               let bad_shift: integer = 1 << r;\n\
               let promoted: real = 1 + r;\n\
               let bits: unsigned[8] = 3;\n\
               let incremented: unsigned[8] = bits + 1;\n\
             }\n",
        );
        assert_eq!(
            errors, 3,
            "intrinsic arithmetic has numeric operand domains"
        );
    }

    #[test]
    fn struct_literal_fields_are_checked_in_every_value_context() {
        let errors = check_src(
            "module m;\n\
             struct Packet { data: unsigned[8] }\n\
             fn consume(packet: Packet) {}\n\
             fn make_bad(r: real) -> Packet { return { .data = r }; }\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let r: real = 1.5;\n\
               let packet: Packet;\n\
               packet = { .data = r };\n\
               consume({ .data = r });\n\
               y = '0';\n\
             }\n",
        );
        assert_eq!(
            errors, 3,
            "assignment, return, and call arguments all supply struct-field context"
        );
    }

    #[test]
    fn method_calls_check_argument_count_and_types() {
        let errors = check_src(
            "module m;\n\
             struct Device { value: unsigned[8] }\n\
             impl Device { fn take(self, value: unsigned[8]) {} }\n\
             struct DefaultDevice { value: unsigned[8] }\n\
             trait Takes {\n\
               fn take(self, value: unsigned[8]) {\n\
                 let copy: unsigned[8] = value;\n\
               }\n\
             }\n\
             impl Takes for DefaultDevice {}\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let device: Device = { .value = 0 };\n\
               let default_device: DefaultDevice = { .value = 0 };\n\
               let r: real = 1.5;\n\
               device.take(r);\n\
               device.take();\n\
               default_device.take(r);\n\
               default_device.take();\n\
               y = '0';\n\
             }\n",
        );
        assert_eq!(
            errors, 4,
            "inherent and inherited-default methods enforce parameter type and arity"
        );
    }

    #[test]
    fn associated_and_instance_method_call_forms_are_distinct() {
        let errors = check_src(
            "module m;\n\
             struct Thing { value: Bit }\n\
             struct Other { value: Bit }\n\
             trait Factory {\n\
               fn make(value: integer) -> integer { return value; }\n\
             }\n\
             impl Factory for Other {}\n\
             impl Thing {\n\
               fn static_value(value: integer) -> integer { return value; }\n\
               fn logic_value() -> Logic { return 'X'; }\n\
               fn instance_value(self) -> integer { return 1; }\n\
             }\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let thing: Thing = { .value = '0' };\n\
               let r: real = 1.5;\n\
               Thing::static_value(r);\n\
               Thing::static_value();\n\
               Thing::instance_value();\n\
               thing.static_value(1);\n\
               Other::make(r);\n\
               if Thing::logic_value() { y = '1'; } else { y = '0'; }\n\
             }\n",
        );
        assert_eq!(
            errors, 6,
            "associated calls enforce signatures, receiver form, and return typing"
        );
    }

    #[test]
    fn module_qualified_free_calls_keep_their_contracts() {
        let errors = check_src(
            "module m;\n\
             fn take(value: integer) -> Logic { return 'X'; }\n\
             fn same<T>(first: T, second: T) -> T { return first; }\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let i: integer = 1;\n\
               let r: real = 1.5;\n\
               m::take(r);\n\
               m::take();\n\
               m::same(i, r);\n\
               if m::take(i) { y = '1'; } else { y = '0'; }\n\
             }\n",
        );
        assert_eq!(
            errors, 4,
            "qualification must not discard arity, generic, argument, or return facts"
        );
    }

    #[test]
    fn value_returning_functions_return_on_every_path() {
        let missing = check_src(
            "module m;\n\
             fn empty() -> unsigned[8] {}\n\
             fn partial(flag: Bool) -> unsigned[8] {\n\
               if flag { return 1; }\n\
             }\n\
             fn numeric_gap(value: unsigned[2]) -> unsigned[8] {\n\
               match value { 0..2 => { return 1; } }\n\
             }\n\
             fn generic_gap<T>(value: T) -> T {\n\
               let copy: T = value;\n\
             }\n",
        );
        assert_eq!(
            missing, 4,
            "empty, one-sided, non-exhaustive numeric, and generic bodies fall through"
        );

        let complete = check_src(
            "module m;\n\
             enum State { A, B, C }\n\
             fn branch(flag: Bool) -> unsigned[8] {\n\
               if flag { return 1; } else { return 2; }\n\
             }\n\
             fn choose(state: State) -> unsigned[8] {\n\
               match state {\n\
                 State::A => { return 1; }\n\
                 State::B => { return 2; }\n\
                 State::C => { return 3; }\n\
               }\n\
             }\n\
             fn numeric(value: unsigned[2]) -> unsigned[8] {\n\
               match value { 0..3 => { return 4; } }\n\
             }\n",
        );
        assert_eq!(
            complete, 0,
            "two-sided branches and exhaustive matches return on every path"
        );
    }

    #[test]
    fn value_returning_methods_cannot_fall_through() {
        let errors = check_src(
            "module m;\n\
             struct S { value: unsigned[8] }\n\
             impl S { fn bad(self) -> unsigned[8] {} }\n\
             trait Defaulted {\n\
               fn partial(self, flag: Bool) -> unsigned[8] {\n\
                 if flag { return 1; }\n\
               }\n\
             }\n",
        );
        assert_eq!(
            errors, 2,
            "implementation and non-empty trait-default methods are inlined expressions"
        );
    }

    #[test]
    fn array_aliases_preserve_declared_index_bounds() {
        let errors = check_src(
            "module m;\n\
             using Window = Logic[15..8];\n\
             entity E { y: Logic out }\n\
             impl E { let data: Window; y = data[0]; }\n",
        );
        assert_eq!(
            errors, 1,
            "an alias must not turn a ranged array into an unchecked zero-based array"
        );
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
    fn assigning_to_a_const_is_rejected() {
        let has = |src: &str| diag_codes(src).iter().any(|c| c.contains("E-P018"));
        // This reached the emitter as "unknown signal `K`" — a message naming
        // something the author had in fact declared.
        assert!(has(
            "module m;\nentity E { y: Bit out, }\nimpl E { const K: Bit = '1'; K = '0'; y = K; }\n"
        ));
        assert!(!has(
            "module m;\nentity E { y: Bit out, }\nimpl E { const K: Bit = '1'; y = K; }\n"
        ));
        // A `let` of the same shape is storage and stays writable.
        assert!(!has(
            "module m;\nentity E { y: Bit out, }\nimpl E { let k: Bit; k = '0'; y = k; }\n"
        ));
    }

    #[test]
    fn struct_literal_field_names_are_checked() {
        let has = |src: &str, code: &str| diag_codes(src).iter().any(|c| c.contains(code));
        let base = "module m;\nstruct S { a: Bit, b: Bit }\nentity E { y: Bit out, }\nimpl E { let s: S = LIT; y = s.a; }\n";
        // A misspelled name was dropped whole and the literal still checked.
        assert!(has(
            &base.replace("LIT", "{ .a = '1', .zz = '0' }"),
            "E-P003"
        ));
        assert!(!has(
            &base.replace("LIT", "{ .a = '1', .b = '0' }"),
            "E-P003"
        ));
        // Omitting one is legal (it defaults) but worth saying out loud.
        assert!(has(&base.replace("LIT", "{ .a = '1' }"), "W-P016"));
        assert!(!has(
            &base.replace("LIT", "{ .a = '1', .b = '0' }"),
            "W-P016"
        ));
        // A spread supplies the rest, so nothing is left implicit.
        let with_base = "module m;\nstruct S { a: Bit, b: Bit }\nentity E { y: Bit out, }\nimpl E { let p: S = { .a = '1', .b = '0' }; let s: S = { ..p, .a = '0' }; y = s.a; }\n";
        assert!(!has(with_base, "W-P016"));
    }

    #[test]
    fn a_struct_containing_itself_is_rejected() {
        let bad = |src: &str| diag_codes(src).iter().any(|c| c.contains("E-P003"));
        // Elaboration flattens a struct into leaf signals, so each of these
        // recursed until the process aborted — with typecheck reporting
        // nothing at all. Four lines was enough.
        assert!(bad(
            "module m;\nstruct S { f: S }\nentity E { y: Bit out, }\nimpl E { let v: S; y = '0'; }\n"
        ));
        assert!(bad(
            "module m;\nstruct A { f: B }\nstruct B { f: A }\nentity E { y: Bit out, }\nimpl E { let v: A; y = '0'; }\n"
        ));
        // Through an array element, which is just as infinite.
        assert!(bad(
            "module m;\nstruct A { f: A[2] }\nentity E { y: Bit out, }\nimpl E { let v: A; y = '0'; }\n"
        ));
        // Ordinary nesting, and the same struct used twice, stay legal.
        assert!(!bad(
            "module m;\nstruct I { x: Bit }\nstruct O { a: I, b: I }\n\
             entity E { y: Bit out, }\nimpl E { let v: O; y = v.a.x; }\n"
        ));
    }

    #[test]
    fn duplicate_let_is_an_error() {
        let has = |src: &str| diag_codes(src).iter().any(|c| c.contains("E-P002"));
        // A scalar silently shadowed; a struct emitted its field locals twice
        // and failed at link with a clang error naming a mangled symbol.
        assert!(has(
            "module m;\nentity E { y: Bit out, }\nimpl E { let a: Bit; let a: Bit; y = a; }\n"
        ));
        assert!(!has(
            "module m;\nentity E { y: Bit out, }\nimpl E { let a: Bit; let b: Bit; y = a and b; }\n"
        ));
    }

    #[test]
    fn unreachable_arms_warn_in_a_match_expression() {
        // The two match forms share `MatchArm` but not the code walking it,
        // so this was statement-only.
        let base = "module m;\nenum State { Idle, Run, Done }\nentity E { y: Bit out, }\nimpl E {\n  let s: State;\n  y = match s { ARMS };\n}\n";
        assert_eq!(
            warnings(
                &base.replace("ARMS", "State::Idle => '0', State::Idle => '1', _ => '0'"),
                codes::UNREACHABLE_MATCH_ARM
            ),
            1
        );
        assert_eq!(
            warnings(
                &base.replace("ARMS", "State::Idle => '0', _ => '1'"),
                codes::UNREACHABLE_MATCH_ARM
            ),
            0
        );
    }

    #[test]
    fn data_array_index_is_bound_checked() {
        let oob = |src: &str| diag_codes(src).iter().any(|c| c.contains("E-P003"));
        // A plain count is 0-based: `v[9]` used to read `v[3]` in silence.
        let plain = "module m;\nentity E { y: Bit out, }\nimpl E {\n  let v: Logic[4];\n  y = if v[IX] == '0' { '1' } else { '0' };\n}\n";
        assert!(oob(&plain.replace("IX", "9")));
        assert!(!oob(&plain.replace("IX", "3")));
        // A declared range is indexed by that range, not by `0..len-1`.
        let ranged = "module m;\nentity E { y: Bit out, }\nimpl E {\n  let v: Logic[15..8];\n  y = if v[IX] == '0' { '1' } else { '0' };\n}\n";
        assert!(!oob(&ranged.replace("IX", "15")));
        assert!(!oob(&ranged.replace("IX", "8")));
        assert!(oob(&ranged.replace("IX", "7")));
        assert!(oob(&ranged.replace("IX", "16")));

        // Packed vector families retain the same nonzero labels. Cover both
        // an impl-local declaration and a port, whose metadata is collected
        // on different checker paths.
        let packed_local = "module m; using std::bits::unsigned; using std::logic::Logic; \
            entity E { y: Logic out } impl E { let v: unsigned[15..8]; y = v[IX]; }";
        assert!(!oob(&packed_local.replace("IX", "15")));
        assert!(!oob(&packed_local.replace("IX", "8")));
        assert!(oob(&packed_local.replace("IX", "7")));
        let packed_port = "module m; using std::bits::unsigned; using std::logic::Logic; \
            entity E { v: unsigned[15..8] in, y: Logic out } impl E { y = v[IX]; }";
        assert!(!oob(&packed_port.replace("IX", "15")));
        assert!(oob(&packed_port.replace("IX", "16")));
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
        let blanket = |op: &str| {
            format!(
                "module m;\n\
                 #[precedence = 35]\n\
                 impl<T: Operator<\"{op}\", T, T>> Operator<\"{op}\", T, T> for T[] {{\n\
                   fn apply(self, rhs: T[]) -> T[] {{ return self; }}\n\
                 }}\n"
            )
        };
        // The whole logic family lowers element-wise now, so `xor` is
        // accepted alongside `and`/`or` rather than held back.
        assert_eq!(check_src(&blanket("xor")), 0);
        assert_eq!(check_src(&blanket("nand")), 0);
        // Arithmetic has no element-wise lowering, and saying so beats
        // accepting an impl nothing would call.
        assert_eq!(check_src(&blanket("+")), 1);
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
    fn operator_overloads_match_the_declared_input_type() {
        let header = "module m;\nstruct Left { a: Bit }\nstruct Right { b: Bit }\n";
        let explicit = "impl Operator<\"+\", Right, Left> for Left {\n\
                          fn apply(self, rhs: Right) -> Left { return self; }\n\
                        }\n";
        assert_eq!(
            check_src(&format!(
                "{header}{explicit}entity E {{ a: Left in, b: Right in, }}\n\
                 impl E {{ let good: Left = a + b; }}\n"
            )),
            0,
            "the declared right-hand type selects the overload"
        );
        assert_eq!(
            check_src(&format!(
                "{header}{explicit}entity E {{ a: Left in, }}\n\
                 impl E {{ let bad: Left = a + a; }}\n"
            )),
            1,
            "an impl for another input type is not a wildcard"
        );

        let self_typed = "impl Operator<\"+\", Self, Self> for Left {\n\
                            fn apply(self, rhs: Self) -> Self { return self; }\n\
                          }\n";
        assert_eq!(
            check_src(&format!(
                "{header}{self_typed}entity E {{ a: Left in, b: Right in, }}\n\
                 impl E {{ let bad: Left = a + b; }}\n"
            )),
            1,
            "`Self` means the impl owner rather than any input type"
        );
    }

    #[test]
    fn self_in_method_signatures_is_the_impl_target() {
        let methods = "impl Left {\n\
                         fn choose(self, rhs: Self) -> Self { return rhs; }\n\
                         fn identity(value: Self) -> Self { return value; }\n\
                       }\n";
        let header = "module m;\nstruct Left { a: Bit }\nstruct Right { b: Bit }\n";
        assert_eq!(
            check_src(&format!(
                "{header}{methods}entity E {{ a: Left in, b: Left in, }}\n\
                 impl E {{ let x: Left = a.choose(b); let y: Left = Left::identity(x); }}\n"
            )),
            0,
            "`Self` parameters and returns rebind at method call sites"
        );
        assert_eq!(
            check_src(&format!(
                "{header}{methods}entity E {{ a: Left in, b: Right in, }}\n\
                 impl E {{ let bad: Left = a.choose(b); }}\n"
            )),
            1,
            "a `Self` parameter rejects a different nominal type"
        );
    }

    #[test]
    fn self_in_index_contracts_is_the_impl_target() {
        let contracts = "module m;\n\
             struct Box { value: integer }\n\
             impl Index<Self, Self> for Box {\n\
               fn index(self, index: Self) -> Self { return index; }\n\
             }\n\
             impl IndexAssign<Self, Self> for Box {\n\
               fn index_assign(self, index: Self, value: Self) {}\n\
             }\n";
        let errors = check_src(&format!(
            "{}{}",
            contracts,
            "\
             #[test] entity T {}\n\
             impl T {\n\
               let left: Box = Box { .value = 1 };\n\
               let right: Box = Box { .value = 2 };\n\
               let selected: Box = left[right];\n\
               left[right] = selected;\n\
             }\n"
        ));
        assert_eq!(
            errors, 0,
            "`Self` selects the owner for Index input/output and IndexAssign values"
        );

        assert_eq!(
            check_src(&format!(
                "{}{}",
                contracts,
                "\
                 struct Other { value: integer }\n\
                 #[test] entity T {}\n\
                 impl T {\n\
                   let container: Box = Box { .value = 1 };\n\
                   let other: Other = Other { .value = 2 };\n\
                   let invalid: Box = container[other];\n\
                 }\n",
            )),
            1,
            "`Self` does not accept an unrelated index type"
        );

        assert_eq!(
            check_src(&format!(
                "{}{}",
                contracts,
                "\
                 struct Other { value: integer }\n\
                 #[test] entity T {}\n\
                 impl T {\n\
                   let container: Box = Box { .value = 1 };\n\
                   let index: Box = Box { .value = 2 };\n\
                   let other: Other = Other { .value = 3 };\n\
                   container[index] = other;\
                 }\n",
            )),
            1,
            "`Self` does not accept an unrelated assigned value type"
        );
    }

    #[test]
    fn struct_equality_is_derived_from_three_way_comparison() {
        let base = |operator: &str| {
            format!(
                "module m;\nstruct V {{ a: Bit }}\n{operator}\n\
                 entity E {{ p: V in, q: V in, }}\n\
                 impl E {{ let equal: Bool = p == q; }}\n"
            )
        };
        assert_eq!(
            check_src(&base("")),
            1,
            "struct equality needs an operator contract"
        );
        assert_eq!(
            check_src(&base(
                "impl Operator<\"<=>\", V, Ordering> for V {\n\
                   fn apply(self, rhs: V) -> Ordering { return Ordering::Equal; }\n\
                 }"
            )),
            0,
            "one `<=>` implementation derives equality"
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
            let c = Checker::new(&mut sink, &r, std::slice::from_ref(&m));
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

    #[test]
    fn struct_literal_values_use_their_field_types() {
        let direct = check_src(
            "module m;\n\
             struct Packet { data: unsigned[8], valid: Bit }\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let logic: Logic = 'X';\n\
               let packet: Packet = { .data = logic, .valid = '1' };\n\
               y = packet.valid;\n\
             }\n",
        );
        assert_eq!(direct, 1, "a named field keeps its declared type");

        let nested = check_src(
            "module m;\n\
             struct Inner { data: unsigned[8] }\n\
             struct Outer { inner: Inner, valid: Bit }\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let logic: Logic = 'X';\n\
               let value: Outer = { .inner = { .data = logic }, .valid = '1' };\n\
               y = value.valid;\n\
             }\n",
        );
        assert_eq!(nested, 1, "nested literals are checked recursively");

        let wrong_type = check_src(
            "module m;\n\
             struct A { a: Bit }\n\
             struct B { b: Bit }\n\
             entity E { y: Bit out }\n\
             impl E { let value: A = B { .b = '1' }; y = value.a; }\n",
        );
        assert_eq!(
            wrong_type, 1,
            "an explicitly typed construction must match its destination"
        );

        let wrong_spread = check_src(
            "module m;\n\
             struct Packet { data: unsigned[8], valid: Bit }\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let logic: Logic = 'X';\n\
               let packet: Packet = { ..logic, .valid = '1' };\n\
               y = packet.valid;\n\
             }\n",
        );
        assert_eq!(wrong_spread, 1, "a spread must have the struct's type");

        let extra_positional = check_src(
            "module m;\n\
             struct Pair { a: Bit, b: Bit }\n\
             entity E { y: Bit out }\n\
             impl E { let pair: Pair = { '0', '1', '0' }; y = pair.a; }\n",
        );
        assert_eq!(
            extra_positional, 1,
            "extra positional values cannot be silently dropped"
        );

        let unknown_field_once = check_src(
            "module m;\n\
             struct Packet { data: unsigned[8] }\n\
             entity E { y: Bit out }\n\
             impl E { let packet: Packet = { .missing = 1 }; y = '1'; }\n",
        );
        assert_eq!(
            unknown_field_once, 1,
            "the struct literal checker must run exactly once"
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
    fn type_constructors_accept_at_most_one_argument() {
        let fixture = |call: &str| {
            format!(
                "module m;\nenum Phase {{ Idle, Run }}\n\
                 entity Child {{ y: Bit out }}\nimpl Child {{ y = '0'; }}\n\
                 #[test] entity T {{}}\nimpl T {{ {call}; }}\n"
            )
        };
        assert_eq!(check_src(&fixture("integer(1, 2)")), 1);
        assert_eq!(check_src(&fixture("unsigned[8](1, 2)")), 1);
        assert_eq!(check_src(&fixture("Phase(1, 2)")), 1);
        assert_eq!(check_src(&fixture("Phase()")), 0, "explicit default");
        assert_eq!(
            check_src(&fixture("Phase(Phase::Idle)")),
            0,
            "one conversion input"
        );
        assert_eq!(
            check_src(&fixture("Phase::new(1)")),
            1,
            "associated default construction is nullary"
        );
        assert_eq!(
            check_src(&fixture("Child()")),
            1,
            "an entity is instantiated with a struct literal, not called as a value"
        );
    }

    #[test]
    fn extern_call_arguments_must_match_the_declaration() {
        let src = "module m;\n\
                   extern \"C\" { fn take_int(value: integer) -> integer; }\n\
                   entity E { y: Bit out }\n\
                   impl E { let value: real = 1.5; take_int(value); y = '0'; }\n";
        assert_eq!(
            check_src(src),
            1,
            "an extern call must not reinterpret a real argument as an integer"
        );
    }

    #[test]
    fn extern_c_signatures_are_limited_to_the_implemented_scalar_abi() {
        assert_eq!(
            check_src(
                "module m;\n\
                 extern \"C\" {\n\
                   fn mixed(x: real, y: integer, bits: unsigned[64]) -> integer;\n\
                 }\n"
            ),
            0,
            "real, integer, and one-word packed values are supported"
        );
        assert_eq!(
            check_src("module m;\nextern \"C\" { fn wide(x: unsigned[65]) -> integer; }\n"),
            1,
            "a packed argument wider than the C ABI word must be rejected"
        );
        assert_eq!(
            check_src("module m;\nextern \"C\" { fn aggregate(x: unsigned[8][2]) -> integer; }\n"),
            1,
            "an array has no scalar C ABI mapping"
        );
        assert_eq!(
            check_src(
                "module m;\nstruct Pair { a: integer, b: integer }\n\
                 extern \"C\" { fn record() -> Pair; }\n"
            ),
            1,
            "a struct return has no C layout mapping"
        );
        assert_eq!(
            check_src("module m;\nextern \"C\" { fn side_effect(x: integer); }\n"),
            1,
            "void calls must not be accepted and then dropped"
        );
    }

    #[test]
    fn generic_call_arguments_obey_concrete_and_repeated_types() {
        let src = "module m;\n\
                   fn select<T>(tag: integer, first: T, second: T) -> T { return first; }\n\
                   entity E { y: Bit out }\n\
                   impl E {\n\
                     let i: integer = 1;\n\
                     let r: real = 1.5;\n\
                     select(r, i, i);\n\
                     select(i, i, r);\n\
                     y = '0';\n\
                   }\n";
        assert_eq!(
            check_src(src),
            2,
            "generic calls keep concrete parameters and one consistent inferred T"
        );
    }

    #[test]
    fn free_function_return_types_propagate_to_the_call_site() {
        let src = "module m;\n\
                   fn logic_value() -> Logic { return 'X'; }\n\
                   fn real_value() -> real { return 1.5; }\n\
                   entity E { y: Logic out }\n\
                   impl E {\n\
                     let i: integer = real_value();\n\
                     if logic_value() { y = '1'; } else { y = '0'; }\n\
                   }\n";
        assert_eq!(
            check_src(src),
            2,
            "a call has its declaration's return type in every surrounding check"
        );
    }

    #[test]
    fn procedures_cannot_be_used_as_values() {
        let src = "module m;\n\
                   fn procedure() {}\n\
                   fn consume(value: integer) {}\n\
                   struct Device { value: Bit }\n\
                   impl Device { fn procedure(self) {} }\n\
                   entity E { y: Bit out }\n\
                   impl E {\n\
                     let device: Device = { .value = '0' };\n\
                     let a: integer = procedure();\n\
                     let b: integer = device.procedure();\n\
                     consume(procedure());\n\
                     let same: Bool = procedure() == procedure();\n\
                     if procedure() { y = '1'; } else { y = '0'; }\n\
                   }\n";
        assert_eq!(
            check_src(src),
            5,
            "a procedure call is valid as a statement, never as a value"
        );
        assert_eq!(
            check_src(
                "module m;\nfn procedure() {}\nentity E { y: Bit out }\n\
                 impl E { procedure(); y = '0'; }\n"
            ),
            0,
            "a procedure call remains valid in statement position"
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

    #[test]
    fn runtime_functions_enforce_types_results_and_removed_forms() {
        let tb = |body: &str| format!("module m;\n#[test] entity T {{}}\nimpl T {{ {body} }}\n");
        assert_eq!(
            check_src(&tb("let value: integer = uniform();")),
            1,
            "uniform returns real"
        );
        assert_eq!(
            check_src(&tb("let value: integer = std::rand::uniform();")),
            1,
            "qualification keeps a runtime primitive's return contract"
        );
        assert_eq!(
            check_src(&tb("let value: integer = seed(1);")),
            1,
            "seed is a procedure"
        );
        assert_eq!(
            check_src(&tb("randint(1.5, 2);")),
            1,
            "randint needs integer bounds"
        );
        assert_eq!(check_src(&tb("seed(1.5);")), 1, "seed needs an integer");
        assert_eq!(
            check_src(&tb("read<integer>(7);")),
            1,
            "file primitives need literal paths"
        );
        assert_eq!(
            check_src(&tb(
                "let value: unsigned[16] = read<unsigned[16]>(\"word.bin\");"
            )),
            0,
            "a numeric read constructs its requested scalar type"
        );
        assert_eq!(
            check_src(&tb(
                "let value: unsigned[8] = read<unsigned[16]>(\"word.bin\");"
            )),
            1,
            "the destination must contain the requested constructed type"
        );
        assert_eq!(
            check_src(&tb("let value: integer = read_to_string(\"word.bin\");")),
            1,
            "the old split text-read primitive is removed"
        );
        assert_eq!(check_src(&tb("finish(1);")), 1, "finish is nullary");
        assert_eq!(
            check_src(&tb("clock('0', 1ns);")),
            1,
            "removed clock sugar is rejected before code generation"
        );
        assert_eq!(check_src(&tb("assert!();")), 1, "assert needs a condition");
        assert_eq!(check_src(&tb("print!();")), 1, "print needs a format");
        assert_eq!(
            check_src(&tb("print!(123);")),
            1,
            "the format must be a string literal"
        );
        assert_eq!(
            check_src(&tb("assert!(1);")),
            1,
            "assert needs a Boolean condition"
        );
        assert_eq!(
            check_src(&tb("let flag: Bool = exists(\"fixture\");")),
            0,
            "exists returns Bool"
        );
        assert_eq!(
            check_src(
                "module m;\nfn read<T>(value: T) -> T { return value; }\n\
                 #[test] entity T {}\nimpl T { let value: integer = read(1); }\n"
            ),
            0,
            "a declared function shadows a runtime primitive with the same name"
        );
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
    fn match_expression_checks_every_arm_type() {
        let bad_assignment = check_src(
            "module m;\n\
             enum Select { Number, Other }\n\
             entity E { select: Select in, logic: Logic in, y: unsigned[8] out }\n\
             impl E {\n\
               y = match select { Select::Number => 1, Select::Other => logic };\n\
             }\n",
        );
        assert_eq!(
            bad_assignment, 1,
            "a later match arm cannot bypass assignment compatibility"
        );

        let bad_return = check_src(
            "module m;\n\
             enum Select { Number, Other }\n\
             fn choose(select: Select, logic: Logic) -> unsigned[8] {\n\
               return match select { Select::Number => 1, Select::Other => logic };\n\
             }\n",
        );
        assert_eq!(
            bad_return, 1,
            "the return context applies to every match arm"
        );

        let good = check_src(
            "module m;\n\
             enum Select { One, Two }\n\
             fn choose(select: Select) -> unsigned[8] {\n\
               return match select { Select::One => 1, Select::Two => 2 };\n\
             }\n",
        );
        assert_eq!(good, 0, "compatible match arms remain valid");
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

    /// A bit-string prefix the compiler cannot evaluate is a type error, and
    /// must be one in pattern position too. The pattern parser recognized
    /// `"x" | "o"` where expression position accepts any letter, so the same
    /// prefix produced a clean diagnostic in `y = d"42";` and a raw parse
    /// error in `match s { d"42" => .. }`. `check_src` asserts the source
    /// parses, which is the property that regressed.
    #[test]
    fn an_unevaluable_prefix_is_a_diagnostic_in_pattern_position_too() {
        let pattern = check_src(
            "module m;\nentity E { s: unsigned[8] in, y: unsigned[8] out }\n\
             impl E { match s { d\"42\" => y = 1, _ => y = 0, } }\n",
        );
        assert_eq!(pattern, 1, "reported, not a parse failure");
        let expression =
            check_src("module m;\nentity E { y: unsigned[8] out }\nimpl E { y = d\"42\"; }\n");
        assert_eq!(expression, 1, "and expression position is unchanged");

        // The prefixes the table does list still work in both positions.
        let ok = check_src(
            "module m;\nentity E { s: unsigned[8] in, y: unsigned[8] out }\n\
             impl E { match s { x\"A?\" => y = 1, o\"7?\" => y = 2, _ => y = x\"0F\", } }\n",
        );
        assert_eq!(ok, 0, "`x` and `o` are in RADIX_PREFIXES");
    }

    /// A character pattern names a variant of a character-valued enum, so it
    /// is meaningless against anything else. Expression position has always
    /// rejected that (`s == '0'` on a `State` is "no numeric identity"), but
    /// when char patterns landed nothing checked pattern position — and since
    /// a character has no intrinsic value the arm compared two unrelated
    /// discriminants and *matched*, because `State::Idle` and `'0'` are both 0.
    /// A view gives each leaf of a role a direction, and writing an `in` leaf
    /// is `E-P004` when written inline. Method bodies were checked with no
    /// directions at all, so the same write hidden behind a method was
    /// accepted *and driven* — `fn bad(self) { self.ready = '1'; }` on a
    /// Source, whose `ready` is an input, defeated the whole point of the view.
    /// A declared attribute with a value type needs one. `check_attr_value`
    /// returned immediately when an attribute had no value, so a bare
    /// `#[speed]` on `attr speed: integer` passed unexamined and was carried
    /// through elaboration into `--emit tree` as `#[speed]` — an attribute a
    /// synthesis backend reads with no number in it. `Bool` is exempt: a bare
    /// flag reads as `true`, as the marker attributes do.
    #[test]
    fn a_value_typed_attribute_needs_a_value() {
        let count = |src: &str| {
            let src = format!("{src}{VEC}");
            let mut sink = DiagnosticSink::new();
            let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
            let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
            check(std::slice::from_ref(&module), &resolved, &mut sink);
            sink.diagnostics()
                .iter()
                .filter(|d| d.code == Some(codes::INVALID_ATTR_VALUE_TYPE))
                .count()
        };
        const DECL: &str = "module m;\n\
            pub attr speed: integer for Pll;\n\
            pub attr vendor: string for Pll;\n\
            pub attr flag: Bool for Pll;\n\
            entity Pll { clk: Bit in, locked: Bit out }\nimpl Pll { locked = clk; }\n";

        let body = |attr: &str| {
            count(&format!(
                "{DECL}entity E {{ c: Bit in, y: Bit out }}\n\
                 impl E {{ {attr} let p: Pll = {{ .clk = c }}; y = p.locked; }}\n"
            ))
        };

        assert_eq!(body("#[speed]"), 1, "an integer attribute needs a number");
        assert_eq!(body("#[vendor]"), 1, "a string attribute needs a string");

        // The forms that carry a value are unaffected.
        assert_eq!(body("#[speed = 42]"), 0, "a number satisfies it");
        assert_eq!(body("#[vendor = \"acme\"]"), 0, "and a string");
        assert_eq!(body("#[flag = Bool::true]"), 0, "and an explicit Bool");

        // A bare Bool flag stays legal: it reads as `true`, like `#[top]`.
        assert_eq!(body("#[flag]"), 0, "a bare Bool flag is still a flag");

        // The wrong *type* was already reported, and still is.
        assert_eq!(
            body("#[speed = \"fast\"]"),
            1,
            "a string where a number belongs"
        );
    }

    #[test]
    fn a_view_method_cannot_drive_an_input_leaf() {
        let count = |src: &str| {
            let src = format!("{src}{VEC}");
            let mut sink = DiagnosticSink::new();
            let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
            let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
            check(std::slice::from_ref(&module), &resolved, &mut sink);
            sink.diagnostics()
                .iter()
                .filter(|d| d.code == Some(codes::WRITE_TO_INPUT_PORT))
                .count()
        };
        const BUS: &str = "module m;\n\
            struct Stream { valid: Bit, data: unsigned[8], ready: Bit }\n\
            view StreamSource for Stream { valid out, data out, ready in }\n\
            view StreamSink for Stream { valid in, data in, ready out }\n";

        // `ready` is an input for the Source role.
        assert_eq!(
            count(&format!(
                "{BUS}impl Stream StreamSource {{ fn bad(self) {{ self.ready = '1'; }} }}\n"
            )),
            1,
            "a Source method driving `ready`"
        );
        // The Sink role has the opposite polarity, so `valid` is its input.
        assert_eq!(
            count(&format!(
                "{BUS}impl Stream StreamSink {{ fn bad(self) {{ self.valid = '1'; }} }}\n"
            )),
            1,
            "a Sink method driving `valid`"
        );
        // Nested inside control flow, where the walk has to carry the context.
        assert_eq!(
            count(&format!(
                "{BUS}impl Stream StreamSource {{ \
                 fn bad(self, e: Bit) {{ if e == '1' {{ self.ready = '1'; }} }} }}\n"
            )),
            1,
            "and one hidden inside an `if`"
        );

        // The outputs of each role stay writable — this is what methods are for.
        assert_eq!(
            count(&format!(
                "{BUS}impl Stream StreamSource {{ \
                 fn send(self, v: unsigned[8]) {{ self.valid = '1'; self.data = v; }} }}\n"
            )),
            0,
            "a Source may drive `valid` and `data`"
        );
        assert_eq!(
            count(&format!(
                "{BUS}impl Stream StreamSink {{ fn accept(self) {{ self.ready = '1'; }} }}\n"
            )),
            0,
            "and a Sink may drive `ready`"
        );

        // A plain struct carries no directions, so every field is writable.
        assert_eq!(
            count(
                "module m;\nstruct Pair { a: unsigned[8], b: unsigned[8] }\n\
                 impl Pair { fn set(self) { self.a = 1; self.b = 2; } }\n"
            ),
            0,
            "a plain struct method writes any field"
        );

        // The same hole, one target over: a function in an *entity* impl
        // inlines into that entity's body, so the entity's own `in` ports and
        // `const`s are off limits there too. Both were accepted inside a
        // function and rejected written inline.
        const ENT: &str = "module m;\n\
            entity E { a: unsigned[8] in, y: unsigned[8] out }\n";
        assert_eq!(
            count(&format!(
                "{ENT}impl E {{ fn writes() {{ a = 99; }} y = a; }}\n"
            )),
            1,
            "an entity function driving an `in` port"
        );
        assert_eq!(
            count(&format!(
                "{ENT}impl E {{ fn deep(e: Bit) {{ if e == '1' {{ a = 99; }} }} y = a; }}\n"
            )),
            1,
            "and one nested in control flow"
        );
        // Outputs and locals stay writable, or a helper could not do anything.
        assert_eq!(
            count(&format!(
                "{ENT}impl E {{ fn plus(n: unsigned[8]) -> unsigned[8] {{ return n + 1; }}\n\
                 y = E::plus(a); }}\n"
            )),
            0,
            "a helper that reads its argument and returns"
        );
    }

    /// The `const` half of the same hole: a `const` declared in an impl is
    /// fixed at elaboration, and assigning to it is `E-P018` written inline.
    /// Inside a function of that impl it was accepted.
    #[test]
    fn an_impl_function_cannot_assign_to_a_const() {
        let count = |src: &str| {
            let src = format!("{src}{VEC}");
            let mut sink = DiagnosticSink::new();
            let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
            let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
            check(std::slice::from_ref(&module), &resolved, &mut sink);
            sink.diagnostics()
                .iter()
                .filter(|d| d.code == Some(codes::INVALID_ASSIGN_TARGET))
                .count()
        };
        assert_eq!(
            count(
                "module m;\nentity E { y: unsigned[8] out }\n\
                 impl E { const K: unsigned[8] = 5; fn bad() { K = 1; } y = K; }\n"
            ),
            1,
            "a function assigning to its impl's `const`"
        );
        assert_eq!(
            count(
                "module m;\nentity E { y: unsigned[8] out }\n\
                 impl E { const K: unsigned[8] = 5; fn ok() -> unsigned[8] { return K; } \
                 y = E::ok(); }\n"
            ),
            0,
            "reading it is fine"
        );
        // A parameter shadows the impl-level name it repeats, so this writes
        // its own argument and not the `const`. Inheriting the restrictions
        // without this exclusion rejected it.
        assert_eq!(
            count(
                "module m;\nentity E { y: unsigned[8] out }\n\
                 impl E { const K: unsigned[8] = 5; \
                 fn shadow(K: unsigned[8]) -> unsigned[8] { K = K + 1; return K; } \
                 y = E::shadow(1); }\n"
            ),
            0,
            "a parameter named `K` is not the `const`"
        );
    }

    /// The ranged-integer bounds shared the same hole: `y = 20` on an
    /// `integer<0..7>` is a compile-time error written inline, and inside a
    /// function of the same impl it was not checked at all, because the body
    /// walk was handed an empty bounds map along with its empty directions.
    #[test]
    fn an_impl_function_checks_ranged_assignments() {
        let count = |src: &str| {
            let src = format!("{src}{VEC}");
            let mut sink = DiagnosticSink::new();
            let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
            let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
            check(std::slice::from_ref(&module), &resolved, &mut sink);
            sink.diagnostics()
                .iter()
                .filter(|d| d.message.contains("outside the range"))
                .count()
        };
        assert_eq!(
            count(
                "module m;\nentity E { y: integer<0..7> out }\n\
                 impl E { fn drive() { y = 20; } }\n"
            ),
            1,
            "an out-of-range constant inside a function"
        );
        assert_eq!(
            count(
                "module m;\nentity E { y: integer<0..7> out }\n\
                 impl E { fn drive(e: Bit) { if e == '1' { y = 20; } } }\n"
            ),
            1,
            "and one nested in control flow"
        );
        assert_eq!(
            count(
                "module m;\nentity E { y: integer<0..7> out }\n\
                 impl E { fn drive() { y = 5; } }\n"
            ),
            0,
            "a value inside the range is fine"
        );
        // Again the shadowing rule: the parameter has its own type.
        assert_eq!(
            count(
                "module m;\nentity E { y: integer<0..7> out }\n\
                 impl E { fn wide(y: unsigned[8]) -> unsigned[8] { y = 20; return y; } }\n"
            ),
            0,
            "a parameter named `y` is not the ranged port"
        );
    }

    /// The last of the three things a function body was denied: types. It was
    /// walked with an empty symbol table, so the strict assignment-width rule
    /// had nothing to compare and never fired inside a method. A method's own
    /// parameters are declared right there, and typing them is enough for the
    /// rule to work on them.
    #[test]
    fn an_impl_function_checks_widths_of_its_own_parameters() {
        let count = |src: &str| {
            let src = format!("{src}{VEC}");
            let mut sink = DiagnosticSink::new();
            let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
            let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
            check(std::slice::from_ref(&module), &resolved, &mut sink);
            sink.diagnostics()
                .iter()
                .filter(|d| d.message.contains("without an explicit conversion"))
                .count()
        };
        assert_eq!(
            count(
                "module m;\nstruct S { }\n\
                 impl S { fn f(self, wide: unsigned[16], narrow: unsigned[8]) \
                 { narrow = wide; } }\n"
            ),
            1,
            "a 16-bit parameter assigned into an 8-bit one"
        );
        assert_eq!(
            count(
                "module m;\nstruct S { }\n\
                 impl S { fn f(self, a: unsigned[8], b: unsigned[8]) { b = a; } }\n"
            ),
            0,
            "matching widths are fine"
        );
        assert_eq!(
            count(
                "module m;\nstruct S { }\n\
                 impl S { fn f(self, wide: unsigned[16], narrow: unsigned[8]) \
                 { narrow = unsigned[8](wide); } }\n"
            ),
            0,
            "an explicit conversion is the way through"
        );

        // The target that matters: a struct *field*. `type_of` returned
        // `Ty::Error` for any data field access, which suppresses every check
        // that consults it, so this truncated 0x1234 into eight bits in
        // silence — through a view method, which does inline.
        const BUS: &str = "module m;\nstruct Inner { v: unsigned[8] }\n\
            struct Bus { data: unsigned[8], flag: Bit, inner: Inner }\n";
        assert_eq!(
            count(&format!(
                "{BUS}impl Bus {{ fn load(self, wide: unsigned[16]) {{ self.data = wide; }} }}\n"
            )),
            1,
            "a wide parameter into a narrow field"
        );
        assert_eq!(
            count(&format!(
                "{BUS}view BusOut for Bus {{ data out, flag out, inner out }}\n\
                 impl Bus BusOut {{ fn load(self, wide: unsigned[16]) {{ self.data = wide; }} }}\n"
            )),
            1,
            "and the same through a view, where `self` is the backing struct"
        );
        assert_eq!(
            count(&format!(
                "{BUS}impl Bus {{ fn deep(self, wide: unsigned[16]) {{ self.inner.v = wide; }} }}\n"
            )),
            1,
            "a nested field types too"
        );

        // Legitimate field writes must stay legal, or every method breaks.
        assert_eq!(
            count(&format!(
                "{BUS}impl Bus {{ fn ok(self, v: unsigned[8]) {{ self.data = v; }}\n\
                 fn conv(self, w: unsigned[16]) {{ self.data = unsigned[8](w); }}\n\
                 fn nest(self, v: unsigned[8]) {{ self.inner.v = v; }}\n\
                 fn lit(self) {{ self.data = 200; self.flag = '1'; }} }}\n"
            )),
            0,
            "matching widths, conversions, nesting and literals are all fine"
        );
    }

    #[test]
    fn a_character_pattern_needs_a_character_valued_enum() {
        let count = |src: &str| {
            let src = format!("{src}{VEC}");
            let mut sink = DiagnosticSink::new();
            let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
            let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
            check(std::slice::from_ref(&module), &resolved, &mut sink);
            sink.diagnostics()
                .iter()
                .filter(|d| {
                    d.code == Some(codes::INVALID_PATTERN) || d.code == Some(codes::TYPE_MISMATCH)
                })
                .count()
        };
        const STATE: &str = "module m;\nenum State { Idle, Run }\n";

        assert_eq!(
            count(&format!(
                "{STATE}entity E {{ s: State in, a: unsigned[8] out }}\n\
                 impl E {{ match s {{ '0' => a = 1, _ => a = 2, }} }}\n"
            )),
            1,
            "a character against a name-valued enum"
        );
        assert_eq!(
            count(
                "module m;\nentity E { n: unsigned[4] in, a: unsigned[8] out }\n\
                 impl E { match n { '0' => a = 1, _ => a = 2, } }\n"
            ),
            1,
            "a character against a numeric scrutinee"
        );
        assert_eq!(
            count(
                "module m;\nentity E { l: Logic in, a: unsigned[8] out }\n\
                 impl E { match l { 'Q' => a = 1, _ => a = 2, } }\n"
            ),
            1,
            "a character that is not one of the enum's variants"
        );
        // Inside an alternation, where the pattern walk has to recurse.
        assert_eq!(
            count(&format!(
                "{STATE}entity E {{ s: State in, a: unsigned[8] out }}\n\
                 impl E {{ match s {{ State::Idle | '0' => a = 1, _ => a = 2, }} }}\n"
            )),
            1,
            "and one hidden in an or-pattern"
        );

        // The spelling that is meant to work.
        assert_eq!(
            count(
                "module m;\nentity E { l: Logic in, a: unsigned[8] out }\n\
                 impl E { match l { '0' | '1' => a = 1, 'Z' => a = 2, _ => a = 3, } }\n"
            ),
            0,
            "characters against `Logic`, alternation included"
        );
    }

    #[test]
    fn match_patterns_must_belong_to_the_scrutinee_domain() {
        let enums = "module m;\nenum Left { Zero, One }\nenum Right { Zero, One }\n";
        assert_eq!(
            check_src(&format!(
                "{enums}#[test] entity T {{}}\nimpl T {{\n\
                 let value: Left = Left::Zero;\n\
                 match value {{ Right::Zero => {{}}, _ => {{}}, }}\n\
                 }}\n"
            )),
            1,
            "equal discriminants from different enums are not interchangeable"
        );
        assert_eq!(
            check_src(&format!(
                "{enums}#[test] entity T {{}}\nimpl T {{\n\
                 let value: unsigned[2] = 0;\n\
                 match value {{ Left::Zero => {{}}, _ => {{}}, }}\n\
                 }}\n"
            )),
            1,
            "an enum variant cannot pattern-match a numeric value"
        );
        assert_eq!(
            check_src(&format!(
                "{enums}#[test] entity T {{}}\nimpl T {{\n\
                 let value: Left = Left::Zero;\n\
                 match value {{ 0 => {{}}, _ => {{}}, }}\n\
                 }}\n"
            )),
            1,
            "an integer pattern cannot inspect an enum discriminant"
        );
        assert_eq!(
            check_src(&format!(
                "{enums}#[test] entity T {{}}\nimpl T {{\n\
                 let value: Left = Left::Zero;\n\
                 match value {{ \"--\" => {{}}, _ => {{}}, }}\n\
                 }}\n"
            )),
            1,
            "a bit mask cannot inspect an enum discriminant"
        );
        assert_eq!(
            check_src(&format!(
                "{enums}#[test] entity T {{}}\nimpl T {{\n\
                 let value: Left = Left::Zero;\n\
                 match value {{ Left::Zero | Left::One => {{}}, }}\n\
                 }}\n"
            )),
            0,
            "matching alternatives from the scrutinee enum remains valid"
        );
    }

    /// An entity may be instantiated at the root layer of another entity's
    /// body, or inside a generate `for`/`if` — nowhere else. A `match` arm and
    /// a function body used to be accepted and then quietly dropped by
    /// elaboration, so the design ran as though the instance had never been
    /// written; a function failed later still, with "contains an Unknown".
    #[test]
    fn an_entity_cannot_be_instantiated_outside_a_generate() {
        let count = |src: &str| {
            let src = format!("{src}{VEC}");
            let mut sink = DiagnosticSink::new();
            let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
            let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
            check(std::slice::from_ref(&module), &resolved, &mut sink);
            sink.diagnostics()
                .iter()
                .filter(|d| d.code == Some(codes::INSTANCE_PLACEMENT))
                .count()
        };
        const CELL: &str = "module m;\nentity Cell { i: unsigned[8] in, o: unsigned[8] out }\n\
                            impl Cell { o = i; }\n";

        let in_match = count(&format!(
            "{CELL}entity E {{ s: unsigned[2] in, y: unsigned[8] out }}\n\
             impl E {{ y = 0; match s {{ 0 => {{ let c: Cell = {{ .i = 5 }}; }} _ => {{ y = 1; }} }} }}\n"
        ));
        assert_eq!(in_match, 1, "a `match` arm");

        let in_fn = count(&format!(
            "{CELL}fn helper(x: unsigned[8]) -> unsigned[8] {{ let c: Cell = {{ .i = x }}; return 1; }}\n\
             entity E {{ y: unsigned[8] out }}\nimpl E {{ y = helper(5); }}\n"
        ));
        assert_eq!(in_fn, 1, "a function body");

        // The legal placements: root layer, and a generate `for`/`if`. The
        // behavioural-`if` case is elaboration's to report, not this stage's.
        let root = count(&format!(
            "{CELL}entity E {{ y: unsigned[8] out }}\n\
             impl E {{ let c: Cell = {{ .i = 5 }}; y = c.o; }}\n"
        ));
        assert_eq!(root, 0, "the root layer of an entity body");

        let generate = count(&format!(
            "{CELL}entity E {{ y: unsigned[8] out }}\n\
             impl E {{ let s: Cell[2]; for i in 0..1 {{ s[i] = Cell {{ .i = i }}; }}\n\
             if 1 == 1 {{ let g: Cell = {{ .i = 3 }}; y = g.o; }} else {{ y = 0; }} }}\n"
        ));
        assert_eq!(generate, 0, "a generate `for` and a generate `if`");

        // A generic parameter names data, never an instance, even when an
        // entity happens to share its name. Checking the head against the
        // entity table alone rejected `let held: T` in a generic method.
        let shadowed_method = count(
            "module m;\nentity T { i: unsigned[8] in, o: unsigned[8] out }\nimpl T { o = i; }\n\
             struct Box<T> { v: T }\n\
             impl<T> Box<T> { fn get(self) -> T { let held: T = self.v; return held; } }\n",
        );
        assert_eq!(shadowed_method, 0, "`T` here is the impl's parameter");

        let shadowed_match = count(
            "module m;\nentity T { i: unsigned[8] in, o: unsigned[8] out }\nimpl T { o = i; }\n\
             entity Sel<T> { s: unsigned[2] in, d: T in, y: T out }\n\
             impl<T> Sel<T> { y = d; match s { 0 => { let a: T; y = d; } _ => { y = d; } } }\n",
        );
        assert_eq!(
            shadowed_match, 0,
            "and in a `match` arm of a generic entity"
        );

        // The entity is still an entity outside that binder.
        let unshadowed = count(
            "module m;\nentity T { i: unsigned[8] in, o: unsigned[8] out }\nimpl T { o = i; }\n\
             entity E { s: unsigned[2] in, y: unsigned[8] out }\n\
             impl E { y = 0; match s { 0 => { let a: T = { .i = 1 }; } _ => { y = 1; } } }\n",
        );
        assert_eq!(unshadowed, 1, "no binder in scope, so `T` is the entity");
    }

    /// A statement expression that is not a call was dropped by lowering's
    /// catch-all without a word, so a misspelled name compiled clean and a
    /// stray `continue;` — a Rust habit siox does not have — looked accepted
    /// while the `for` body ran every iteration anyway.
    #[test]
    fn a_statement_with_no_effect_is_reported() {
        // Count E-P019 alone: `q + 1;` also trips the width checker, and this
        // test is about the statement being reported at all, not about what
        // else the discarded expression happens to be wrong about.
        let no_effect = |src: &str| {
            let src = format!("{src}{VEC}");
            let mut sink = DiagnosticSink::new();
            let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
            let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
            check(std::slice::from_ref(&module), &resolved, &mut sink);
            sink.diagnostics()
                .iter()
                .filter(|d| d.code == Some(codes::NO_EFFECT_STATEMENT))
                .count()
        };
        let body = |stmts: &str| {
            no_effect(&format!(
                "module m;\nentity E {{ y: unsigned[8] out }}\n\
                 impl E {{ let q: unsigned[8] = 3; {stmts} y = q; }}\n"
            ))
        };
        assert_eq!(body("zzz_undefined_name;"), 1, "a misspelled bare name");
        assert_eq!(body("q;"), 1, "a name that does resolve is still dead");
        assert_eq!(body("q + 1;"), 1, "a computed value that goes nowhere");
        assert_eq!(
            body("for i in 0..2 { continue; }"),
            1,
            "`continue` has no meaning in an unrolled loop"
        );
        assert_eq!(body("if q == 3 { zzz; }"), 1, "nested in a block");

        // A call is the one statement shape that does something.
        assert_eq!(body(""), 0, "the same body without the dead statement");
        let call = no_effect(
            "module m;\nstruct S { v: unsigned[8] }\n\
             impl S { fn bump(self) { self.v = self.v + 1; } }\n\
             entity E { y: unsigned[8] out }\n\
             impl E { let s: S = { .v = 3 }; s.bump(); y = s.v; }\n",
        );
        assert_eq!(call, 0, "a method call as a statement");
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

    #[test]
    fn intrinsic_indices_require_numeric_index_values() {
        let tb = |body: &str| {
            format!(
                "module m;\n#[test] entity T {{}}\nimpl T {{\n\
                 let bits: unsigned[8] = 0;\n{body}\n}}\n"
            )
        };
        assert_eq!(
            check_src(&tb("let r: real = 1.5; let value: Logic = bits[r];")),
            1,
            "a real is not a bit index"
        );
        assert_eq!(
            check_src(&tb("let flag: Bool = true; let value: Logic = bits[flag];")),
            1,
            "an enum value is not a bit index"
        );
        assert_eq!(
            check_src(&tb("let r: real = 1.5; bits[r] = '1';")),
            1,
            "indexed writes enforce the same index domain"
        );
        assert_eq!(
            check_src(&tb("let r: real = 1.5; bits[0..r] = 0;")),
            1,
            "slice bounds are numeric index values too"
        );
        assert_eq!(
            check_src(&tb(
                "let index: unsigned[3] = 2; let value: Logic = bits[index];"
            )),
            0,
            "packed numeric values remain valid dynamic indices"
        );
    }

    #[test]
    fn for_loops_require_ranges_or_iterable_arrays() {
        let tb = |range: &str| {
            format!(
                "module m;\n#[test] entity T {{}}\nimpl T {{ for item in {range} {{ print!(\"{{}}\", item); }} }}\n"
            )
        };
        assert_eq!(check_src(&tb("5")), 1, "an integer is not iterable");
        assert_eq!(check_src(&tb("true")), 1, "an enum is not iterable");
        assert_eq!(
            check_src(&tb("1.5..2.5")),
            2,
            "both range endpoints must be integer-like"
        );
        assert_eq!(check_src(&tb("0..3")), 0, "integer ranges remain valid");
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

        // Testbench stimulus settles after each connected-signal write. The
        // first half of a clock pulse is observable even with no `await`
        // between these statements, so neither it nor an unrolled repetition
        // belongs to the driver-context lint.
        let stimulus = "module m;\n#[test] entity T {}\nimpl T {\n  let clk: Bit = '0';\n  clk = '1';\n  clk = '0';\n  for i in 0..1 { clk = '1'; clk = '0'; }\n}\n";
        assert_eq!(warnings(stimulus, codes::DEAD_ASSIGNMENT), 0);
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
