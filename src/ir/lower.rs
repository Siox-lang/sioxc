//! Siox frontend lowering into the language-neutral digital IR.
//!
//! This module is the only large IR component allowed to depend on Siox AST
//! details. The surrounding modules define and transform backend-facing data.

use super::*;

mod block_locals;
mod body;
mod calls;
mod collect;
mod control;
mod diagnostics;
mod expressions;
mod initializers;
mod layout;
mod metavalue;
mod operators;
mod resolution;
mod values;
mod writes;

/// A design ready to simulate: signals, combinational drivers, and event blocks.
/// `(operator, left type, right type, span)` for an unmatched operator.
type BadOperator = (String, String, Option<String>, crate::diag::Span);

/// Lower the elaborated design into simulation IR. Relative file-read paths
/// resolve against the current working directory (see [`lower_in`] to set a
/// source-relative base directory).
pub fn lower(
    modules: &[Module],
    resolved: &Resolved,
    hier: &Hierarchy,
    sink: &mut DiagnosticSink,
) -> Design {
    lower_in(modules, resolved, hier, sink, std::path::Path::new(""))
}

/// Lower with `base_dir` as the root that relative `read<T>`
/// paths resolve against (the design's source directory), so a program that
/// bakes in a data file works regardless of the working directory.
pub fn lower_in(
    modules: &[Module],
    resolved: &Resolved,
    hier: &Hierarchy,
    sink: &mut DiagnosticSink,
    base_dir: &std::path::Path,
) -> Design {
    let mut l = Lowering::new(sink, resolved);
    l.expr_types = hier.expr_types.clone();
    l.base_dir = base_dir.to_path_buf();
    l.out.base_dir = base_dir.to_path_buf();
    // Enum discriminants first: `collect` folds constants, and a constant may
    // *be* a variant (`const M: Mode = Mode::Fast;`). Populating the map
    // afterwards left the fold with nothing to look the variant up in, so the
    // constant never entered the tables and every read of it reported the
    // name as unknown.
    l.enum_variants = enum_discriminants(modules, &l.free_fns);
    l.enum_first_disc = enum_first_discriminants(modules, &l.free_fns);
    l.collect(modules);
    {
        let enums = enum_index(modules, &l.free_fns);
        for (name, e) in &enums {
            if let Some(b) = enum_base_name(e, &enums, &l.free_fns) {
                l.enum_bases.insert(name.clone(), b);
            }
        }
        l.out.enum_bases = l.enum_bases.clone();
    }
    l.logic_encodings = l.compute_logic_encodings();
    l.out.logic_encodings = l.logic_encodings.clone();
    l.new_defaults = l.enum_first_disc.clone();
    l.new_defaults.extend(l.compute_new_defaults());
    l.out.new_defaults = l.new_defaults.clone();
    // Reverse each enum's variant map (name -> disc) into disc -> symbol, so
    // consumers can render stored discriminants symbolically.
    l.out.enum_syms = l
        .enum_variants
        .iter()
        .map(|(ty, vars)| {
            (
                ty.clone(),
                vars.iter().map(|(sym, &d)| (d, sym.clone())).collect(),
            )
        })
        .collect();
    l.enum_reprs = enum_reprs(modules, &l.free_fns);
    l.array_families = array_families(modules, &l.free_fns);
    // Record what each family's elements are, so a consumer that sees only
    // the finished `Design` (the testbench emitter) can type a bit of a
    // packed vector the same way lowering does.
    for family in &l.array_families {
        if let Some(element) = l.array_element_enum(family) {
            l.out
                .array_element_of_family
                .insert(family.clone(), element);
        }
    }
    // The entity types that appear in the elaborated hierarchy, in first-seen
    // order, deduplicated. Each entity's parameters are taken from its first
    // instance, so `unsigned[W]` lowers with the instance's concrete `W`.
    let mut seen = Vec::new();
    for inst in &hier.instances {
        if !seen.contains(&inst.entity_id) {
            seen.push(inst.entity_id);
            l.entity_params.entry(inst.entity_id).or_insert_with(|| {
                inst.params
                    .iter()
                    .filter_map(|(n, v)| match v {
                        crate::elab::ParamValue::Int(i) => Some((n.clone(), *i)),
                        crate::elab::ParamValue::Unknown => None,
                    })
                    .collect()
            });
        }
    }
    // Hierarchy owns the authoritative result of generate elaboration. Keep
    // its per-parent instance-array facts keyed by the same dotted paths IR
    // uses while recursively lowering bodies.
    for &root in &hier.roots {
        let path = hier.root_path(root);
        l.collect_instance_array_facts(hier, root, &path);
    }
    // Lower only the selected root designs. Their
    // sub-instances (and a testbench's DUTs) are lowered recursively from there,
    // each per-instance, so no entity is lowered standalone by type.
    let mut roots = Vec::new();
    for &r in &hier.roots {
        let ent = hier.instance(r).entity_id;
        if !roots.iter().any(|(id, _)| *id == ent) {
            roots.push((ent, hier.root_path(r)));
        }
    }
    for (entity, path) in &roots {
        l.lower_entity(*entity, path);
    }
    l.report_depth_exceeded();
    l.report_bad_operators();
    l.report_bad_conversions();
    l.report_unresolved_names();
    l.report_unelaborated_instance_uses();
    l.report_unsupported_exprs();
    l.lint_possible_latches();
    l.resolve_driver_contexts();
    l.propagate_metavalues();
    l.reconstruct_reads();
    l.lint_combinational_loops();
    l.lint_undriven_outputs();
    l.lint_unused_signals();
    // Resolve any logic literal that no typed context claimed to its position
    // in std's default logic type, so the IR the backends consume carries only
    // `Const`s — no raw chars, no compiler-side value table.
    l.normalize_logic_literals();
    compact_lookup_tables(&mut l.out);
    l.out
}

