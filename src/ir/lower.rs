//! Siox frontend lowering into the language-neutral digital IR.
//!
//! This module is the only large IR component allowed to depend on Siox AST
//! details. The surrounding modules define and transform backend-facing data.

use super::*;

mod diagnostics;
mod expressions;
mod layout;
mod metavalue;
mod statements;

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

impl<'a> Lowering<'a> {
    /// Record which slots of each instance array were declared and which
    /// survived generate elaboration, so an intentionally absent element is
    /// distinguishable from an unresolved path.
    fn collect_instance_array_facts(
        &mut self,
        hierarchy: &Hierarchy,
        id: crate::elab::InstanceId,
        path: &str,
    ) {
        let instance = hierarchy.instance(id);
        if !instance.instance_arrays.is_empty() {
            self.instance_array_facts
                .insert(path.to_string(), instance.instance_arrays.clone());
        }
        for &child in &instance.children {
            let child_instance = hierarchy.instance(child);
            self.collect_instance_array_facts(
                hierarchy,
                child,
                &format!("{path}.{}", child_instance.name),
            );
        }
    }

    /// A lowering pass reporting into `sink`, over the given resolution.
    fn new(sink: &'a mut DiagnosticSink, resolved: &'a Resolved) -> Self {
        Lowering {
            sink,
            resolved,
            expr_types: HashMap::new(),
            base_dir: std::path::PathBuf::new(),
            lint_defaulted: std::collections::HashSet::new(),
            entities: HashMap::new(),
            impls: HashMap::new(),
            inherent_impls: HashMap::new(),
            trait_decls: HashMap::new(),
            implemented_traits: HashMap::new(),
            entity_params: HashMap::new(),
            enum_variants: HashMap::new(),
            enum_first_disc: HashMap::new(),
            new_defaults: HashMap::new(),
            logic_encodings: HashMap::new(),
            structs: HashMap::new(),
            views: HashMap::new(),
            view_dirs: HashMap::new(),
            enum_reprs: HashMap::new(),
            enum_bases: HashMap::new(),
            op_impls: HashMap::new(),
            blanket_array_impls: HashMap::new(),
            suffix_impls: HashMap::new(),
            free_fns: FunctionIndex::new(resolved),
            inline_depth: std::cell::Cell::new(0),
            meta_temps: std::cell::RefCell::new(MetaTemps::inline_only()),
            expanding_structs: std::cell::RefCell::new(std::collections::HashSet::new()),
            depth_exceeded: std::cell::RefCell::new(Vec::new()),
            unresolved_names: std::cell::RefCell::new(Vec::new()),
            block_scopes: std::cell::RefCell::new(Vec::new()),
            unsupported_exprs: std::cell::RefCell::new(Vec::new()),
            instance_array_facts: HashMap::new(),
            cur_instance_path: String::new(),
            unelaborated_instance_uses: std::cell::RefCell::new(Vec::new()),
            self_signal: std::cell::Cell::new(None),
            param_types: std::cell::RefCell::new(HashMap::new()),
            param_integers: std::cell::RefCell::new(HashSet::new()),
            param_widths: std::cell::RefCell::new(HashMap::new()),
            consts: HashMap::new(),
            const_values: HashMap::new(),
            const_arrays: HashMap::new(),
            consts_real: HashMap::new(),
            const_ranges: HashMap::new(),
            aliases: HashMap::new(),
            cur_env: HashMap::new(),
            cur_type_env: HashMap::new(),
            lower_stack: Vec::new(),
            plain_out_ports: Vec::new(),
            undriven_lets: Vec::new(),
            unused_lets: Vec::new(),
            out: Design::default(),
            locals: HashMap::new(),
            local_enum: HashMap::new(),
            local_struct: HashMap::new(),
            local_struct_repr: HashMap::new(),
            local_char: std::collections::HashSet::new(),
            local_array: HashMap::new(),
            bad_operators: std::cell::RefCell::new(Vec::new()),
            bad_conversions: std::cell::RefCell::new(Vec::new()),
            instance_arrays: HashSet::new(),
            cur_span: None,
            reported_oob: HashSet::new(),
            reported_generated_dead_assignments: HashSet::new(),
            local_numeric: HashMap::new(),
            array_families: std::collections::HashSet::new(),
            cur_ctx: 0,
            ctx_span: HashMap::new(),
            sig_type: HashMap::new(),
        }
    }

