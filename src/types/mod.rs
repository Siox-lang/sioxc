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
use crate::resolve::{is_compiler_trait, DefId, DefKind, Resolved};
use crate::syntax::ast::*;
use crate::syntax::Module;

mod assignments;
mod ast_types;
mod calls;
mod collect;
mod expressions;
mod helpers;
mod impls;
mod indexing;
mod inference;
mod items;
mod keys;
mod literals;
mod members;
mod operators;
mod patterns;
mod statements;

use helpers::*;

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
        /// The element type.
        elem: Box<Ty>,
        /// Element count. Widths are concrete by this stage.
        len: u32,
        /// The nominal family for a library newtype such as `unsigned`, which
        /// drives method and operator dispatch. `None` for a plain array.
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
    /// Checked type of each expression, keyed by its span.
    expr_types: HashMap<Span, Ty>,
}

impl Typed {
    /// Checked type of the expression covering this exact source span.
    pub fn expr_type(&self, span: Span) -> Option<&Ty> {
        self.expr_types.get(&span)
    }

    /// Every expression type, keyed by the expression's span.
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
    /// Port name as declared.
    name: String,
    /// Resolved port type.
    ty: Ty,
    /// Declared direction, or `None` when the port is view-typed and its
    /// directions come from `view` instead.
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
    /// `true` / `false`.
    Bool,
    /// A string literal.
    Str,
    /// An integer literal.
    Integer,
    /// Anything else, which the checker accepts without constraining.
    Other,
}

/// `(operator trait, implementing type)` to its overloads, each
/// `(input type, output type)`.
type OperatorSignatures = HashMap<(String, String), Vec<(Option<String>, Option<String>)>>;
/// A generic function's declared parameters and their written types.
type GenericFnSignature = (Vec<Param>, Vec<Option<Type>>);
/// A method's declared non-`self` parameter types, `None` where the
/// type was left to inference.
type MethodParams = Vec<Option<Type>>;
/// A trait's defaulted method: name, return type, parameters, and
/// whether it takes `self`.
type TraitDefaultSignature = (String, Option<Type>, MethodParams, bool);
/// A method a type inherits from a trait: owner, name, return type,
/// parameters, and whether it takes `self`.
type InheritedMethodSignature = (String, String, Option<Type>, MethodParams, bool);
/// The environment one impl body is checked in: port directions,
/// named types in scope, and declared numeric ranges.
type ImplEnvironment = (PortDirs, HashMap<String, Ty>, HashMap<String, (i64, i64)>);

/// Where a member may be named from, resolved once at collection so
/// every later access checks against the same answer.
#[derive(Clone)]
struct MemberVisibility {
    /// Whether the member is declared `pub`.
    is_pub: bool,
    /// Inherent members and representation fields belong to the owning type.
    /// Trait methods instead inherit the trait declaration's module boundary.
    type_private: bool,
    /// Type or trait that declares the member.
    owner: String,
    /// Module the visibility boundary is drawn around.
    module: String,
    /// Declaration span, so a violation can point at it.
    span: Span,
}

/// Type and kind checker for one set of resolved modules.
///
/// Most fields are registries collected in a first pass over every
/// declaration, so the checking walk can answer questions about items
/// it has not reached yet. The `Cell`/`RefCell` fields are walk state
/// rather than registries: they track where in the tree the checker
/// currently is, and are borrowed from `&self` methods.
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
    /// Where type diagnostics are emitted.
    sink: &'a mut DiagnosticSink,
    /// Name resolution results, for definition lookup.
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
    /// Custom operator symbol to its declared precedence and declaration
    /// span. The span makes a conflicting redeclaration reportable at both
    /// sites.
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
    /// Nominal newtypes whose representation is an array (`struct F(T[])`).
    array_families: HashSet<String>,
    /// Nominal array family -> element type, following derived array bases.
    array_elements: HashMap<String, String>,
    /// Trait/operator keys implemented generically for an unconstrained array
    /// target (`impl<T: Tr> Tr for T[]`). A nominal array family may forward
    /// one of these only when its element type implements the same key.
    blanket_array_impls: HashMap<String, String>,
    /// Generic module fns: definition -> (type params with bounds, value params).
    /// Bounds are checked at each call (spec: generic bounds).
    generic_fns: HashMap<DefId, GenericFnSignature>,
    /// Declared free function -> its parameter count, for call-arity checking.
    /// Covers module `fn`s and `extern "C"` declarations; runtime-provided std
    /// functions (rand/fs) have no declaration and are not listed.
    fn_arity: HashMap<DefId, usize>,
    /// Free-function definition -> its declared parameter types. Arguments were
    /// checked for count but never for type, so a value of the wrong type was
    /// reinterpreted bit-for-bit at the call.
    fn_param_types: HashMap<DefId, Vec<Option<Type>>>,
    /// Free-function definition -> its declared return type. A call expression used
    /// to type as `Error` even for a known declaration, suppressing checks in
    /// assignments, conditions, arguments, and enclosing expressions.
    fn_return_types: HashMap<DefId, Option<Type>>,
    /// Module constant definition -> its declared value type. Constant paths
    /// are values, not nominal types; retaining the declaration identity lets
    /// equal leaves in different modules keep distinct contracts.
    const_types: HashMap<DefId, Type>,
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
    /// `(type head, method name)` to where the method may be named from.
    method_visibility: HashMap<(String, String), MemberVisibility>,
    /// Entity implementation state is never part of the entity's structural
    /// interface. Keep its declaration site so `instance.hidden` is diagnosed
    /// as a privacy violation instead of falling through as an unchecked
    /// field access.
    private_entity_members: HashMap<(String, String), Span>,
    /// Named view -> per-field directions.
    view_dirs: HashMap<String, HashMap<String, Direction>>,
    /// Persistent Stage-4 facts keyed by the AST expression's stable span.
    expr_types: std::cell::RefCell<HashMap<Span, Ty>>,
    /// Struct literals are reached both from their contextual consumer and
    /// from the ordinary expression walk. Keep structural/privacy diagnostics
    /// single-shot while still checking the consumer's expected type on every
    /// contextual visit.
    checked_struct_literals: std::cell::RefCell<HashSet<Span>>,
    /// Concrete meaning of the `Self` type while checking one impl. The
    /// resolver correctly binds `Self` locally, but its synthetic definition
    /// is not the impl target and must not become a distinct nominal type.
    current_self_ty: std::cell::RefCell<Option<Ty>>,
    /// Semantic impl target while checking an implementation. Applied views
    /// have the backing struct as `Self` but remain a distinct method owner,
    /// so privacy checks need both identities.
    current_impl_owner: std::cell::RefCell<Option<String>>,
    /// Source file to its declared module path. Privacy belongs to the
    /// module, never to the file that happened to hold the declaration.
    file_modules: HashMap<crate::diag::FileId, String>,
    /// Every entity name, so a type that is not an entity is not mistaken
    /// for an instantiation.
    entity_names: HashSet<String>,
}

#[cfg(test)]
mod tests;