struct Lowering<'a> {
    sink: &'a mut DiagnosticSink,
    resolved: &'a Resolved,
    expr_types: HashMap<crate::diag::Span, crate::types::Ty>,
    /// Root for relative compile-time file reads (the source directory).
    base_dir: std::path::PathBuf,
    /// Signals given a default by a match wildcard arm — excluded from the
    /// possible-latch lint even though their lowered drivers are conditional.
    lint_defaulted: std::collections::HashSet<u32>,
    entities: HashMap<DefId, &'a ast::EntityDecl>,
    impls: HashMap<DefId, Vec<&'a ast::ImplDecl>>,
    /// Inherent implementations keyed by their complete nominal type. Unlike
    /// `impls`, which groups entity bodies by declaration id for recursive
    /// hierarchy lowering, this must distinguish applied views that share a
    /// view declaration leaf but have different backing structs.
    inherent_impls: HashMap<String, Vec<&'a ast::ImplDecl>>,
    /// Trait name -> its declaration, for the defaulted methods an
    /// implementing type inherits (spec 3.20: a trait body is a contract, and
    /// a method *with* a body is a default the impl may omit — which the type
    /// checker already allows, so dispatch has to find it).
    trait_decls: HashMap<String, &'a ast::TraitDecl>,
    /// Type head -> the traits it implements, for that fallback.
    implemented_traits: HashMap<String, Vec<String>>,
    /// Entity name -> its instance's concrete parameter values.
    entity_params: HashMap<DefId, HashMap<String, i64>>,
    /// Enum name -> variant name -> discriminant value.
    enum_variants: HashMap<String, HashMap<String, u64>>,
    /// Enum name -> discriminant of its *first* (declaration-order) variant,
    /// the derived `new()` default (VHDL `T'LEFT`): an uninitialized enum signal
    /// powers on holding this value, so it is always a valid member of the type.
    enum_first_disc: HashMap<String, u64>,
    /// Type name -> its `impl New for T` default value (a constant `new()`
    /// body), the uninitialized value a signal of that type powers on to. Beats
    /// the structural first-variant default. (`New for Logic` -> `'U'`.)
    new_defaults: HashMap<String, u64>,
    /// Source-owned multi-valued logic semantics, keyed by every enum identity
    /// in the implementing enum's nominal derivation family.
    logic_encodings: HashMap<String, LogicEncoding>,
    /// Struct name -> its declaration (for flattening struct signals).
    structs: HashMap<String, &'a ast::StructDecl>,
    /// Named, storage-free directional views.
    views: HashMap<String, &'a ast::ViewDecl>,
    /// View name -> per-leaf directions.
    view_dirs: HashMap<String, HashMap<String, ast::Direction>>,
    /// Enum name -> its bit width (repr, or bits for the variant count).
    enum_reprs: HashMap<String, u32>,
    /// Enum name -> base enum name (derivation chain, enums only).
    enum_bases: HashMap<String, String>,
    /// (trait name, target type) -> the impl's fns with the impl's declared
    /// rhs type (the `integer` in `impl Add<integer> for T`; `None` reads as
    /// `Self`). Overloads select by that rhs, or the fn's rhs parameter type.
    op_impls: OperatorImpls<'a>,
    /// Generic implementations whose target is an unconstrained scalar array.
    /// Nominal array families forward these when their element satisfies the
    /// implementation's constraint.
    blanket_array_impls: HashMap<String, String>,
    /// Literal suffix -> (target type, fn), for suffix inlining.
    suffix_impls: HashMap<String, (String, &'a ast::FnDecl)>,
    /// Module-level and static associated functions, inlined at call sites /
    /// const-evaluated without collapsing namespaced free functions by leaf.
    free_fns: FunctionIndex<'a>,
    /// Inline depth guard (recursive fns must const-fold; runaway inlining
    /// stops here).
    inline_depth: std::cell::Cell<u32>,
    /// Structs whose fields are currently being expanded, so a cyclic
    /// derivation terminates instead of overflowing the stack.
    expanding_structs: std::cell::RefCell<std::collections::HashSet<String>>,
    /// Functions whose inlining hit the depth guard. Lowering runs behind
    /// `&self`, so the diagnostic is recorded here and flushed by `lower`
    /// instead of silently leaving an `Unknown` in the driver.
    depth_exceeded: std::cell::RefCell<Vec<(String, crate::diag::Span)>>,
    /// Operands hoisted out of a per-element metavalue unroll by a helper that
    /// holds only `&self` -- resolution folding and the two partial-write
    /// helpers. Those cannot append a signal themselves, so they hoist here and
    /// the `&mut self` caller drains it with [`Lowering::flush_meta_temps`]
    /// immediately afterwards. Nothing may create a signal in between: the
    /// hoisted expressions already carry the ids they were promised.
    ///
    /// Left non-hoisting between those windows, so any other caller keeps the
    /// fully inlined lowering.
    meta_temps: std::cell::RefCell<MetaTemps>,
    /// Value names that resolved to nothing while lowering. Name
    /// resolution deliberately leaves plain value identifiers to later
    /// stages, and this is the stage that knows every signal, constant
    /// and parameter — so an unmatched name here is a genuine typo. It
    /// used to become a silent `Unknown`, which `check` reported as ok
    /// and a build reported as "driver 0 contains an Unknown".
    unresolved_names: std::cell::RefCell<Vec<(String, crate::diag::Span)>>,
    /// Lexical, storage-free values declared by `let` inside a hardware block.
    /// Each assignment replaces the binding with an expression (a conditional
    /// assignment becomes a `Select`), so event blocks retain next-state
    /// semantics for signals while their local values update immediately.
    block_scopes: std::cell::RefCell<Vec<HashMap<String, BlockLocal>>>,
    /// Field/index expressions with no hardware form. Recorded here with the
    /// source spelling, which the IR no longer has by the time validation sees
    /// an `Unknown`.
    unsupported_exprs: std::cell::RefCell<Vec<(String, crate::diag::Span)>>,
    /// Concrete instance-array shapes from hierarchy elaboration, keyed by
    /// the owning instance's dotted IR path.
    instance_array_facts: HashMap<String, Vec<crate::elab::InstanceArrayFact>>,
    /// Dotted path of the body currently being lowered.
    cur_instance_path: String,
    /// Reads of declared slots omitted by the active generate conditions.
    /// Expression lowering is intentionally `&self`, so these are collected
    /// through interior mutability and emitted after the design is lowered.
    unelaborated_instance_uses: std::cell::RefCell<Vec<UnelaboratedInstanceUse>>,
    /// The receiver signal bound to `self` while inlining a method body, so a
    /// `self'event`/`self'old` sysattr in the body resolves to the receiver's
    /// signal (the `ClockLike` edge methods are defined this way in std).
    self_signal: std::cell::Cell<Option<SignalId>>,
    /// Type-family of each generic-fn parameter during inlining (param name ->
    /// the concrete argument's family), so operator dispatch in the body uses
    /// the caller's type (e.g. signed's signed `Ord`, not the kernel compare).
    param_types: std::cell::RefCell<HashMap<String, String>>,
    /// Parameters of the function currently being inlined whose *declared*
    /// type is the kernel `integer`.
    ///
    /// Signedness of an operation is decided from the recorded type of its
    /// operand expressions, and a free function's body is type-checked with
    /// no parameters in scope, so every use of a parameter records as
    /// `Ty::Error`. `if v < 0` inside `fn f(v: integer)` therefore compiled to
    /// an *unsigned* comparison: `abs(n)` returned `n` for every negative `n`,
    /// and `min`/`max` picked the wrong side — in hardware only, since the
    /// testbench evaluates the body in C where the values are signed.
    param_integers: std::cell::RefCell<HashSet<String>>,
    /// A bound parameter's width, alongside its family in `param_types`. The
    /// family alone let the body dispatch `signed`'s operators while
    /// `self'length` inside them fell back to 1, so the sign-bit test shifted
    /// by 0 and `abs(-5)` returned 251.
    param_widths: std::cell::RefCell<HashMap<String, u32>>,
    /// Module-level integer constants (`const N: integer = 4`).
    consts: HashMap<String, i64>,
    /// Exact literal values for module constants, including values wider than
    /// the signed width/parameter evaluator can represent.
    const_values: HashMap<String, Expr>,
    /// Constant lookup tables (`const TAB: unsigned[8][4] = [..]`), whose
    /// elements are folded individually. `const_values` holds one scalar
    /// per name and has no room for a sequence, so an indexed read of a
    /// const array used to find nothing and lower to `Unknown`.
    const_arrays: HashMap<String, Vec<Expr>>,
    /// Module-level `real` constants (`const PI: real = 3.14159...`).
    consts_real: HashMap<String, f64>,
    /// Module-level range constants (`const BYTE: range = 7..0`), as written
    /// (left, right) so direction is preserved.
    const_ranges: HashMap<String, (i64, i64)>,
    /// Type aliases (`using Word = unsigned[32]`).
    aliases: HashMap<String, ast::Type>,
    /// The active entity's width environment (consts + instance params),
    /// for const-evaluating slice bounds during expression lowering.
    cur_env: HashMap<String, i64>,
    /// The active entity's type-parameter bindings (`T -> unsigned[8]` for a
    /// generic entity `Buf<unsigned[8]>`), substituted into port/signal types.
    cur_type_env: HashMap<String, ast::Type>,
    /// The stack of entity names currently being lowered (`lower_body`), so a
    /// sub-instance that would re-enter an entity already on the stack is
    /// skipped instead of recursing forever. The elaborator has already
    /// emitted the `cyclic instantiation` diagnostic; this just keeps lowering
    /// from overflowing on the same cycle (best-effort, spec cross-cutting).
    lower_stack: Vec<DefId>,
    /// Plain (non-bus-mode, non-`inout`) `out` port signals, for the
    /// undriven-output warning after all drivers are collected.
    plain_out_ports: Vec<SignalId>,
    /// Internal `let` signals with no initializer, in a non-`#[test]` entity —
    /// they must be driven, so an undriven one is a forgotten assignment.
    undriven_lets: Vec<SignalId>,
    /// Internal component locals eligible for W-P003. Test/top locals are
    /// externally observed by the runner and deliberately excluded.
    unused_lets: Vec<SignalId>,
    out: Design,
    /// Signal name -> id, valid while lowering a single entity.
    locals: HashMap<String, SignalId>,
    /// Local name -> its enum type name (operator-impl operands).
    local_enum: HashMap<String, String>,
    /// Local name -> its struct type name (multi-signal operands/targets).
    /// Applied views store the view name here so method dispatch uses the
    /// view's nominal interface rather than the backing struct.
    local_struct: HashMap<String, String>,
    /// Local name -> its backing struct representation. This differs from
    /// `local_struct` for an applied view (`Controller Bus`): methods dispatch
    /// on `Controller`, while reads of the aggregate bind all fields of `Bus`.
    local_struct_repr: HashMap<String, String>,
    /// Locals of the symbol base type `Char`.
    local_char: std::collections::HashSet<String>,
    /// Array-typed locals -> their ordered element indices (whole-array
    /// assignment and string literals expand per element).
    local_array: HashMap<String, Vec<i64>>,
    /// Binary operators where the left operand's type has `Operator` impls but
    /// none accepts the right operand. For an aggregate struct there is no
    /// builtin arithmetic to fall back to, so the expression produced nothing
    /// and the assignment it fed was silently dropped.
    bad_operators: std::cell::RefCell<Vec<BadOperator>>,
    /// Conversions `T(x)` where `T` names a real struct/enum but no `From`
    /// impl and no derivation connects it to the argument's type. Lowering
    /// left an `Unknown`, which surfaced at the very end as "contains an
    /// Unknown (unlowered) expression" with no code and no span.
    bad_conversions: std::cell::RefCell<Vec<(String, Option<String>, crate::diag::Span)>>,
    /// Locals whose element type is an entity (`let stage: Inc[3]`), so an
    /// out-of-range element can be named an instance the way the `types` check
    /// names it rather than being called an array element.
    instance_arrays: HashSet<String>,
    /// Out-of-range constant indices already reported, keyed by
    /// (array, index, span start). One source index is visited once per
    /// generate iteration, so without this a loop reports the same element
    /// as many times as the loop is long.
    reported_oob: HashSet<(String, i64, u32)>,
    /// Generated dead assignments already reported, keyed by the two source
    /// sites and the concrete target after loop/parameter substitution. The
    /// same entity body may be lowered for several instances; diagnostics are
    /// source facts and must not repeat per instance.
    reported_generated_dead_assignments: HashSet<(crate::diag::Span, crate::diag::Span, String)>,
    /// The assignment statement being lowered, so the drivers and next-state
    /// updates it produces can carry it without every construction site
    /// having to be handed one. `None` outside statement lowering — the
    /// drivers synthesized there answer to no source line.
    cur_span: Option<crate::diag::Span>,
    /// The active driver context (bumped per process/concurrent statement or
    /// connection).
    cur_ctx: u32,
    /// Driver context -> the source site that created it (a port connection's
    /// value expression). Lets the conflicting-driver error point at each
    /// contributing connection instead of naming only the signal.
    ctx_span: HashMap<u32, crate::diag::Span>,
    /// Signal -> declared type name (enum / unsigned / signed), for Resolve lookup.
    sig_type: HashMap<u32, String>,
    /// Nominal array families (`struct F(Logic[])`). Unsigned and signed are
    /// just the first two; the family set is read from declarations.
    array_families: std::collections::HashSet<String>,
    /// Numeric-vector locals -> the family name, for operator-impl dispatch
    /// (kernel `integer`/`real` keep builtin operators; unsigned/signed live in std).
    local_numeric: HashMap<String, String>,
}