    /// Index every declaration the lowering needs -- entities, structs, enums,
    /// functions and impls -- so later lookups are by definition rather than by
    /// name.
    fn collect(&mut self, modules: &'a [Module]) {
        let mut constant_decls = Vec::new();
        for m in modules {
            for item in &m.items {
                match item {
                    ast::Item::Entity(e) => {
                        if let Some(id) = self.resolved.declared(e.name.span) {
                            self.entities.insert(id, e);
                        }
                    }
                    ast::Item::Fn(f) => {
                        self.free_fns.insert_free(f);
                    }
                    ast::Item::ExternBlock { fns, .. } => {
                        for f in fns {
                            self.free_fns.insert_free(f);
                        }
                    }
                    ast::Item::Struct(s) => {
                        self.structs
                            .insert(self.free_fns.struct_decl_key(&s.name), s);
                    }
                    ast::Item::View(v) => {
                        let target = self
                            .free_fns
                            .type_head_key(&v.target)
                            .unwrap_or_else(|| "<error>".to_string());
                        let key = format!("{}@{target}", self.free_fns.view_decl_key(&v.name));
                        self.views.insert(key.clone(), v);
                        self.view_dirs.insert(
                            key,
                            v.fields
                                .iter()
                                .map(|f| (f.name.text.clone(), f.dir))
                                .collect(),
                        );
                    }
                    // Module constants join the width environment; range
                    // constants (`const BYTE: range = 7..0`) keep their
                    // written direction. Aliases substitute during lowering.
                    ast::Item::Const(c) => {
                        constant_decls.push((self.free_fns.constant_decl_key(c), c));
                    }
                    ast::Item::Using(u) => {
                        if let ast::UsingKind::Alias { name, ty } = &u.kind {
                            self.aliases
                                .insert(self.free_fns.type_alias_decl_key(name), ty.clone());
                        }
                    }
                    ast::Item::Trait(t) => {
                        self.trait_decls
                            .insert(self.free_fns.trait_decl_key(&t.name), t);
                    }
                    ast::Item::Impl(im) if im.trait_.is_none() => {
                        self.register_static_fns(im);
                        if let Some(key) = self.free_fns.type_head_key(&im.target) {
                            self.inherent_impls.entry(key).or_default().push(im);
                        }
                        if let Some(id) = type_def_id(&im.target, self.resolved) {
                            self.impls.entry(id).or_default().push(im);
                        }
                    }
                    // A trait impl's first fn is the operator body for
                    // `impl "+" for T` (spec 3.25); an `impl Suffix<"ns", _>
                    // for T` defines the literal suffix named by its symbol
                    // argument, its `suffix` method inlined at the use site
                    // (spec 3.24).
                    ast::Item::Impl(im) => {
                        let trait_path = im.trait_.as_ref();
                        let trait_key =
                            trait_path.and_then(|path| self.free_fns.trait_path_key(path));
                        let target = self.free_fns.type_head_key(&im.target);
                        if let (Some(tr), Some(ty)) = (trait_key.as_ref(), target.as_ref()) {
                            self.implemented_traits
                                .entry(ty.clone())
                                .or_default()
                                .push(tr.clone());
                        }
                        self.register_static_fns(im);
                        if let (Some(tr), Some(ty)) = (trait_key.as_ref(), target.as_ref()) {
                            if tr == "Suffix" {
                                let symbol = im.trait_args.first().and_then(|a| match a {
                                    ast::GenericArg::Positional(ast::Expr::StrLit {
                                        text, ..
                                    }) => Some(text.clone()),
                                    _ => None,
                                });
                                if let Some(symbol) = symbol {
                                    for it in &im.items {
                                        if let ast::ImplItem::Fn(f) = it {
                                            self.suffix_impls
                                                .insert(symbol.clone(), (ty.clone(), f));
                                        }
                                    }
                                }
                            } else {
                                // `impl Operator<"+", integer, _> for T`: the
                                // symbol keys the impl and the next trait
                                // argument names the rhs operand type. A
                                // non-operator trait (Resolve/New/From) keys by
                                // its own name and reads its first type arg.
                                let op_symbol = (tr == "Operator")
                                    .then(|| im.trait_args.first())
                                    .flatten()
                                    .and_then(|a| match a {
                                        ast::GenericArg::Positional(ast::Expr::StrLit {
                                            text,
                                            ..
                                        }) => Some(text.clone()),
                                        _ => None,
                                    });
                                let input_index = usize::from(op_symbol.is_some());
                                let rhs_arg =
                                    im.trait_args.get(input_index).and_then(|a| match a {
                                        ast::GenericArg::Positional(ast::Expr::Path(p)) => {
                                            self.free_fns.type_path_key(p)
                                        }
                                        ast::GenericArg::PositionalType(ty) => {
                                            self.free_fns.type_head_key(ty)
                                        }
                                        _ => None,
                                    });
                                let operator = op_symbol.unwrap_or_else(|| tr.clone());
                                if is_blanket_array_impl(im) {
                                    let requirement = blanket_requirement(im, &self.free_fns)
                                        .unwrap_or_else(|| operator.clone());
                                    self.blanket_array_impls.insert(operator, requirement);
                                    continue;
                                }
                                for it in &im.items {
                                    if let ast::ImplItem::Fn(f) = it {
                                        self.op_impls
                                            .entry((operator.clone(), ty.clone()))
                                            .or_default()
                                            .push((f, rhs_arg.clone()));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // A trait's `self`-less defaults are associated functions the
        // implementing type inherits, callable as `Thing::tag()`. Registering
        // them needs every trait declaration, so it waits until collection is
        // done — a trait may be written after the impl that implements it.
        let inherited: Vec<(String, &'a ast::FnDecl)> = self
            .implemented_traits
            .iter()
            .flat_map(|(ty, traits)| traits.iter().map(move |tr| (ty.clone(), tr)))
            .filter_map(|(ty, tr)| self.trait_decls.get(tr.as_str()).map(|t| (ty, *t)))
            .flat_map(|(ty, t)| {
                t.items
                    .iter()
                    .filter(|f| f.body.is_some() && !f.params.iter().any(|p| p.is_self))
                    .map(move |f| (ty.clone(), f))
            })
            .collect();
        for (ty, f) in inherited {
            // The impl's own statics went in during collection, so this only
            // supplies what it omitted.
            self.free_fns
                .insert_associated_default(format!("{ty}::{}", f.name.text), f);
        }
        // Constants are order-independent. Keep the narrow signed value table
        // for widths/generate conditions, and a separate exact literal table
        // for signal values so a 128-bit constant never passes through i64.
        for _ in 0..=constant_decls.len() {
            let mut progressed = false;
            for (key, constant) in &constant_decls {
                let scope = self.consts.clone();
                progressed |= self.fold_const(key, constant, &scope);
            }
            if !progressed {
                break;
            }
        }
    }

    /// Register an impl's *static* associated fns (those without a `self`
    /// parameter) under a `Type::name` key, so `Unicode::code(c)` is callable
    /// in expressions through the same const-folding/inlining path as a
    /// module-level `fn`. Methods take `self` and dispatch via the receiver.
    fn register_static_fns(&mut self, im: &'a ast::ImplDecl) {
        let Some(ty) = self.free_fns.type_head_key(&im.target) else {
            return;
        };
        for it in &im.items {
            if let ast::ImplItem::Fn(f) = it {
                if !f.params.iter().any(|p| p.is_self) {
                    self.free_fns
                        .insert_associated(format!("{ty}::{}", f.name.text), f);
                }
            }
        }
    }

    /// Lower one entity instance: its ports, state and behavior, under
    /// `root_path`.
    fn lower_entity(&mut self, entity_id: DefId, root_path: &str) {
        let Some(edecl) = self.entities.get(&entity_id).copied() else {
            return;
        };
        // Extern entities are black boxes.
        if edecl.is_extern {
            return;
        }
        let mut env = self.consts.clone();
        env.extend(
            self.entity_params
                .get(&entity_id)
                .cloned()
                .unwrap_or_default(),
        );
        if is_test_entity(edecl, self.resolved) {
            // A testbench: lower only its DUT instances, each per-instance under
            // the testbench path (`CounterTest.dut.*`), so two instances of one
            // entity are distinct. Stimulus statements are interpreted by the
            // runner, and testbench<->DUT connections go through the runner's
            // signal map — so no top-level connection drivers here. Its local
            // values still need concrete layouts: the native runner consumes
            // the finished IR rather than independently specializing AST
            // declarations.
            self.persist_testbench_layouts(entity_id, root_path, &env);
            self.lower_testbench_duts(entity_id, root_path, &env);
            return;
        }
        // A top-level DUT: signals are entity-qualified (`Counter.count`), and
        // widths come from its first instance's parameters.
        self.lower_body(
            entity_id,
            root_path,
            &env,
            &HashMap::new(),
            &HashMap::new(),
            true,
        );
    }

    /// Persist concrete layouts for testbench-owned values without creating
    /// hardware signals for them. Native execution owns their storage, while
    /// `Design` remains the authoritative source for aggregate shape, ranges,
    /// scalar families, and widths.
    fn persist_testbench_layouts(
        &mut self,
        entity_id: DefId,
        entity: &str,
        env: &HashMap<String, i64>,
    ) {
        let impls: Vec<&ast::ImplDecl> = self.impls.get(&entity_id).cloned().unwrap_or_default();
        for im in impls {
            for item in &im.items {
                let ast::ImplItem::Let(declaration) = item else {
                    continue;
                };
                if instance_let_parts(declaration, &self.entities, self.resolved).is_some() {
                    continue;
                }
                let Some(ty) = declaration.ty.as_ref() else {
                    continue;
                };
                let layout = self.source_layout(ty, env);
                self.persist_layout_tree(entity, &declaration.name.text, &layout);
            }
        }
    }

    /// Record the source layout for a value and, recursively, its fields, so
    /// consumers can rebuild the declared shape after flattening.
    fn persist_layout_tree(&mut self, entity: &str, name: &str, layout: &SourceLayout) {
        self.out
            .source_layouts
            .insert(format!("{entity}.{name}"), layout.clone());
        match &layout.kind {
            LayoutKind::Struct { fields, .. } => {
                for field in fields {
                    self.persist_layout_tree(
                        entity,
                        &format!("{name}.{}", field.name),
                        &field.layout,
                    );
                }
            }
            LayoutKind::Array {
                range: Some(range),
                element,
            } => {
                for index in loop_range(range.left, range.right) {
                    self.persist_layout_tree(entity, &format!("{name}[{index}]"), element);
                }
            }
            LayoutKind::Array { range: None, .. }
            | LayoutKind::Packed { .. }
            | LayoutKind::Scalar { .. }
            | LayoutKind::Opaque { .. } => {}
        }
    }

    /// Lower each `let inst: Sub = { .. }` DUT of a testbench into its own
    /// namespace `<testbench>.<inst>.*` (with the DUT's internal logic and
    /// sub-instances). No testbench signals, statements, or top connections.
    fn lower_testbench_duts(&mut self, entity_id: DefId, name: &str, env: &HashMap<String, i64>) {
        let impls: Vec<&ast::ImplDecl> = self.impls.get(&entity_id).cloned().unwrap_or_default();
        // Every port a testbench name is connected to, across all DUTs — when
        // one name binds an `out` and `in` ports (a DUT feeding another, or
        // its own input), the out drives the ins as real hardware, so the
        // value propagates on every settle without runner involvement.
        let mut bindings: HashMap<
            String,
            Vec<(SignalId, Option<ast::Direction>, crate::diag::Span)>,
        > = HashMap::new();
        for im in &impls {
            for item in &im.items {
                if let ast::ImplItem::Let(l) = item {
                    if let Some((cty, args)) = instance_let_parts(l, &self.entities, self.resolved)
                    {
                        if let Some(sub) = type_def_id(&cty, self.resolved) {
                            let sub_path = format!("{name}.{}", l.name.text);
                            let mut sub_env = self.consts.clone();
                            sub_env.extend(self.construct_params(&cty, sub, env));
                            let sub_tenv = self.construct_type_params(&cty, sub);
                            let sub_ports = self.lower_body(
                                sub,
                                &sub_path,
                                &sub_env,
                                &sub_tenv,
                                &HashMap::new(),
                                false,
                            );
                            for (port, value) in self.norm_conns(&args, sub) {
                                // The testbench name the port binds to; a
                                // literal/expression connection has no name.
                                let Some(tbname) = expr_path(&value) else {
                                    continue;
                                };
                                if let Some(&(sig, dir)) = sub_ports.get(&port) {
                                    bindings.entry(tbname).or_default().push((
                                        sig,
                                        dir,
                                        ast::expr_span(&value),
                                    ));
                                    continue;
                                }
                                // A struct/bus port is not one signal: it is
                                // flattened into leaves (`bus.valid`, ...), so
                                // the scalar lookup above finds nothing and the
                                // binding used to be dropped — two testbench
                                // DUTs sharing a struct local were left
                                // unconnected, silently. Bind each leaf to the
                                // matching leaf of the testbench name.
                                let prefix = format!("{port}.");
                                for (pname, &(sig, dir)) in sub_ports.iter() {
                                    let Some(leaf) = pname.strip_prefix(&prefix) else {
                                        continue;
                                    };
                                    bindings
                                        .entry(format!("{tbname}.{leaf}"))
                                        .or_default()
                                        .push((sig, dir, ast::expr_span(&value)));
                                }
                            }
                        }
                    }
                }
            }
        }
        for (tbname, ports) in &bindings {
            // A tristate net needs one shared node that folds each driver's
            // *expression*; the entity path builds one, this path has no
            // testbench signal to build it on. Connecting an `inout` here used
            // to bind nothing at all and read back high-Z, as though no one
            // were driving — report it instead of simulating a lie.
            if ports
                .iter()
                .any(|(_, d, _)| *d == Some(ast::Direction::Inout))
            {
                let span = ports
                    .iter()
                    .find(|(_, d, _)| *d == Some(ast::Direction::Inout))
                    .map(|(_, _, span)| *span)
                    .expect("an inout binding has a source span");
                self.sink.emit(
                    crate::diag::Diagnostic::error(format!(
                        "`{tbname}` connects an `inout` port between testbench instances"
                    ))
                    .with_code(crate::diag::codes::INVALID_METHOD_CALL)
                    .at(span)
                    .help(
                        "a shared tristate net is built inside an entity — wire the \
                         instances there and drive that entity from the testbench",
                    ),
                );
                continue;
            }
            let outs: Vec<SignalId> = ports
                .iter()
                .filter(|(_, d, _)| *d == Some(ast::Direction::Out))
                .map(|&(s, _, _)| s)
                .collect();
            let ins: Vec<SignalId> = ports
                .iter()
                .filter(|(_, d, _)| *d == Some(ast::Direction::In))
                .map(|&(s, _, _)| s)
                .collect();
            if outs.is_empty() || ins.is_empty() {
                continue;
            }
            // Each out contributes in its own context; several outs onto one
            // name then fold through the type's Resolve (or error), exactly
            // like parallel drivers anywhere else.
            for &o in &outs {
                let ctx = self.next_ctx();
                for &i in &ins {
                    self.out.drivers.push(Driver {
                        span: self.cur_span,
                        target: i,
                        cond: None,
                        expr: Expr::Current(o),
                        meta: None,
                        ctx,
                    });
                }
            }
        }
    }

    /// Lower entity `ename`'s body, naming signals under `path` (the instance
    /// path — `Counter.count` at the top, `Add2.s1.a` for a sub-instance) in the
    /// width environment `env`. Sub-instances (`let s: Sub = { .p = x, .. }`) are
    /// lowered recursively under `path.s` and their port connections become
    /// drivers. Returns each port's (signal, direction) so a parent can wire to
    /// it. Runs in a fresh name scope, restoring the caller's on return.
    /// Fold one constant declaration into the constant tables, returning
    /// whether it produced a value. `scope` is what integer expressions
    /// evaluate against: module constants see the constant table, an
    /// implementation's own constants also see the entity's parameters.
    ///
    /// Shared so the two callers cannot diverge by *kind* — the first version
    /// of implementation constants folded integers only, so a `const` holding
    /// a lookup table or a real reported its own name as unknown while the
    /// module-level spelling of it worked.
    fn fold_const(
        &mut self,
        name: &str,
        constant: &ast::ConstDecl,
        scope: &HashMap<String, i64>,
    ) -> bool {
        if self.const_ranges.contains_key(name)
            || self.consts.contains_key(name)
            || self.consts_real.contains_key(name)
            || self.const_values.contains_key(name)
            || self.const_arrays.contains_key(name)
        {
            return false;
        }
        if let ast::Expr::Range { lo, hi, .. } = &constant.value {
            if let (Some(left), Some(right)) =
                (self.eval_const(lo, scope), self.eval_const(hi, scope))
            {
                self.const_ranges.insert(name.to_string(), (left, right));
                return true;
            }
        } else if let ast::Expr::Array { elems, .. } = &constant.value {
            // A constant lookup table. Every element has to fold, or the table
            // is left for a later round of the fixed point (an element may
            // name a constant not yet resolved).
            let values: Option<Vec<Expr>> = elems
                .iter()
                .map(|e| lower_const_value(e, &self.const_values, scope, &self.free_fns))
                .collect();
            if let Some(values) = values {
                self.const_arrays.insert(name.to_string(), values);
                return true;
            }
        } else if let Some(args) = self
            .free_fns
            .type_head_key(&constant.ty)
            .and_then(|head| self.positional_struct_args(&head, &constant.value))
        {
            // `const P: Pair = { 6, 7 };` — the same constant written without
            // field names. The declared type says these braces are a struct
            // literal, so they bind by declaration order.
            let Some(fields) = self.const_struct_fields(&constant.ty, &args) else {
                return false;
            };
            for (field, value) in fields {
                let key = format!("{name}.{field}");
                if let Some(narrow) = self.eval_const(&value, scope) {
                    self.consts.insert(key.clone(), narrow);
                }
                let Some(lowered) =
                    lower_const_value(&value, &self.const_values, scope, &self.free_fns)
                else {
                    return false;
                };
                self.const_values.insert(key, lowered);
            }
            return true;
        } else if let ast::Expr::Construct { args, .. } = &constant.value {
            // `const K: Pair = { .a = 4, .b = 5 };` — a struct constant is one
            // folded value per field, keyed by the dotted path a read spells.
            // Nothing folded it before, so the constant never entered any
            // table: `K.a` reported "has no hardware form" (a message about
            // runtime indices, on a source with no index) and `p = K` reported
            // `K` as an unknown name — both after stage 4 had accepted the
            // declaration with no diagnostic at all.
            let Some(fields) = self.const_struct_fields(&constant.ty, args) else {
                return false;
            };
            for (field, value) in fields {
                let key = format!("{name}.{field}");
                if let Some(narrow) = self.eval_const(&value, scope) {
                    self.consts.insert(key.clone(), narrow);
                }
                let Some(lowered) =
                    lower_const_value(&value, &self.const_values, scope, &self.free_fns)
                else {
                    return false;
                };
                self.const_values.insert(key, lowered);
            }
            return true;
        } else if let ast::Expr::Int { text, .. } = &constant.value {
            if text.contains('.') {
                if let Ok(value) = text.replace('_', "").parse::<f64>() {
                    self.consts_real.insert(name.to_string(), value);
                    return true;
                }
            } else if let Some(value) = integer_const(text) {
                if let Expr::Const(word) = value {
                    if let Ok(narrow) = i64::try_from(word) {
                        self.consts.insert(name.to_string(), narrow);
                    }
                    self.const_values
                        .insert(name.to_string(), Expr::Const(word));
                } else {
                    self.const_values.insert(name.to_string(), value);
                }
                return true;
            }
        } else if let ast::Expr::Path(path) = &constant.value {
            // `const M: Mode = Mode::Fast;` — an enum variant is a value like
            // any other. Only a single-segment path was folded, so the
            // constant never entered the tables and every read of it reported
            // the name as unknown; binding it to a signal first happened to
            // work, which is what made it look supported.
            if path.segments.len() >= 2 {
                if let Some(disc) = self.enum_variant_path(path) {
                    self.consts.insert(name.to_string(), disc as i64);
                    self.const_values
                        .insert(name.to_string(), Expr::Const(disc));
                    return true;
                }
            }
            if let Some(source) = self.free_fns.constant_path_key(path) {
                if let Some(value) = self.const_values.get(&source).cloned() {
                    self.const_values.insert(name.to_string(), value);
                    return true;
                } else if let Some(&value) = self.consts.get(&source) {
                    self.consts.insert(name.to_string(), value);
                    return true;
                }
            }
        } else if let Some(value) =
            lower_const_value(&constant.value, &self.const_values, scope, &self.free_fns)
        {
            self.const_values.insert(name.to_string(), value);
            if let Some(narrow) = self.eval_const(&constant.value, scope) {
                self.consts.insert(name.to_string(), narrow);
            }
            return true;
        } else if let Some(value) = self.eval_const(&constant.value, scope) {
            self.consts.insert(name.to_string(), value);
            return true;
        }
        false
    }

    /// Fold a constant expression in `env`, or `None` when it is not constant.
    fn eval_const(&self, expression: &ast::Expr, env: &HashMap<String, i64>) -> Option<i64> {
        eval_const_fns(expression, env, &self.free_fns, 0)
    }

    /// Lower an entity's impl body: declarations, processes and concurrent
    /// statements.
    fn lower_body(
        &mut self,
        entity_id: DefId,
        path: &str,
        env: &HashMap<String, i64>,
        type_env: &HashMap<String, ast::Type>,
        aliases: &HashMap<String, SignalId>,
        is_root: bool,
    ) -> HashMap<String, (SignalId, Option<ast::Direction>)> {
        let Some(edecl) = self.entities.get(&entity_id).copied() else {
            return HashMap::new();
        };

        // Save the caller's scope; give this body a fresh one.
        let saved_instance_path = std::mem::replace(&mut self.cur_instance_path, path.to_string());
        let saved_locals = std::mem::take(&mut self.locals);
        let saved_enum = std::mem::take(&mut self.local_enum);
        let saved_struct = std::mem::take(&mut self.local_struct);
        let saved_struct_repr = std::mem::take(&mut self.local_struct_repr);
        let saved_char = std::mem::take(&mut self.local_char);
        let saved_array = std::mem::take(&mut self.local_array);
        let saved_numeric = std::mem::take(&mut self.local_numeric);
        let saved_instance_arrays = std::mem::take(&mut self.instance_arrays);
        // A Rust-style binder may rename: `impl<M: integer> Counter<M>` calls
        // the entity's first parameter `M` inside its own body. Every lookup
        // below — signal widths as much as expressions — goes through `env`,
        // which is keyed by the entity's declared names, so extend it with
        // each impl's names bound by position, as Rust binds them.
        let mut renamed = env.clone();
        let mut renamed_types = type_env.clone();
        {
            let bodies: Vec<&ast::ImplDecl> =
                self.impls.get(&entity_id).cloned().unwrap_or_default();
            for im in bodies {
                let ast::Type::Generic { args, .. } = &im.target else {
                    continue;
                };
                for (i, arg) in args.iter().enumerate() {
                    let ast::GenericArg::Positional(ast::Expr::Path(path)) = arg else {
                        continue;
                    };
                    let ([seg], Some(param)) =
                        (path.segments.as_slice(), edecl.params.params.get(i))
                    else {
                        continue;
                    };
                    if seg.text == param.name.text {
                        continue;
                    }
                    if let Some(&value) = env.get(&param.name.text) {
                        renamed.insert(seg.text.clone(), value);
                    }
                    if let Some(ty) = type_env.get(&param.name.text) {
                        renamed_types.insert(seg.text.clone(), ty.clone());
                    }
                }
            }
        }
        // Constants declared *inside* an implementation (spec 3.3) were never
        // collected — only module-level ones were — so `const MAX: unsigned[W]
        // = (1 << W) - 1;` compiled and then every read reported the name as
        // unknown. They fold here rather than globally because the spec's own
        // example depends on the entity's parameters, so one declaration is a
        // different number per instance. They go into the *env* as well as the
        // constant tables: array sizes and slice bounds resolve through the
        // env, so a constant missing from it left `let regs: unsigned[8][K]`
        // with no elements at all.
        let saved_consts = self.consts.clone();
        let saved_const_values = self.const_values.clone();
        {
            let body_consts: Vec<&ast::ConstDecl> = self
                .impls
                .get(&entity_id)
                .map(|impls| {
                    impls
                        .iter()
                        .flat_map(|im| &im.items)
                        .filter_map(|item| match item {
                            ast::ImplItem::Const(c) => Some(c),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            for _ in 0..=body_consts.len() {
                let mut progressed = false;
                for c in &body_consts {
                    let mut scope = self.consts.clone();
                    scope.extend(renamed.iter().map(|(k, v)| (k.clone(), *v)));
                    if self.fold_const(&c.name.text, c, &scope) {
                        progressed = true;
                        // Array sizes and slice bounds resolve through the
                        // env, so an integer constant has to reach it too.
                        if let Some(&value) = self.consts.get(&c.name.text) {
                            renamed.insert(c.name.text.clone(), value);
                        }
                    }
                }
                if !progressed {
                    break;
                }
            }
        }
        let env = &renamed;
        let type_env = &renamed_types;
        let saved_env = std::mem::replace(&mut self.cur_env, env.clone());
        let saved_type_env = std::mem::replace(&mut self.cur_type_env, type_env.clone());
        self.lower_stack.push(entity_id);
        // Ports (struct/array-typed ones flatten to leaves), then the port map.
        // An `inout` port aliased to a parent net reuses that net's signal
        // instead of allocating its own: the body's `pin = expr` then drives the
        // shared net (resolving across instances) and reads of `pin` read the
        // resolved value — Verilog's bidirectional-port model.
        for p in &edecl.ports {
            self.add_typed_signal(path, &p.name.text, &p.ty, env, p.span);
        }
        // An aliased `inout` port repoints its (leaf) name at the shared parent
        // net (keeping the type metadata just registered), so the body drives and
        // reads that net directly. The port's own allocated signal is left
        // unused. A scalar port aliases one name (`s`); a struct/array `inout`
        // port aliases each flattened leaf (`s.valid`, `s.data`).
        for (name, &net) in aliases {
            if self.locals.contains_key(name) {
                self.locals.insert(name.clone(), net);
            }
        }
        // The port map. A scalar port is one entry (`s`); a struct/array port
        // flattens to one entry per leaf (`s.valid`, `s.data`, `bus[0]`), each
        // tagged with the port's direction, so a parent can wire every leaf.
        // (Only port signals exist in `locals` at this point — `let` state
        // signals are added below — so the prefix scan can't catch a non-port.)
        let mut ports: HashMap<String, (SignalId, Option<ast::Direction>)> = HashMap::new();
        let mut new_out_ports: Vec<SignalId> = Vec::new();
        for p in &edecl.ports {
            let dot = format!("{}.", p.name.text);
            let idx = format!("{}[", p.name.text);
            // An applied-view port (`bus: Source Stream`) gives each leaf its
            // direction from the view (`out valid; in ready;`); a plain port
            // applies its single direction to every leaf.
            let view = self.view_of(&p.ty).and_then(|k| self.view_dirs.get(&k));
            for (k, &id) in &self.locals {
                if *k == p.name.text || k.starts_with(&dot) || k.starts_with(&idx) {
                    let dir = match view {
                        Some(m) => k
                            .strip_prefix(&dot)
                            .and_then(|field| m.get(field).copied())
                            .or(p.dir),
                        None => p.dir,
                    };
                    // A plain (non-bus-mode) `out` port must be driven inside the
                    // entity; record it for the undriven check. Bus-mode leaves
                    // and `inout` are excluded (their drive model differs).
                    if !edecl.is_extern && view.is_none() && dir == Some(ast::Direction::Out) {
                        new_out_ports.push(id);
                    }
                    ports.insert(k.clone(), (id, dir));
                }
            }
        }
        self.plain_out_ports.extend(new_out_ports);

        // `let` items: instance bindings are collected for recursion; the rest
        // become state signals.
        let impls: Vec<&ast::ImplDecl> = self.impls.get(&entity_id).cloned().unwrap_or_default();
        let mut subinsts: Vec<(String, ast::Type, Vec<ast::ConnectArg>)> = Vec::new();
        // Generate loops (`for i in 0..n { let s: Sub = { .. } }`) unroll here,
        // substituting the loop index into each instance's type args and
        // connections so the flattened element signals (`wires[i]`) resolve.
        for im in &impls {
            for item in &im.items {
                if let ast::ImplItem::Stmt(s) = item {
                    gather_generate(
                        s,
                        env,
                        &[],
                        &self.entities,
                        self.resolved,
                        &self.free_fns,
                        &mut subinsts,
                    );
                }
            }
        }
        for im in &impls {
            for item in &im.items {
                if let ast::ImplItem::Let(l) = item {
                    // `let s: Sub = { .. }` / `let s: Sub [= { .. }]`: a
                    // sub-instance, not a signal. A `let s: T` whose `T` is
                    // *this* entity's type parameter (bound to a concrete type,
                    // e.g. `unsigned[8]`) is a signal even when some entity is also
                    // named `T` — let it fall through to the signal path, where
                    // `add_typed_signal` substitutes `T` via `cur_type_env`.
                    if let Some((cty, args)) = instance_let_parts(l, &self.entities, self.resolved)
                    {
                        let is_type_param =
                            type_head_name(&cty).is_some_and(|h| self.cur_type_env.contains_key(h));
                        if !is_type_param {
                            subinsts.push((l.name.text.clone(), cty, args));
                            continue;
                        }
                    }
                    // `let s: string = "hello";`: the literal sets the range.
                    let unconstrained = match &l.ty {
                        None => true,
                        Some(t) => matches!(
                            self.resolve_alias(t),
                            ast::Type::Indexed { index: None, .. }
                        ),
                    };
                    if unconstrained {
                        if let Some(ast::Expr::StrLit { text, .. }) = &l.value {
                            self.add_char_array(path, &l.name.text, text.chars().count(), l.span);
                            continue;
                        }
                        // `let s: string = read<string>("f.txt");` — the
                        // compiler reads UTF-8; its code-point length sets the
                        // otherwise unconstrained range.
                        if let Some((requested, fpath)) =
                            l.value.as_ref().and_then(Self::fs_read_call)
                        {
                            if self.type_resolves_to(requested, "string") {
                                match std::fs::read_to_string(self.base_dir.join(fpath)) {
                                    Ok(text) => {
                                        let chars: Vec<char> = text.chars().collect();
                                        self.add_char_array(
                                            path,
                                            &l.name.text,
                                            chars.len(),
                                            l.span,
                                        );
                                        for (i, c) in chars.iter().enumerate() {
                                            if let Some(&id) =
                                                self.locals.get(&format!("{}[{i}]", l.name.text))
                                            {
                                                self.out.signals[id.0 as usize].init =
                                                    vec![*c as u32 as u64];
                                            }
                                        }
                                    }
                                    Err(e) => self.sink.emit(
                                        crate::diag::Diagnostic::error(format!(
                                            "read<string>(\"{fpath}\"): {e}"
                                        ))
                                        .with_code(crate::diag::codes::COMPILE_TIME_IO)
                                        .at(l.span),
                                    ),
                                }
                                continue;
                            }
                        }
                    }
                    if let Some(ty) = &l.ty {
                        self.add_typed_signal(path, &l.name.text, ty, env, l.span);
                    } else {
                        self.add_signal(path, &l.name.text, 0, l.span);
                    }
                    // A string initializer on a flattened array
                    // (`let arr: Color[3] = "rgb"`) seeds each element: a
                    // char-enum variant, or a `Char` code point.
                    if let Some(ast::Expr::StrLit { text, .. }) = &l.value {
                        if let Some(indices) = self.local_array.get(&l.name.text).cloned() {
                            for (c, i) in text.chars().zip(&indices) {
                                if let Some(&id) = self.locals.get(&format!("{}[{i}]", l.name.text))
                                {
                                    let en = self.out.signals[id.0 as usize].enum_type.clone();
                                    let v = en
                                        .and_then(|e| self.char_disc(c, &e))
                                        .unwrap_or(c as u32 as u64);
                                    self.out.signals[id.0 as usize].init = vec![v];
                                }
                            }
                        }
                    }
                    // A struct-literal initializer (`let p: P = { .a = 1 }`)
                    // seeds each field signal. The testbench interpreter has
                    // always honoured this; hardware lowering did not, so an
                    // entity-level struct local silently powered on at 0.
                    if let Some(ast::Expr::Construct { args, spread, .. }) = &l.value {
                        let head = l.ty.as_ref().and_then(type_head_name).map(str::to_string);
                        self.seed_struct_literal(
                            &l.name.text,
                            head.as_deref(),
                            args,
                            spread.as_deref(),
                            l.span,
                        );
                    } else if self.local_struct.contains_key(&l.name.text) {
                        // A struct local initialized by anything else. The
                        // scalar fold below never sees these: it is reached
                        // through `locals[name]`, and a struct has signals only
                        // under `name.field`. So `let p: Pair = make(6)` seeded
                        // nothing and every field powered on at zero, silently.
                        if let Some(value) = &l.value {
                            let head = l.ty.as_ref().and_then(type_head_name).map(str::to_string);
                            match self.struct_literal_from_call(value) {
                                Some((fields, spread)) => self.seed_struct_literal(
                                    &l.name.text,
                                    head.as_deref(),
                                    &fields,
                                    spread.as_ref(),
                                    l.span,
                                ),
                                // A default construction (`Pair::new()`,
                                // `Pair()`) names no declared function, and its
                                // structural zeros are the right answer — the
                                // one shape here that must stay silent.
                                None if Self::is_default_construction(value)
                                    && !self.resolves_to_declared_fn(value) => {}
                                // Everything else has no power-on value this
                                // can fold: a body that is not one returned
                                // literal, or a read of another signal. Say so
                                // rather than powering on at zero — the help
                                // names the spelling that works. This matches
                                // the scalar rule exactly, where even a copy
                                // from a constant-initialized local
                                // (`let b: unsigned[8] = a`) is E-P021: an
                                // initializer folds constants, and reading a
                                // signal is not folding.
                                // A whole struct constant (`let p: Pair = K`)
                                // *is* a constant, and the scalar spelling of
                                // it folds, so this seeds rather than reports.
                                None if self.seed_from_struct_const(&l.name.text, value) => {}
                                // A positional literal (`let p: Pair = { 6, 7 }`)
                                // is the named form without the field names.
                                None if head
                                    .as_deref()
                                    .and_then(|h| self.positional_struct_args(h, value))
                                    .is_some() =>
                                {
                                    let args = head
                                        .as_deref()
                                        .and_then(|h| self.positional_struct_args(h, value))
                                        .unwrap_or_default();
                                    self.seed_struct_literal(
                                        &l.name.text,
                                        head.as_deref(),
                                        &args,
                                        None,
                                        l.span,
                                    );
                                }
                                None => {
                                    self.report_non_constant_init(&l.name.text, l.span);
                                }
                            }
                        }
                    }
                    // An array-literal initializer (`let rom: unsigned[8][4] =
                    // [1, 2, 3, 4]`) seeds each element, as the string and
                    // struct-literal forms above do. Without it a lookup table
                    // written this way powered on at 0 in every element and
                    // read back as zeros with no diagnostic.
                    // This used to walk the elements itself, with `enumerate`
                    // for the index and no case for an element that is a
                    // struct. `seed_elements` is the same walk done once: it
                    // takes the indices from the declared range (so a
                    // non-zero-based array seeds the right elements) and seeds
                    // an aggregate element through the struct path.
                    if let Some(ast::Expr::Array { elems, .. }) = &l.value {
                        let name = l.name.text.clone();
                        self.seed_elements(&name, elems.iter().collect(), l.span);
                    }
                    // A value-less internal `let` in a component entity must be
                    // driven; record its leaves for the undriven check. Root
                    // Root entities are excluded: their wires are stimulus fed
                    // externally, so they are not forgotten internal drives.
                    // An instance array (`let stage: Inc[N]`, Inc an entity) is
                    // built element-wise, not driven — never a signal to check.
                    let is_instance_array = l.ty.as_ref().is_some_and(|ty| {
                        type_def_id(ty, self.resolved)
                            .is_some_and(|id| self.entities.contains_key(&id))
                            && !type_head_name(ty)
                                .is_some_and(|head| self.cur_type_env.contains_key(head))
                    });
                    if is_instance_array {
                        self.instance_arrays.insert(l.name.text.clone());
                    }
                    if l.value.is_none() && !is_instance_array && !is_root {
                        let dot = format!("{}.", l.name.text);
                        let idx = format!("{}[", l.name.text);
                        let leaves: Vec<SignalId> = self
                            .locals
                            .iter()
                            .filter(|(k, _)| {
                                **k == l.name.text || k.starts_with(&dot) || k.starts_with(&idx)
                            })
                            .map(|(_, &id)| id)
                            .collect();
                        self.undriven_lets.extend(leaves);
                    }
                    if !is_instance_array && !is_root {
                        let dot = format!("{}.", l.name.text);
                        let idx = format!("{}[", l.name.text);
                        self.unused_lets.extend(
                            self.locals
                                .iter()
                                .filter(|(k, _)| {
                                    **k == l.name.text || k.starts_with(&dot) || k.starts_with(&idx)
                                })
                                .map(|(_, &id)| id),
                        );
                    }
                    // A typed file constructor is owned by elaboration here:
                    // text decodes UTF-8 into Char leaves, while binary packs
                    // little-endian integers and then stores them through the
                    // requested destination representation.
                    if let Some((requested, fpath)) = l.value.as_ref().and_then(Self::fs_read_call)
                    {
                        if self.type_resolves_to(requested, "string") {
                            if let Some(indices) = self.local_array.get(&l.name.text).cloned() {
                                match std::fs::read_to_string(self.base_dir.join(fpath)) {
                                    Ok(text) => {
                                        let chars = text.chars().collect::<Vec<_>>();
                                        if chars.len() > indices.len() {
                                            self.sink.emit(
                                                crate::diag::Diagnostic::error(format!(
                                                    "read<string>(\"{fpath}\"): {} characters do not fit `{}` ({} elements)",
                                                    chars.len(),
                                                    l.name.text,
                                                    indices.len()
                                                ))
                                                .with_code(crate::diag::codes::COMPILE_TIME_IO)
                                                .at(l.span),
                                            );
                                        }
                                        for (position, index) in indices.iter().enumerate() {
                                            if let Some(&id) = self
                                                .locals
                                                .get(&format!("{}[{index}]", l.name.text))
                                            {
                                                self.out.signals[id.0 as usize].init = vec![chars
                                                    .get(position)
                                                    .copied()
                                                    .map(|character| character as u32 as u64)
                                                    .unwrap_or(0)];
                                            }
                                        }
                                    }
                                    Err(error) => self.sink.emit(
                                        crate::diag::Diagnostic::error(format!(
                                            "read<string>(\"{fpath}\"): {error}"
                                        ))
                                        .with_code(crate::diag::codes::COMPILE_TIME_IO)
                                        .at(l.span),
                                    ),
                                }
                            }
                            continue;
                        }

                        let targets = self
                            .local_array
                            .get(&l.name.text)
                            .map(|indices| {
                                indices
                                    .iter()
                                    .filter_map(|index| {
                                        self.locals
                                            .get(&format!("{}[{index}]", l.name.text))
                                            .copied()
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .or_else(|| self.locals.get(&l.name.text).copied().map(|id| vec![id]))
                            .unwrap_or_default();
                        match std::fs::read(self.base_dir.join(fpath)) {
                            Ok(bytes) if !targets.is_empty() => {
                                let element_width =
                                    self.out.signals[targets[0].0 as usize].width.max(1);
                                let element_bytes = element_width.div_ceil(8) as usize;
                                let capacity = element_bytes.saturating_mul(targets.len());
                                if bytes.len() > capacity {
                                    self.sink.emit(
                                        crate::diag::Diagnostic::error(format!(
                                            "read<{}>(\"{fpath}\"): {} bytes do not fit `{}` ({} elements x {element_bytes} bytes)",
                                            crate::syntax::pretty::type_str(requested),
                                            bytes.len(),
                                            l.name.text,
                                            targets.len()
                                        ))
                                        .with_code(crate::diag::codes::COMPILE_TIME_IO)
                                        .at(l.span),
                                    );
                                }
                                for (position, id) in targets.into_iter().enumerate() {
                                    self.out.signals[id.0 as usize].init = file_integer_words(
                                        &bytes,
                                        position.saturating_mul(element_bytes),
                                        element_bytes,
                                        element_width,
                                    );
                                }
                            }
                            Ok(_) => {}
                            Err(error) => self.sink.emit(
                                crate::diag::Diagnostic::error(format!(
                                    "read<{}>(\"{fpath}\"): {error}",
                                    crate::syntax::pretty::type_str(requested)
                                ))
                                .with_code(crate::diag::codes::COMPILE_TIME_IO)
                                .at(l.span),
                            ),
                        }
                        continue;
                    }
                    // A constant initializer is the signal's reset value.
                    if let (Some(v), Some(&id)) = (&l.value, self.locals.get(&l.name.text)) {
                        let en = self.out.signals[id.0 as usize].enum_type.clone();
                        let is_char = self.out.signals[id.0 as usize].char;
                        if let Some(bits) = self.const_init_value(v, en.as_deref(), is_char) {
                            let w = self.out.signals[id.0 as usize].width;
                            let masked = if w > 0 && w < 64 {
                                bits & ((1u64 << w) - 1)
                            } else {
                                bits
                            };
                            self.out.signals[id.0 as usize].init = vec![masked];
                        } else {
                            self.report_non_constant_init(&l.name.text, l.span);
                        }
                        // A metavalue-carrying string init (`"01X0"`) needs a
                        // companion signal to record which elements are `'X'`/… —
                        // the storage half of X/Z vector propagation (stage 1c).
                        if let Some((base, digits)) = Self::bit_string_parts(v) {
                            let (value_words, discs) = self.decode_bit_string_words(base, digits);
                            if value_words.len() > 1 {
                                self.out.signals[id.0 as usize].init = value_words;
                            }
                            if self.has_metavalue(&discs) {
                                self.ensure_meta_companion(id, discs);
                            }
                        }
                    }
                }
            }
        }

        // Sub-instances: lower each under `path.inst`, then wire its ports. An
        // `in` port is driven from the parent's signal; an `out` port drives the
        // parent's. The recursion saves/restores this body's scope, so the
        // parent's names resolve again here.
        for (inst, cty, conns) in &subinsts {
            let Some(sub_id) = type_def_id(cty, self.resolved) else {
                continue;
            };
            // Cyclic instantiation (already diagnosed by the elaborator): don't
            // recurse back into an entity that is still being lowered.
            if self.lower_stack.contains(&sub_id) {
                continue;
            }
            let sub_path = format!("{path}.{inst}");
            let mut sub_env = self.consts.clone();
            sub_env.extend(self.construct_params(cty, sub_id, env));
            let sub_type_env = self.construct_type_params(cty, sub_id);

            // Resolve `inout` connections to the parent net they share *before*
            // lowering the child, so its port aliases to that net. A scalar
            // inout whose parent side isn't a plain signal is left un-aliased
            // (falls back to the in/out wiring below).
            // Normalized `(port, value)` connections (positional bound to port
            // order), used both for inout aliasing and the wiring below.
            let norm = self.norm_conns(conns, sub_id);
            let mut aliases: HashMap<String, SignalId> = HashMap::new();
            if let Some(decl) = self.entities.get(&sub_id).copied() {
                for p in &decl.ports {
                    if p.dir != Some(ast::Direction::Inout) {
                        continue;
                    }
                    let value = norm
                        .iter()
                        .find(|(port, _)| *port == p.name.text)
                        .map(|(_, v)| v.clone());
                    let Some(value) = value else { continue };
                    // Scalar inout: the whole port shares the parent net.
                    if let Some(net) = self.target_signal(&value) {
                        aliases.insert(p.name.text.clone(), net);
                    }
                    // Struct/array inout: alias each leaf of the connected net
                    // (`link.valid`, `bus[0]`) onto the matching port leaf
                    // (`s.valid`, `pin[0]`), so every leaf resolves across the
                    // instances through the shared net.
                    if let Some(net_path) = expr_path(&value) {
                        let dot = format!("{net_path}.");
                        let idx = format!("{net_path}[");
                        for (k, &id) in &self.locals {
                            if let Some(rest) = k.strip_prefix(&dot) {
                                aliases.insert(format!("{}.{}", p.name.text, rest), id);
                            } else if let Some(rest) = k.strip_prefix(&idx) {
                                aliases.insert(format!("{}[{}", p.name.text, rest), id);
                            }
                        }
                    }
                }
            }

            let sub_ports =
                self.lower_body(sub_id, &sub_path, &sub_env, &sub_type_env, &aliases, false);
            // Expose the sub-instance's ports in this scope so `inst.port`
            // (and `stage[i].port`) reads resolve to the child's signal —
            // an output need not be wired to a local to be read.
            for (port, &(sig, _)) in &sub_ports {
                self.locals.entry(format!("{inst}.{port}")).or_insert(sig);
            }
            for (field, value) in &norm {
                let field = field.as_str();
                // The child port's leaves: the port itself (`s`) plus any
                // flattened struct/array members (`s.valid`, `bus[0]`).
                let dot = format!("{field}.");
                let idx = format!("{field}[");
                let mut leaves: Vec<(String, SignalId, Option<ast::Direction>)> = sub_ports
                    .iter()
                    .filter(|(k, _)| **k == *field || k.starts_with(&dot) || k.starts_with(&idx))
                    .map(|(k, &(id, d))| (k.clone(), id, d))
                    .collect();
                if leaves.is_empty() {
                    continue;
                }

                // A scalar port (one leaf named exactly `field`): the connection
                // value may be any expression (`.en = ea`, `.val = 5`).
                if leaves.len() == 1 && leaves[0].0 == *field {
                    let (_, child_id, dir) = leaves[0];
                    // An aliased inout is already wired to the shared net.
                    if dir == Some(ast::Direction::Inout) && aliases.contains_key(field) {
                        continue;
                    }
                    if dir == Some(ast::Direction::Out) {
                        if let Some(target) = self.target_signal(value) {
                            let ctx = self.next_ctx_at(ast::expr_span(value));
                            self.out.drivers.push(Driver {
                                span: self.cur_span,
                                target,
                                cond: None,
                                expr: Expr::Current(child_id),
                                meta: None,
                                ctx,
                            });
                        }
                    } else {
                        let expr = self.lower_expr(value);
                        let ctx = self.next_ctx_at(ast::expr_span(value));
                        self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: child_id,
                            cond: None,
                            expr,
                            meta: None,
                            ctx,
                        });
                    }
                    continue;
                }

                // A composite (struct/array) port connected to a *literal*
                // has no parent signal to wire leaf-by-leaf, so drive each
                // leaf from the matching field. Without this the whole
                // connection was dropped in silence and the port kept its
                // default — a scalar port has always accepted a value here.
                if expr_path(value).is_none() {
                    if let ast::Expr::Construct { args, .. } = value {
                        let mut fields: HashMap<String, &ast::Expr> = HashMap::new();
                        literal_leaves(args, "", &mut fields);
                        for (k, child_id, dir) in &leaves {
                            if *dir == Some(ast::Direction::Out) {
                                continue;
                            }
                            let Some(field_value) = fields.get(&k[field.len()..]) else {
                                continue;
                            };
                            let expr = self.lower_expr(field_value);
                            let ctx = self.next_ctx_at(ast::expr_span(field_value));
                            self.out.drivers.push(Driver {
                                span: self.cur_span,
                                target: *child_id,
                                cond: None,
                                expr,
                                meta: None,
                                ctx,
                            });
                        }
                    }
                    // An *array* literal on a composite port (`.v = [1, 9]`)
                    // drives one leaf per element. Only the struct form was
                    // handled, so this connection was dropped in silence and
                    // the child read its default — a scalar port has always
                    // accepted a literal here.
                    if let ast::Expr::Array { elems, .. } = value {
                        // Declared index order, numerically: `v[10]` must not
                        // sort before `v[2]`.
                        let mut elements: Vec<(i64, SignalId, Option<ast::Direction>)> = leaves
                            .iter()
                            .filter_map(|(k, id, dir)| {
                                let rest = k.strip_prefix(&idx)?;
                                let index = rest.strip_suffix(']')?.parse::<i64>().ok()?;
                                Some((index, *id, *dir))
                            })
                            .collect();
                        elements.sort_by_key(|(index, _, _)| *index);
                        for ((_, child_id, dir), elem) in elements.iter().zip(elems) {
                            if *dir == Some(ast::Direction::Out) {
                                continue;
                            }
                            let expr = self.lower_expr(elem);
                            let ctx = self.next_ctx_at(ast::expr_span(elem));
                            self.out.drivers.push(Driver {
                                span: self.cur_span,
                                target: *child_id,
                                cond: None,
                                expr,
                                meta: None,
                                ctx,
                            });
                        }
                    }
                    continue;
                }
                // A composite (struct/array) port: wire each leaf to the matching
                // leaf of the parent signal (`.s = link` -> `s.valid`<->`link.valid`).
                // The parent side must be a signal path.
                let Some(base) = expr_path(value) else {
                    continue;
                };
                leaves.sort_by(|a, b| a.0.cmp(&b.0));
                for (k, child_id, dir) in leaves {
                    let suffix = &k[field.len()..]; // ".valid", "[0]"
                    let Some(&parent_id) = self.locals.get(&format!("{base}{suffix}")) else {
                        continue;
                    };
                    // An `inout` leaf is already aliased to this parent net (same
                    // signal), so its drivers fold through `Resolve` directly —
                    // wiring it again would make a self-driver.
                    if parent_id == child_id {
                        continue;
                    }
                    let ctx = self.next_ctx_at(ast::expr_span(value));
                    if dir == Some(ast::Direction::Out) {
                        self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: parent_id,
                            cond: None,
                            expr: Expr::Current(child_id),
                            meta: None,
                            ctx,
                        });
                    } else {
                        self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: child_id,
                            cond: None,
                            expr: Expr::Current(parent_id),
                            meta: None,
                            ctx,
                        });
                    }
                }
            }
        }

        // Behaviour: statements in one explicit process share a driver
        // context (source-order override); processes and bare concurrent
        // statements receive separate contexts (parallel-driver resolution).
        for im in &impls {
            for item in &im.items {
                match item {
                    ast::ImplItem::Process(process) => {
                        self.lint_generated_dead_assignments(process.body.stmts.iter());
                        self.cur_ctx += 1;
                        if let Some(name) = &process.name {
                            self.out.process_labels.insert(
                                self.cur_ctx,
                                format!("{}::{}", self.cur_instance_path, name.text),
                            );
                        }
                        for statement in &process.body.stmts {
                            self.lower_stmt(statement, None);
                        }
                    }
                    ast::ImplItem::Stmt(statement) => {
                        self.cur_ctx += 1;
                        self.lower_stmt(statement, None);
                    }
                    _ => {}
                }
            }
        }

        // Restore the caller's scope.
        self.lower_stack.pop();
        self.locals = saved_locals;
        self.local_enum = saved_enum;
        self.local_struct = saved_struct;
        self.local_struct_repr = saved_struct_repr;
        self.local_char = saved_char;
        self.local_array = saved_array;
        self.local_numeric = saved_numeric;
        self.instance_arrays = saved_instance_arrays;
        self.cur_env = saved_env;
        self.cur_type_env = saved_type_env;
        self.consts = saved_consts;
        self.const_values = saved_const_values;
        self.cur_instance_path = saved_instance_path;
        ports
    }

    /// Concrete parameter bindings written on an instance type
    /// (`Counter<W = 8>`, or positionally as `Counter<8>`).
    ///
    /// Positional args bind the declaration's parameters in order, matching
    /// `construct_type_params` — each takes the ones it owns, value params
    /// here and bare type params there. Dropping the positional form left the
    /// parameter unbound, which surfaced far downstream as "signal has unknown
    /// width (0)" rather than anything pointing at the instance.
    fn construct_params(
        &self,
        ty: &ast::Type,
        entity_id: DefId,
        env: &HashMap<String, i64>,
    ) -> HashMap<String, i64> {
        let mut out = HashMap::new();
        let ast::Type::Generic { args, .. } = ty else {
            return out;
        };
        let decl = self.entities.get(&entity_id);
        for (i, a) in args.iter().enumerate() {
            match a {
                ast::GenericArg::Named { name, value } => {
                    if let Some(v) = self.eval_const(value, env) {
                        out.insert(name.text.clone(), v);
                    }
                }
                ast::GenericArg::Positional(e) => {
                    let Some(p) = decl.and_then(|d| d.params.params.get(i)) else {
                        continue;
                    };
                    // A bare type param is `construct_type_params`' business.
                    if p.bound.is_none() {
                        continue;
                    }
                    if let Some(v) = self.eval_const(e, env) {
                        out.insert(p.name.text.clone(), v);
                    }
                }
                ast::GenericArg::PositionalType(_) | ast::GenericArg::NamedType { .. } => {}
            }
        }
        out
    }

    /// Type-parameter bindings for a generic entity instance (`Buf<unsigned[8]>` ->
    /// `T -> unsigned[8]`): the entity's bare type params (bound `None`), matched to
    /// the construct's generic args positionally or by name.
    fn construct_type_params(
        &self,
        ty: &ast::Type,
        entity_id: DefId,
    ) -> HashMap<String, ast::Type> {
        let mut out = HashMap::new();
        let (Some(decl), ast::Type::Generic { args, .. }) = (self.entities.get(&entity_id), ty)
        else {
            return out;
        };
        let type_params: Vec<&ast::Param> = decl
            .params
            .params
            .iter()
            .filter(|p| p.bound.is_none())
            .collect();
        for (i, a) in args.iter().enumerate() {
            match a {
                ast::GenericArg::Named { name, value } => {
                    if type_params.iter().any(|p| p.name.text == name.text) {
                        if let Some(t) = expr_to_type(value) {
                            out.insert(name.text.clone(), t);
                        }
                    }
                }
                ast::GenericArg::NamedType { name, ty } => {
                    if type_params.iter().any(|p| p.name.text == name.text) {
                        out.insert(name.text.clone(), ty.clone());
                    }
                }
                ast::GenericArg::Positional(e) => {
                    if let (Some(p), Some(t)) = (decl.params.params.get(i), expr_to_type(e)) {
                        if p.bound.is_none() {
                            out.insert(p.name.text.clone(), t);
                        }
                    }
                }
                ast::GenericArg::PositionalType(ty) => {
                    if let Some(p) = decl.params.params.get(i) {
                        if p.bound.is_none() {
                            out.insert(p.name.text.clone(), ty.clone());
                        }
                    }
                }
            }
        }
        out
    }

    /// Spec 3.14 + Resolve: a signal driven from several contexts folds each
    /// context's contribution (its override chain over a 'Z' base) through
    /// the type's `Resolve` impl; a type without one is unresolved, and
    /// parallel drivers are an elaboration error.
    fn resolve_driver_contexts(&mut self) {
        use std::collections::BTreeMap;
        // target -> ctx -> ordered driver indices
        let mut by_target: BTreeMap<u32, BTreeMap<u32, Vec<usize>>> = BTreeMap::new();
        for (i, d) in self.out.drivers.iter().enumerate() {
            by_target
                .entry(d.target.0)
                .or_default()
                .entry(d.ctx)
                .or_default()
                .push(i);
        }
        let mut replaced: Vec<(u32, Expr, Option<Expr>, Vec<String>)> = Vec::new();
        for (t, ctxs) in &by_target {
            // Metavalue companions are an implementation plane of their parent
            // signal. The parent's element-wise Resolve replaces their drivers
            // together; diagnosing the temporary per-context companion drivers
            // as an independent unresolved net would be a false conflict.
            if self.out.meta_of.values().any(|companion| companion == t) {
                continue;
            }
            if ctxs.len() < 2 {
                continue;
            }
            let ty = self.sig_type.get(t).cloned().unwrap_or_default();
            let direct_resolve = self
                .op_impls
                .contains_key(&("Resolve".to_string(), ty.clone()));
            let element_resolve = self
                .out
                .array_element_enums
                .get(t)
                .filter(|element| {
                    self.blanket_array_impls
                        .get("Resolve")
                        .is_some_and(|requirement| {
                            self.op_impls
                                .contains_key(&(requirement.clone(), (*element).clone()))
                        })
                })
                .cloned();
            let has_resolve = direct_resolve || element_resolve.is_some();
            let path = self.out.signals[*t as usize].path.clone();
            let declaration_span = self.out.signals[*t as usize].declaration_span;
            let mut labels: Vec<String> = ctxs
                .keys()
                .filter_map(|context| self.out.process_labels.get(context).cloned())
                .collect();
            labels.sort();
            labels.dedup();
            if !has_resolve {
                // Lead with the mistake (several sources driving one signal),
                // not its symptom (a missing `Resolve` impl) — the usual cause
                // is a miswired bus, e.g. two producers on one net. Point at
                // each contributing connection when we know where it came from.
                let sites: Vec<crate::diag::Span> = ctxs
                    .keys()
                    .filter_map(|c| self.ctx_span.get(c).copied())
                    .collect();
                let mut d = crate::diag::Diagnostic::error(format!(
                    "`{path}` is driven by {} conflicting sources",
                    ctxs.len()
                ))
                .with_code(crate::diag::codes::CONFLICTING_DRIVERS);
                if let Some((first, rest)) = sites.split_first() {
                    d = d.at(*first);
                    for (i, s) in rest.iter().enumerate() {
                        d = d.label(*s, format!("conflicting source {}", i + 2));
                    }
                    d = d.label(declaration_span, "signal declared here");
                } else {
                    d = d.at(declaration_span);
                }
                self.sink.emit(d.help(format!(
                    "only one source may drive `{path}`; a bus needs converse \
                     endpoints (one side driving each leaf). To have several \
                     drivers fold instead, `{ty}` needs an `impl Resolve` (as \
                     `Logic` has)"
                )));
                continue;
            }
            // A forwarded array Resolve operates per element and preserves the
            // separate value/discriminant planes.
            if let Some(element) = element_resolve {
                let width = self.out.signals[*t as usize].width;
                // Folding unrolls per element, so an operand it repeats is
                // hoisted rather than deep-copied `width` times. Nothing
                // between the arm and the flush creates a signal.
                self.arm_meta_temps(0, declaration_span);
                let folded = self.resolve_vector_contexts(ctxs, width, &element);
                self.flush_meta_temps();
                if let Some((value, meta)) = folded {
                    replaced.push((*t, value, Some(meta), labels));
                } else {
                    self.sink.emit(
                        crate::diag::Diagnostic::error(format!(
                            "could not instantiate element-wise `impl Resolve for {element}[]` folding `{path}`"
                        ))
                        .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
                        .at(declaration_span)
                        .help(
                            "the element Resolve implementation must have a finite hardware form",
                        ),
                    );
                }
                continue;
            }

            // Each context: fold its drivers (later overrides) over a 'Z' base.
            let neutral = self
                .logic_encoding(&ty)
                .and_then(LogicEncoding::high_impedance_value)
                .or_else(|| self.new_defaults.get(&ty).copied())
                .unwrap_or(0);
            let mut contributions = Vec::new();
            for idxs in ctxs.values() {
                let mut acc = Expr::Const(neutral);
                for &i in idxs {
                    let d = &self.out.drivers[i];
                    acc = match &d.cond {
                        None => d.expr.clone(),
                        Some(c) => Expr::Select {
                            cond: Box::new(c.clone()),
                            then: Box::new(d.expr.clone()),
                            els: Box::new(acc),
                        },
                    };
                }
                contributions.push(acc);
            }
            // Pairwise resolve through a compact source-derived table for a
            // logic enum, or through the ordinary inlined impl for any other
            // user type.
            let mut it = contributions.into_iter();
            let mut folded = it.next().unwrap();
            for c in it {
                let resolved = self
                    .logic_encoding(&ty)
                    .and_then(|encoding| encoding.binary_ops.get("resolve"))
                    .map(|table| logic_binary_table_result(folded.clone(), c.clone(), table))
                    .or_else(|| self.inline_resolve(&ty, folded.clone(), c));
                match resolved {
                    Some(r) => folded = r,
                    None => {
                        self.sink.emit(
                            crate::diag::Diagnostic::error(format!(
                                "could not inline `impl Resolve for {ty}` folding `{path}`"
                            ))
                            .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
                            .at(declaration_span)
                            .help("the Resolve implementation must have a finite hardware form"),
                        );
                        break;
                    }
                }
            }
            replaced.push((*t, folded, None, labels));
        }
        for (t, expr, meta, labels) in replaced {
            self.out.drivers.retain(|d| d.target.0 != t);
            if !labels.is_empty() {
                self.out.resolved_process_labels.insert(t, labels);
            }
            self.out.drivers.push(Driver {
                span: self.cur_span,
                target: SignalId(t),
                cond: None,
                expr,
                meta,
                ctx: 0,
            });
        }
    }

    /// Fold the drivers contributing to one signal through its type's `Resolve`
    /// impl, per element, rather than as one whole-vector expression.
    fn resolve_vector_contexts(
        &self,
        contexts: &std::collections::BTreeMap<u32, Vec<usize>>,
        width: u32,
        element: &str,
    ) -> Option<(Expr, Expr)> {
        let encoding = self.logic_encoding(element)?;
        let z = encoding.high_impedance_value()?;
        let mut contributions = Vec::new();
        for indices in contexts.values() {
            let mut value = Expr::Const(encoding.value_bit(z)?);
            let mut meta = Expr::Const(if encoding.binary.contains(&z) { 0 } else { z });
            value = repeat_element_plane(value, width, 1);
            meta = repeat_element_plane(meta, width, 4);
            for &index in indices {
                let driver = &self.out.drivers[index];
                let next_value = driver.expr.clone();
                let next_meta = driver
                    .meta
                    .clone()
                    .or_else(|| {
                        let mut temps = self.meta_temps.borrow_mut();
                        self.lower_meta_ir(&driver.expr, width, &mut temps)
                    })
                    .unwrap_or(Expr::Const(0));
                match &driver.cond {
                    None => {
                        value = next_value;
                        meta = next_meta;
                    }
                    Some(condition) => {
                        value = Expr::Select {
                            cond: Box::new(condition.clone()),
                            then: Box::new(next_value),
                            els: Box::new(value),
                        };
                        meta = Expr::Select {
                            cond: Box::new(condition.clone()),
                            then: Box::new(next_meta),
                            els: Box::new(meta),
                        };
                    }
                }
            }
            contributions.push((value, meta));
        }

        // The per-element loop below reads every contribution's value and
        // discriminant plane once per element, so an inline contribution is
        // deep-copied `width` times -- which is what made a resolved
        // multi-driver signal grow as `width^2` even after the operand metas
        // were hoisted. Bind each plane once and let the unroll read a leaf.
        let contributions: Vec<(Expr, Expr)> = {
            let mut temps = self.meta_temps.borrow_mut();
            contributions
                .into_iter()
                .map(|(value, meta)| {
                    (
                        materialize(value, width, &mut temps),
                        materialize(meta, width * 4, &mut temps),
                    )
                })
                .collect()
        };

        let table = encoding.binary_ops.get("resolve");
        let mut value = Expr::Const(0);
        let mut meta = Expr::Const(0);
        // Resolve one element all the way across the contexts before packing
        // it back into the two vector planes. Repacking after every pair and
        // slicing that expression apart for the next context duplicated the
        // whole accumulated vector once per element and grew exponentially.
        for index in 0..width {
            let mut incoming = contributions.iter();
            let (first_value, first_meta) = incoming.next()?;
            let mut result = logic_element_disc(first_value, first_meta, index, encoding);
            for (incoming_value, incoming_meta) in incoming {
                let right = logic_element_disc(incoming_value, incoming_meta, index, encoding);
                result = match table {
                    Some(table) => logic_binary_table_result(result, right, table),
                    None => self.inline_resolve(element, result, right)?,
                };
            }
            let value_bit = logic_value_bit(result.clone(), encoding);
            value = or_expr(
                value,
                Expr::Binary {
                    op: BinOp::Shl,
                    lhs: Box::new(value_bit),
                    rhs: Box::new(Expr::Const(index as u64)),
                },
            );
            let is_meta = not1(logic_disc_in(result.clone(), &encoding.binary));
            let nibble = Expr::Select {
                cond: Box::new(is_meta),
                then: Box::new(result),
                els: Box::new(Expr::Const(0)),
            };
            meta = or_expr(
                meta,
                Expr::Binary {
                    op: BinOp::Shl,
                    lhs: Box::new(nibble),
                    rhs: Box::new(Expr::Const((4 * index) as u64)),
                },
            );
        }
        Some((value, meta))
    }

    /// Inline `impl Resolve for <ty>` over two already-lowered expressions.
    fn inline_resolve(&self, ty: &str, a: Expr, b: Expr) -> Option<Expr> {
        let fns = self
            .op_impls
            .get(&("Resolve".to_string(), ty.to_string()))?;
        let (f, _) = fns.first()?;
        let body = f.body.as_ref()?;
        let mut env: HashMap<String, Val> = HashMap::new();
        env.insert("self".to_string(), Val::Scalar(a));
        if let Some(p) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &p.name {
                env.insert(n.text.clone(), Val::Scalar(b));
            }
        }
        match self.inline_block(&body.stmts, &env)? {
            Val::Scalar(e) => Some(e),
            _ => None,
        }
    }

    /// A `read<T>("path")` initializer's requested type and literal path.
    fn fs_read_call(e: &ast::Expr) -> Option<(&ast::Type, &str)> {
        let ast::Expr::Call {
            callee,
            type_args,
            args,
            ..
        } = e
        else {
            return None;
        };
        let ast::Expr::Path(p) = callee.as_ref() else {
            return None;
        };
        if p.segments.len() != 1 || p.segments[0].text != "read" {
            return None;
        }
        match (type_args.as_slice(), args.as_slice()) {
            ([requested], [ast::Expr::StrLit { text, .. }]) => Some((requested, text)),
            _ => None,
        }
    }

    /// Seed a struct literal's leaves, recursing into a nested literal.
    ///
    /// `{ .p = { .x = 7 } }` names no leaf at `p` — the leaves are `p.x` and
    /// `p.y` — so the field loop used to `continue` past it and the inner
    /// values were dropped without a word, while a sibling scalar field on the
    /// same literal seeded correctly.
    /// Whether `value` is a call at all — `Pair::new()` or `Pair()`, once it is
    /// known to name no declared function, is a default construction rather
    /// than an initializer that failed to fold.
    fn is_default_construction(value: &ast::Expr) -> bool {
        matches!(value, ast::Expr::Call { .. })
    }

    /// Whether `value` is a call to a function this compilation declares —
    /// which separates "a body too complex to fold" from a default
    /// construction like `Pair::new()`, whose name resolves to no function at
    /// all and whose structural zeros are the right answer.
    fn resolves_to_declared_fn(&self, value: &ast::Expr) -> bool {
        let ast::Expr::Call { callee, .. } = value else {
            return false;
        };
        self.free_fns.get(callee).is_some()
    }

    /// The expression a call returns, with the arguments written at the call
    /// substituted for the parameters.
    ///
    /// `None` unless the callee is a declared function whose body is a single
    /// returned expression — anything else has no one expression to stand for
    /// the call, and the caller keeps its own handling.
    fn returned_expr_from_call(&self, value: &ast::Expr) -> Option<ast::Expr> {
        let ast::Expr::Call { callee, args, .. } = value else {
            return None;
        };
        let f = self.free_fns.get(callee)?;
        let [ast::Stmt::Return {
            value: Some(returned),
            ..
        }] = f.body.as_ref()?.stmts.as_slice()
        else {
            return None;
        };
        let mut bound: HashMap<String, ast::Expr> = HashMap::new();
        for (param, arg) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(name) = param.name.as_ref() {
                bound.insert(name.text.clone(), arg.clone());
            }
        }
        Some(subst_expr_paths(returned, &bound))
    }

    /// The struct literal a call returns, with the arguments written at the
    /// call substituted for the parameters — so a struct-typed `let` can seed
    /// its fields from `let p: Pair = make(6)` the way it already does from
    /// `let p: Pair = { .a = 6, .b = 7 }`.
    ///
    /// An initializer is a power-on value folded at elaboration, and the scalar
    /// path folds a call: `let a: unsigned[8] = double(6)` is 12. The struct
    /// path folded nothing, because the block that does the folding is reached
    /// only through `locals[name]` and a struct local has no signal under its
    /// bare name — only `p.a`, `p.b`. So every field powered on at zero with no
    /// diagnostic, while the same call written as a separate assignment was
    /// right.
    ///
    /// `None` when the callee is not a known function (`Pair::new()` and
    /// `Pair()` are the structural default, not a call to fold) or its body is
    /// anything but a single returned literal — the caller reports those.
    fn struct_literal_from_call(
        &self,
        value: &ast::Expr,
    ) -> Option<(Vec<ast::ConnectArg>, Option<ast::Expr>)> {
        let ast::Expr::Call { callee, args, .. } = value else {
            return None;
        };
        let f = self.free_fns.get(callee)?;
        let body = f.body.as_ref()?;
        let [ast::Stmt::Return {
            value: Some(returned),
            ..
        }] = body.stmts.as_slice()
        else {
            return None;
        };
        let ast::Expr::Construct {
            args: fields,
            spread,
            ..
        } = returned
        else {
            return None;
        };
        // Bind each declared parameter to its argument. `self` takes no
        // argument, so it is skipped rather than consuming one.
        let mut bound: HashMap<String, ast::Expr> = HashMap::new();
        for (param, arg) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(name) = param.name.as_ref() {
                bound.insert(name.text.clone(), arg.clone());
            }
        }
        let fields = fields
            .iter()
            .map(|field| ast::ConnectArg {
                field: field.field.clone(),
                value: field.value.as_ref().map(|v| subst_expr_paths(v, &bound)),
                span: field.span,
            })
            .collect();
        Some((
            fields,
            spread.as_deref().map(|s| subst_expr_paths(s, &bound)),
        ))
    }

    /// Seed a struct-typed signal's leaves from a literal, so unnamed fields
    /// take their declared defaults.
    fn seed_struct_literal(
        &mut self,
        prefix: &str,
        struct_name: Option<&str>,
        args: &[ast::ConnectArg],
        spread: Option<&ast::Expr>,
        span: crate::diag::Span,
    ) {
        // `{ ..base, .x = v }` takes every leaf from `base` first, so the
        // explicit fields below overwrite what they name. Without this the
        // fields the literal did not mention powered on at 0 rather than at
        // the base's value, while the ones it did name seeded correctly.
        if let Some(base) = spread.and_then(expr_path) {
            let from = format!("{base}.");
            let leaves: Vec<(String, SignalId)> = self
                .locals
                .iter()
                .filter(|(name, _)| name.starts_with(&from))
                .map(|(name, id)| (name.clone(), *id))
                .collect();
            for (name, src) in leaves {
                let target = format!("{prefix}.{}", &name[from.len()..]);
                if let Some(&dst) = self.locals.get(&target) {
                    let init = self.out.signals[src.0 as usize].init.clone();
                    self.out.signals[dst.0 as usize].init = init;
                }
            }
        }
        let fields: Vec<(String, ast::Type)> = struct_name
            .and_then(|h| self.structs.get(h))
            .map(|sd| {
                sd.fields
                    .iter()
                    .map(|f| (f.name.text.clone(), f.ty.clone()))
                    .collect()
            })
            .unwrap_or_default();
        for (i, arg) in args.iter().enumerate() {
            // Named (`.a = 1`) or positional (bound by declaration order).
            let field = match &arg.field {
                Some(f) => Some(f.text.clone()),
                None => fields.get(i).map(|(n, _)| n.clone()),
            };
            let (Some(field), Some(value)) = (field, arg.value.as_ref()) else {
                continue;
            };
            let path = format!("{prefix}.{field}");
            // A struct-typed field's value is read against *that field's*
            // type, so a positional literal nested inside a named one
            // (`{ .inner = { 1, 2 }, .tag = 9 }`) is a struct literal too. It
            // stayed a concat and seeded nothing.
            let field_ty = fields.iter().find(|(n, _)| *n == field).map(|(_, t)| t);
            let value = &self.as_struct_literal(field_ty, value);
            // A field whose value is itself an aggregate names no leaf of its
            // own, so each of these used to fall through the lookup below and
            // seed nothing — silently, and without even reaching the
            // non-constant check.
            match value {
                ast::Expr::Construct {
                    args: inner,
                    spread: inner_spread,
                    ..
                } => {
                    let inner_ty = fields
                        .iter()
                        .find(|(n, _)| *n == field)
                        .and_then(|(_, ty)| self.free_fns.type_head_key(ty));
                    self.seed_struct_literal(
                        &path,
                        inner_ty.as_deref(),
                        inner,
                        inner_spread.as_deref(),
                        span,
                    );
                    continue;
                }
                // `{ .arr = [4, 5, 6] }` seeds `arr[0..2]`.
                ast::Expr::Array { elems, .. } => {
                    self.seed_elements(&path, elems.iter().collect::<Vec<_>>(), span);
                    continue;
                }
                // `{ .name = "abc" }` seeds one element per character.
                ast::Expr::StrLit { text, .. } => {
                    let indices = self.local_array.get(&path).cloned().unwrap_or_default();
                    for (c, i) in text.chars().zip(&indices) {
                        if let Some(&id) = self.locals.get(&format!("{path}[{i}]")) {
                            let en = self.out.signals[id.0 as usize].enum_type.clone();
                            let v = en
                                .and_then(|e| self.char_disc(c, &e))
                                .unwrap_or(c as u32 as u64);
                            self.out.signals[id.0 as usize].init = vec![v];
                        }
                    }
                    continue;
                }
                _ => {}
            }
            // Only constants seed an init. A non-constant is *not* lowered as
            // a driver here, whatever the comment used to claim: `{ .y = src +
            // 1 }` left `p.y` at 0 for every value of `src`, and the undriven
            // lint does not reach a struct leaf, so nothing said anything.
            let Some(&id) = self.locals.get(&path) else {
                continue;
            };
            // The field's own enum type resolves a character literal
            // (`.state = 'Z'`) to its variant.
            let en = self.out.signals[id.0 as usize].enum_type.clone();
            let is_char = self.out.signals[id.0 as usize].char;
            if let Some(v) = self.const_init_value(value, en.as_deref(), is_char) {
                self.out.signals[id.0 as usize].init = vec![v];
            } else {
                self.report_non_constant_init(&path, span);
            }
        }
    }

    /// Seed each element of an array-valued field or local from a literal.
    fn seed_elements(&mut self, prefix: &str, elems: Vec<&ast::Expr>, span: crate::diag::Span) {
        let indices = self.local_array.get(prefix).cloned().unwrap_or_default();
        for (elem, i) in elems.into_iter().zip(indices) {
            let path = format!("{prefix}[{i}]");
            let Some(&id) = self.locals.get(&path) else {
                // An element that is itself a struct has no scalar leaf of its
                // own — its fields are `ps[0].a` — so the lookup above found
                // nothing and the element was skipped in silence, leaving every
                // field of every element at zero. It is a struct literal in
                // element position, read against the element's type like any
                // other.
                if let Some(struct_name) = self.local_struct.get(&path).cloned() {
                    match elem {
                        ast::Expr::Construct { args, spread, .. } => self.seed_struct_literal(
                            &path,
                            Some(&struct_name),
                            args,
                            spread.as_deref(),
                            span,
                        ),
                        _ => {
                            if let Some(args) = self.positional_struct_args(&struct_name, elem) {
                                self.seed_struct_literal(
                                    &path,
                                    Some(&struct_name),
                                    &args,
                                    None,
                                    span,
                                );
                            }
                        }
                    }
                }
                continue;
            };
            let en = self.out.signals[id.0 as usize].enum_type.clone();
            let is_char = self.out.signals[id.0 as usize].char;
            if let Some(v) = self.const_init_value(elem, en.as_deref(), is_char) {
                self.out.signals[id.0 as usize].init = vec![v];
            } else {
                self.report_non_constant_init(&path, span);
            }
        }
    }

    /// An initializer is a power-on value, folded at elaboration (spec 3.29).
    /// One that reads another signal cannot fold, and every site that seeds an
    /// init — scalar, struct field, array element — used to drop it in
    /// silence, leaving the signal at its type's default. A driver is the
    /// spelling that computes from other signals, and is a different thing:
    /// continuous rather than once.
    /// Seed a struct local's leaves from a whole struct constant
    /// (`let p: Pair = K`). `false` when `value` names no struct constant, so
    /// the caller can fall through to its diagnostic.
    fn seed_from_struct_const(&mut self, prefix: &str, value: &ast::Expr) -> bool {
        let Some(fields) = expr_path(value).and_then(|name| self.const_struct_value(&name)) else {
            return false;
        };
        for (field, folded) in fields {
            let Expr::Const(word) = folded else { continue };
            let Some(&id) = self.locals.get(&format!("{prefix}.{field}")) else {
                continue;
            };
            let width = self.out.signals[id.0 as usize].width;
            let masked = if width > 0 && width < 64 {
                word & ((1u64 << width) - 1)
            } else {
                word
            };
            self.out.signals[id.0 as usize].init = vec![masked];
        }
        true
    }

    /// A *positional* struct literal, as the named form's arguments.
    ///
    /// `{ 6, 7 }` carries no field names, so it lexes as a bit concatenation
    /// and every struct-typed use of it saw a concat where `{ .a = 6, .b = 7 }`
    /// gives a construction. Nothing bound the parts to fields, so a `let`
    /// initialized this way seeded nothing and an assignment written this way
    /// was dropped entirely — its leaves then reported as never driven.
    /// Binding part *i* to declared field *i* is what the named form means.
    ///
    /// Which reading applies is decided by the *assigned type*, not by the
    /// shape of the braces: against a struct these braces are a struct
    /// literal, against an array or packed vector they stay a concatenation.
    /// So this is keyed on the target's struct name, and a part count that
    /// does not match the field count binds what it can — the fields left
    /// unbound are then reported by the checks that already look for them.
    fn positional_struct_args(
        &self,
        struct_name: &str,
        value: &ast::Expr,
    ) -> Option<Vec<ast::ConnectArg>> {
        let ast::Expr::Concat { parts, span } = value else {
            return None;
        };
        let declared = self.structs.get(struct_name)?;
        Some(
            parts
                .iter()
                .zip(&declared.fields)
                .map(|(part, field)| ast::ConnectArg {
                    field: Some(field.name.clone()),
                    value: Some(part.clone()),
                    span: *span,
                })
                .collect(),
        )
    }

    /// `value` as a struct literal when the type it is being assigned to is a
    /// struct: a positional `{ 6, 7 }` becomes the named form, and anything
    /// else is returned unchanged.
    ///
    /// Every position that knows its destination's type goes through here — a
    /// field of an enclosing literal, an instance's port connection, a
    /// function's parameter and its return — so the one rule ("the assigned
    /// type decides how to read the braces") is applied in one way rather than
    /// re-derived per site.
    fn as_struct_literal(&self, ty: Option<&ast::Type>, value: &ast::Expr) -> ast::Expr {
        let Some(args) = ty
            .and_then(|ty| self.free_fns.type_head_key(ty))
            .and_then(|head| self.positional_struct_args(&head, value))
        else {
            return value.clone();
        };
        ast::Expr::Construct {
            ty: None,
            args,
            spread: None,
            span: ast::expr_span(value),
        }
    }

    /// A struct constant's folded fields, keyed off the dotted entries
    /// `fold_const` left in the constant table. `None` when `name` names no
    /// struct constant.
    fn const_struct_value(&self, name: &str) -> Option<Vec<(String, Expr)>> {
        let prefix = format!("{name}.");
        let mut fields: Vec<(String, Expr)> = self
            .const_values
            .iter()
            .filter_map(|(key, value)| {
                key.strip_prefix(&prefix)
                    .map(|field| (field.to_string(), value.clone()))
            })
            .collect();
        if fields.is_empty() {
            return None;
        }
        // Leaves are matched by name downstream; a stable order only keeps the
        // emitted IR reproducible.
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        Some(fields)
    }

    /// A struct constant's `(field, value)` pairs. A named argument binds by
    /// name; a positional one binds to the declared field at that position, so
    /// both spellings of a literal fold the same way. `None` when the type is
    /// not a known struct or an argument carries no value.
    fn const_struct_fields(
        &self,
        ty: &ast::Type,
        args: &[ast::ConnectArg],
    ) -> Option<Vec<(String, ast::Expr)>> {
        let declared = self.structs.get(&self.free_fns.type_head_key(ty)?)?;
        let mut out = Vec::new();
        for (position, arg) in args.iter().enumerate() {
            let field = match &arg.field {
                Some(name) => name.text.clone(),
                None => declared.fields.get(position)?.name.text.clone(),
            };
            out.push((field, arg.value.clone()?));
        }
        Some(out)
    }

    /// Report a `let` initializer that is not constant (E-P021). An initializer
    /// is a power-on value folded at elaboration, so one reading another signal
    /// cannot be honoured.
    fn report_non_constant_init(&mut self, name: &str, span: crate::diag::Span) {
        self.sink.emit(
            crate::diag::Diagnostic::error(format!(
                "the initializer for `{name}` is not a constant"
            ))
            .with_code(crate::diag::codes::NON_CONSTANT_INITIALIZER)
            .at(span)
            .help(format!(
                "an initializer is the signal's power-on value and is folded at \
                 elaboration. To compute it from other signals, drive it instead: \
                 declare `{name}` without a value, then assign it"
            )),
        );
    }

    /// The initial value of a constant `let` initializer, folded at compile
    /// time into a signal's power-on `init`. Two shapes reach here:
    /// **literals** — a real's f64 bits, or a character's position in its enum
    /// (a `'g'` has no intrinsic value; its type gives it one) — and **enum
    /// variants** — `Color::Red`, or `Bool`'s `true`/`false`, resolved to their
    /// discriminant. An integer or const-fn expression folds through
    /// `eval_const_fns`. A string initializer is a `Char` array, not a scalar,
    /// so it is written element-wise elsewhere, not here.
    fn const_init_value(&self, e: &ast::Expr, target: Option<&str>, is_char: bool) -> Option<u64> {
        match e {
            // --- literals: a value read as bits ---
            ast::Expr::Int { text, .. } if text.contains('.') => {
                text.replace('_', "").parse::<f64>().ok().map(f64::to_bits)
            }
            // A character reads by its position in the target enum (VHDL
            // `T'pos`), else std's default logic type. No value table here.
            // A `Char` target reads it through the Unicode table (its code
            // point), as `typed_char_literal` does for an operand. Only the
            // enum paths were tried here, and `Char` is a kernel type with no
            // variants, so `let c: Char = 'A';` folded to nothing — and once
            // a non-constant initializer became an error, that turned into
            // "the initializer for `c` is not a constant" on an obviously
            // constant character.
            ast::Expr::CharLit { ch, .. } if is_char => Some(*ch as u32 as u64),
            ast::Expr::CharLit { ch, .. } => target
                .and_then(|en| self.char_disc(*ch, en))
                .or_else(|| self.char_disc(*ch, DEFAULT_LOGIC_TYPE)),
            // --- enum variants: a name resolved to its discriminant ---
            // Includes `Bool`'s `true`/`false` (desugared to `Bool::true` etc.).
            ast::Expr::Path(p) if p.segments.len() >= 2 => self.enum_variant_path(p),
            // A radix bit-string initializer (`let v: unsigned[8] = x"AB"`) —
            // its value bits (metavalue positions carried separately, stage 1b).
            ast::Expr::BitStrLit { base, digits, .. } => {
                Some(self.decode_bit_string(*base, digits).0)
            }
            // A plain string on a logic-vector target reads as a logic array —
            // each character is a `std_ulogic` (no prefix needed). Only
            // reached for a single-signal (packed vector) target; a `Char[]`
            // flattens and never lands here.
            ast::Expr::StrLit { text, .. } => Some(self.decode_bit_string('b', text).0),
            // --- integer / const-fn arithmetic ---
            // A newtype constructor is value-transparent, so `let b: Byte =
            // Byte(200);` seeds 200. `eval_const_fns` has no rule for a call,
            // so the signal kept its default and read 0.
            ast::Expr::Call { callee, args, .. }
                if (match callee.as_ref() {
                    ast::Expr::Path(path) => self.free_fns.struct_path_key(path),
                    _ => None,
                })
                .and_then(|name| self.structs.get(&name).cloned())
                .is_some_and(|s| s.fields.is_empty() && s.base.is_some()) =>
            {
                self.const_init_value(args.first()?, target, is_char)
            }
            _ => eval_const_fns(e, &self.cur_env, &self.free_fns, 0).map(|v| v as u64),
        }
    }

    /// Fold each `impl New for T { fn new() -> T { return <const>; } }` to the
    /// type's uninitialized default value. Runs after `impls`/`enum_variants`
    /// are collected.
    fn compute_new_defaults(&self) -> HashMap<String, u64> {
        let mut out = HashMap::new();
        // Trait impls land in `op_impls` keyed by (trait, type); `impl New for T`
        // has a `new()` whose constant body is `T`'s uninitialized default.
        for ((tr, ty), fns) in &self.op_impls {
            if tr != "New" {
                continue;
            }
            for (f, _) in fns {
                if f.name.text != "new" {
                    continue;
                }
                if let Some(body) = &f.body {
                    for st in &body.stmts {
                        if let ast::Stmt::Return { value: Some(e), .. } = st {
                            let is_char = ty == "Char"
                                || struct_derives_kernel(ty, "Char", &self.structs, &self.free_fns);
                            if let Some(v) = self.const_init_value(e, Some(ty), is_char) {
                                out.insert(ty.clone(), v);
                            }
                        }
                    }
                }
            }
        }
        out
    }
}