/// A lowered value: a scalar expression, or one expression per struct field
/// (a struct-typed value has no single-signal representation).
#[derive(Clone, Debug)]
enum Val {
    Scalar(Expr),
    Fields(Vec<(String, Expr)>),
}

/// One suffix in a flattened aggregate access. Keeping the source expression
/// for an index lets a runtime access expand into a mux or gated writes over
/// the concrete leaf signals.
#[derive(Clone, Copy)]
pub(super) enum AccessStep<'e> {
    Field(&'e str),
    Index(&'e ast::Expr),
}

enum DynamicWriteTarget {
    Whole {
        signal: SignalId,
        hit: Expr,
    },
    PackedBit {
        signal: SignalId,
        position: u32,
        hit: Expr,
    },
}

/// One block-local binding. The declared type is retained because substituting
/// the value expression alone would lose its operator family and width.
#[derive(Clone, Debug)]
struct BlockLocal {
    value: Val,
    ty: ast::Type,
}

#[derive(Clone, Debug)]
struct UnelaboratedInstanceUse {
    slot: String,
    parent_path: String,
    use_span: crate::diag::Span,
    declaration_span: crate::diag::Span,
}

impl UnelaboratedInstanceUse {
    /// The storage root this slot belongs to, i.e. the path prefix its leaves
    /// share.
    fn slot_root(&self) -> &str {
        self.slot
            .split_once('[')
            .map_or(self.slot.as_str(), |(root, _)| root)
    }
}

/// `cond ? then : els` over values; struct values select per field.
fn select_val(cond: Expr, then: Val, els: Val) -> Val {
    match (then, els) {
        (Val::Scalar(t), Val::Scalar(e)) => Val::Scalar(Expr::Select {
            cond: Box::new(cond),
            then: Box::new(t),
            els: Box::new(e),
        }),
        (Val::Fields(ts), Val::Fields(es)) => Val::Fields(
            ts.into_iter()
                .map(|(name, t)| {
                    let e = es
                        .iter()
                        .find(|(n, _)| *n == name)
                        .map(|(_, e)| e.clone())
                        .unwrap_or(Expr::Unknown);
                    (
                        name,
                        Expr::Select {
                            cond: Box::new(cond.clone()),
                            then: Box::new(t),
                            els: Box::new(e),
                        },
                    )
                })
                .collect(),
        ),
        _ => Val::Scalar(Expr::Unknown),
    }
}
