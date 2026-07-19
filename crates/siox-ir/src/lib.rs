//! Digital simulation IR for siox Phase 1 (spec Stage 6).
//!
//! Lowers the typed, elaborated design into a simulator-friendly form where
//! event dependencies and combinational dependencies are explicit, and
//! sequential next-state updates are separated from immediate local
//! assignments. `::event` and `::old` become explicit IR operations.
//!
//! Spec IR distinction:
//! ```text
//! Driver(signal, expression, condition)              // combinational
//! OnEvent(event_condition): next(signal) = expression // sequential
//! ```
//! and `Rising(clk)` lowers to
//! `Event(clk) && Old(clk) == '0' && Current(clk) == '1'`.
//!
//! The IR data types are deliberately **language-neutral** — they use their own
//! `BinOp`/`UnOp` and never reference the siox AST — so that other HDL frontends
//! could target the same IR. Only `lower` (the siox frontend lowering) consumes
//! the siox AST.
//!
//! Phase-1 scope: lowers the behaviour of each non-extern entity in the design,
//! with the entity's declared (possibly parametric) widths. Per-instance width
//! specialization and cross-instance flattening/connection are follow-ups.

use std::collections::HashMap;

use siox_diag::DiagnosticSink;
use siox_elab::Hierarchy;
use siox_syntax::ast::{self, BinOp as AstBinOp, UnOp as AstUnOp};
use siox_syntax::Module;

/// A design ready to simulate: signals, combinational drivers, and event blocks.
#[derive(Default)]
pub struct Design {
    pub signals: Vec<Signal>,
    pub drivers: Vec<Driver>,
    pub event_blocks: Vec<EventBlock>,
    /// Enum name -> (discriminant -> variant symbol), over every module
    /// (including `std`). Consumers render a `Signal::enum_type` value as its
    /// symbol (`'X'`, `Idle`) instead of a bare number.
    pub enum_syms: HashMap<String, HashMap<u64, String>>,
    /// Directory that relative `read`/`read_to_string`/`exists` paths resolve
    /// against — the design's source directory. Empty means the current working
    /// directory (the default; a bare `Design` reads CWD-relative).
    pub base_dir: std::path::PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SignalId(pub u32);

#[derive(Clone, Debug)]
pub struct Signal {
    /// Hierarchical path, e.g. `Counter.count`.
    pub path: String,
    /// Bit width; `0` means "not yet known" (a parametric width).
    pub width: u32,
    /// A `real`-typed value: the 64-bit slot holds f64 bits, and arithmetic
    /// uses the float operators.
    pub real: bool,
    /// A `Char`-typed value: the slot holds a symbol (stored as its Unicode
    /// code point — an implementation detail); character literals compared or
    /// assigned to it read through the Unicode table.
    pub char: bool,
    /// A ranged numeric's value domain (`integer<lo..hi>`, spec 3.26): the
    /// simulation checks every settled value against it — a dynamic range
    /// assert. Plain `uint[N]`/`int[N]` wrap instead (documented semantics).
    pub range: Option<(i64, i64)>,
    /// The declared initial value's bit pattern (`let v: T = 1;`): engines
    /// reset signals to it (VHDL-style initial values), not to zero.
    pub init: u64,
    /// The enum type name, when this signal holds an enum value (`Logic`,
    /// `Bit`, a user FSM `State`). Lets consumers render the stored
    /// discriminant as its variant symbol (`'X'`, `Idle`) instead of a number.
    pub enum_type: Option<String>,
}

/// A combinational driver: `signal = expr` under `cond` (spec 3.14 source-order
/// override is resolved during lowering into a priority chain).
#[derive(Clone, Debug)]
pub struct Driver {
    pub target: SignalId,
    pub cond: Option<Expr>,
    pub expr: Expr,
    /// Driver context (spec 3.14): one per impl block / per port connection.
    /// Within a context later drivers override; a signal driven from several
    /// contexts folds via its type's `Resolve` impl (or errors without one).
    pub ctx: u32,
}

/// An event-controlled block: on `condition`, queue `next(target) = expr`
/// (spec 3.13 next-state semantics).
#[derive(Clone, Debug)]
pub struct EventBlock {
    pub condition: Expr,
    pub updates: Vec<NextUpdate>,
}

#[derive(Clone, Debug)]
pub struct NextUpdate {
    pub target: SignalId,
    pub cond: Option<Expr>,
    pub expr: Expr,
}

/// IR expression. `::event`/`::old` are first-class so the scheduler can read
/// them directly; `clk::rising` lowers into `Event`/`Old`/`Current`.
#[derive(Clone, Debug)]
pub enum Expr {
    Const(u64),
    /// A `real` constant; evaluates to its f64 bit pattern.
    Real(f64),
    Logic(char),
    Current(SignalId),
    Old(SignalId),
    Event(SignalId),
    Unary { op: UnOp, rhs: Box<Expr> },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr> },
    /// Bit slice `base[hi..lo]` (inclusive), value `(base >> lo) & mask(hi-lo+1)`.
    Slice { base: Box<Expr>, hi: u32, lo: u32 },
    /// `cond ? then : els` — produced by inlining operator-trait impl bodies
    /// (`if`/`else` chains of `return`s become nested selects).
    Select { cond: Box<Expr>, then: Box<Expr>, els: Box<Expr> },
    /// A foreign C call (`extern "C"` declarations, spec 3.27): `real`
    /// parameters/results are f64 (bit-pattern operands), everything else a
    /// 64-bit word. Engines call the named symbol (JIT: process symbols;
    /// native: linked libraries; the interpreter evaluates the libm set).
    CCall { name: String, args: Vec<Expr>, f64_args: Vec<bool>, f64_ret: bool },
    /// A reference that could not be lowered (unknown signal, unsupported form).
    Unknown,
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    And,
    Or,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Float arithmetic on f64-bit values (`real` operands).
    FAdd,
    FSub,
    FMul,
    FDiv,
}

/// Lower the elaborated design into simulation IR. Relative file-read paths
/// resolve against the current working directory (see [`lower_in`] to set a
/// source-relative base directory).
pub fn lower(modules: &[Module], hier: &Hierarchy, sink: &mut DiagnosticSink) -> Design {
    lower_in(modules, hier, sink, std::path::Path::new(""))
}

/// Lower with `base_dir` as the root that relative `read`/`read_to_string`
/// paths resolve against (the design's source directory), so a program that
/// bakes in a data file works regardless of the working directory.
pub fn lower_in(
    modules: &[Module],
    hier: &Hierarchy,
    sink: &mut DiagnosticSink,
    base_dir: &std::path::Path,
) -> Design {
    let mut l = Lowering::new(sink);
    l.base_dir = base_dir.to_path_buf();
    l.out.base_dir = base_dir.to_path_buf();
    l.collect(modules);
    l.enum_variants = enum_discriminants(modules);
    // Reverse each enum's variant map (name -> disc) into disc -> symbol, so
    // consumers can render stored discriminants symbolically.
    l.out.enum_syms = l
        .enum_variants
        .iter()
        .map(|(ty, vars)| {
            (ty.clone(), vars.iter().map(|(sym, &d)| (d, sym.clone())).collect())
        })
        .collect();
    l.enum_reprs = enum_reprs(modules);
    l.vector_families = vector_families(modules);
    {
        let enums = enum_index(modules);
        for (name, e) in &enums {
            if let Some(b) = enum_base_name(e, &enums) {
                l.enum_bases.insert(name.clone(), b);
            }
        }
    }

    // The entity types that appear in the elaborated hierarchy, in first-seen
    // order, deduplicated. Each entity's parameters are taken from its first
    // instance, so `uint[W]` lowers with the instance's concrete `W`.
    let mut seen = Vec::new();
    for inst in &hier.instances {
        if !seen.contains(&inst.entity) {
            seen.push(inst.entity.clone());
            l.entity_params.entry(inst.entity.clone()).or_insert_with(|| {
                inst.params
                    .iter()
                    .filter_map(|(n, v)| match v {
                        siox_elab::ParamValue::Int(i) => Some((n.clone(), *i)),
                        siox_elab::ParamValue::Unknown => None,
                    })
                    .collect()
            });
        }
    }
    // Lower only the top-level designs — the `#[top]`/`#[test]` roots. Their
    // sub-instances (and a testbench's DUTs) are lowered recursively from there,
    // each per-instance, so no entity is lowered standalone by type.
    let mut roots = Vec::new();
    for &r in &hier.roots {
        let ent = hier.instance(r).entity.clone();
        if !roots.contains(&ent) {
            roots.push(ent);
        }
    }
    for name in &roots {
        l.lower_entity(name);
    }
    l.lint_possible_latches();
    l.resolve_driver_contexts();
    l.lint_combinational_loops();
    l.out
}

struct Lowering<'a> {
    sink: &'a mut DiagnosticSink,
    /// Root for relative compile-time file reads (the source directory).
    base_dir: std::path::PathBuf,
    /// Signals given a default by a match wildcard arm — excluded from the
    /// possible-latch lint even though their lowered drivers are conditional.
    lint_defaulted: std::collections::HashSet<u32>,
    entities: HashMap<String, &'a ast::EntityDecl>,
    impls: HashMap<String, Vec<&'a ast::ImplDecl>>,
    /// Entity name -> its instance's concrete parameter values.
    entity_params: HashMap<String, HashMap<String, i64>>,
    /// Enum name -> variant name -> discriminant value.
    enum_variants: HashMap<String, HashMap<String, u64>>,
    /// Struct name -> its declaration (for flattening struct signals).
    structs: HashMap<String, &'a ast::StructDecl>,
    /// Bus-mode per-leaf directions: `(struct, mode) -> {field -> dir}` from
    /// `impl <dir> Struct::Mode { in a; out b; }` (spec 3.19).
    mode_dirs: HashMap<(String, String), HashMap<String, ast::Direction>>,
    /// Enum name -> its bit width (repr, or bits for the variant count).
    enum_reprs: HashMap<String, u32>,
    /// Enum name -> base enum name (derivation chain, enums only).
    enum_bases: HashMap<String, String>,
    /// (trait name, target type) -> the impl's fns with the impl's declared
    /// rhs type (the `integer` in `impl Add<integer> for T`; `None` reads as
    /// `Self`). Overloads select by that rhs, or the fn's rhs parameter type.
    op_impls: HashMap<(String, String), Vec<(&'a ast::FnDecl, Option<String>)>>,
    /// Literal suffix -> (target type, fn), for suffix inlining.
    suffix_impls: HashMap<String, (String, &'a ast::FnDecl)>,
    /// Module-level functions, inlined at call sites / const-evaluated.
    free_fns: HashMap<String, &'a ast::FnDecl>,
    /// Inline depth guard (recursive fns must const-fold; runaway inlining
    /// stops here).
    inline_depth: std::cell::Cell<u32>,
    /// Type-family of each generic-fn parameter during inlining (param name ->
    /// the concrete argument's family), so operator dispatch in the body uses
    /// the caller's type (e.g. int's signed `Ord`, not the kernel compare).
    param_types: std::cell::RefCell<HashMap<String, String>>,
    /// Module-level integer constants (`const N: integer = 4`).
    consts: HashMap<String, i64>,
    /// Module-level `real` constants (`const PI: real = 3.14159...`).
    consts_real: HashMap<String, f64>,
    /// Module-level range constants (`const BYTE: range = 7..0`), as written
    /// (left, right) so direction is preserved.
    const_ranges: HashMap<String, (i64, i64)>,
    /// Type aliases (`using Word = uint[32]`).
    aliases: HashMap<String, ast::Type>,
    /// The active entity's width environment (consts + instance params),
    /// for const-evaluating slice bounds during expression lowering.
    cur_env: HashMap<String, i64>,
    /// The active entity's type-parameter bindings (`T -> uint[8]` for a
    /// generic entity `Buf<uint[8]>`), substituted into port/signal types.
    cur_type_env: HashMap<String, ast::Type>,
    out: Design,
    /// Signal name -> id, valid while lowering a single entity.
    locals: HashMap<String, SignalId>,
    /// Local name -> its enum type name (operator-impl operands).
    local_enum: HashMap<String, String>,
    /// Local name -> its struct type name (multi-signal operands/targets).
    local_struct: HashMap<String, String>,
    /// Locals of the symbol base type `Char`.
    local_char: std::collections::HashSet<String>,
    /// Array-typed locals -> their ordered element indices (whole-array
    /// assignment and string literals expand per element).
    local_array: HashMap<String, Vec<i64>>,
    /// The active driver context (bumped per impl block / connection).
    cur_ctx: u32,
    /// Signal -> declared type name (enum / uint / int), for Resolve lookup.
    sig_type: HashMap<u32, String>,
    /// Array-derived Logic vector families (`struct F : Logic[]`) -> signed?.
    /// uint/int are just the first two; the family set is read from the
    /// declarations, not hardcoded.
    vector_families: std::collections::HashSet<String>,
    /// Numeric-vector locals -> the family name, for operator-impl dispatch
    /// (kernel `integer`/`real` keep builtin operators; uint/int live in std).
    local_numeric: HashMap<String, String>,
}

/// A lowered value: a scalar expression, or one expression per struct field
/// (a struct-typed value has no single-signal representation).
#[derive(Clone, Debug)]
enum Val {
    Scalar(Expr),
    Fields(Vec<(String, Expr)>),
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
                        Expr::Select { cond: Box::new(cond.clone()), then: Box::new(t), els: Box::new(e) },
                    )
                })
                .collect(),
        ),
        _ => Val::Scalar(Expr::Unknown),
    }
}

impl<'a> Lowering<'a> {
    fn new(sink: &'a mut DiagnosticSink) -> Self {
        Lowering {
            sink,
            base_dir: std::path::PathBuf::new(),
            lint_defaulted: std::collections::HashSet::new(),
            entities: HashMap::new(),
            impls: HashMap::new(),
            entity_params: HashMap::new(),
            enum_variants: HashMap::new(),
            structs: HashMap::new(),
            mode_dirs: HashMap::new(),
            enum_reprs: HashMap::new(),
            enum_bases: HashMap::new(),
            op_impls: HashMap::new(),
            suffix_impls: HashMap::new(),
            free_fns: HashMap::new(),
            inline_depth: std::cell::Cell::new(0),
            param_types: std::cell::RefCell::new(HashMap::new()),
            consts: HashMap::new(),
            consts_real: HashMap::new(),
            const_ranges: HashMap::new(),
            aliases: HashMap::new(),
            cur_env: HashMap::new(),
            cur_type_env: HashMap::new(),
            out: Design::default(),
            locals: HashMap::new(),
            local_enum: HashMap::new(),
            local_struct: HashMap::new(),
            local_char: std::collections::HashSet::new(),
            local_array: HashMap::new(),
            local_numeric: HashMap::new(),
            vector_families: std::collections::HashSet::new(),
            cur_ctx: 0,
            sig_type: HashMap::new(),
        }
    }

    fn collect(&mut self, modules: &'a [Module]) {
        for m in modules {
            for item in &m.items {
                match item {
                    ast::Item::Entity(e) => {
                        self.entities.insert(e.name.text.clone(), e);
                    }
                    ast::Item::Fn(f) => {
                        self.free_fns.insert(f.name.text.clone(), f);
                    }
                    ast::Item::ExternBlock { fns, .. } => {
                        for f in fns {
                            self.free_fns.insert(f.name.text.clone(), f);
                        }
                    }
                    ast::Item::Struct(s) => {
                        self.structs.insert(s.name.text.clone(), s);
                    }
                    // Module constants join the width environment; range
                    // constants (`const BYTE: range = 7..0`) keep their
                    // written direction. Aliases substitute during lowering.
                    ast::Item::Const(c) => {
                        if let ast::Expr::Range { lo, hi, .. } = &c.value {
                            if let (Some(a), Some(b)) =
                                (eval_const(lo, &self.consts), eval_const(hi, &self.consts))
                            {
                                self.const_ranges.insert(c.name.text.clone(), (a, b));
                            }
                        } else if let Some(v) = eval_const(&c.value, &self.consts) {
                            self.consts.insert(c.name.text.clone(), v);
                        } else if let ast::Expr::Int { text, .. } = &c.value {
                            if let Ok(f) = text.parse::<f64>() {
                                self.consts_real.insert(c.name.text.clone(), f);
                            }
                        }
                    }
                    ast::Item::Using(u) => {
                        if let ast::UsingKind::Alias { name, ty } = &u.kind {
                            self.aliases.insert(name.text.clone(), ty.clone());
                        }
                    }
                    ast::Item::Impl(im) if im.trait_.is_none() => {
                        // A bus-mode impl (`impl out Stream::Source { in a;
                        // out b; }`, spec 3.19) records its fields' directions;
                        // a plain inherent impl holds methods.
                        if im.mode_dir.is_some() {
                            if let Some(key) = Self::mode_of(&im.target) {
                                let map = self.mode_dirs.entry(key).or_default();
                                for it in &im.items {
                                    if let ast::ImplItem::ModeField { dir, name, .. } = it {
                                        map.insert(name.text.clone(), *dir);
                                    }
                                }
                            }
                        } else if let Some(name) = type_head_name(&im.target) {
                            self.impls.entry(name.to_string()).or_default().push(im);
                        }
                    }
                    // A trait impl's first fn is the operator body for
                    // `impl "+" for T` (spec 3.25); each fn of an
                    // `impl Suffix for T` defines the literal suffix of its
                    // name (spec 3.24).
                    ast::Item::Impl(im) => {
                        let tr = im.trait_.as_ref().and_then(|t| t.segments.last());
                        let target = type_head_name(&im.target);
                        if let (Some(tr), Some(ty)) = (tr, target) {
                            if tr.text == "Suffix" {
                                for it in &im.items {
                                    if let ast::ImplItem::Fn(f) = it {
                                        self.suffix_impls
                                            .insert(f.name.text.clone(), (ty.to_string(), f));
                                    }
                                }
                            } else {
                                // `impl Add<integer> for T`: the trait's type
                                // argument names the rhs operand type.
                                let custom_symbol = (tr.text == "custom")
                                    .then(|| im.trait_args.first())
                                    .flatten()
                                    .and_then(|a| match a {
                                        ast::GenericArg::Positional(ast::Expr::StrLit {
                                            text,
                                            ..
                                        }) => Some(text.clone()),
                                        _ => None,
                                    });
                                let input_index = usize::from(custom_symbol.is_some());
                                let rhs_arg = im.trait_args.get(input_index).and_then(|a| match a {
                                    ast::GenericArg::Positional(ast::Expr::Path(p)) => {
                                        p.segments.last().map(|s| s.text.clone())
                                    }
                                    _ => None,
                                });
                                let operator = custom_symbol.unwrap_or_else(|| tr.text.clone());
                                for it in &im.items {
                                    if let ast::ImplItem::Fn(f) = it {
                                        self.op_impls
                                            .entry((operator.clone(), ty.to_string()))
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
    }

    fn lower_entity(&mut self, name: &str) {
        let Some(edecl) = self.entities.get(name).copied() else { return };
        // Extern entities are black boxes.
        if edecl.is_extern {
            return;
        }
        let mut env = self.consts.clone();
        env.extend(self.entity_params.get(name).cloned().unwrap_or_default());
        if has_attr(edecl, "test") {
            // A testbench: lower only its DUT instances, each per-instance under
            // the testbench path (`CounterTest.dut.*`), so two instances of one
            // entity are distinct. Stimulus statements are interpreted by the
            // runner, and testbench<->DUT connections go through the runner's
            // signal map — so no top-level connection drivers here.
            self.lower_testbench_duts(name, &env);
            return;
        }
        // A top-level DUT: signals are entity-qualified (`Counter.count`), and
        // widths come from its first instance's parameters.
        self.lower_body(name, name, &env, &HashMap::new(), &HashMap::new());
    }

    /// Lower each `let inst = Sub { .. }` DUT of a testbench into its own
    /// namespace `<testbench>.<inst>.*` (with the DUT's internal logic and
    /// sub-instances). No testbench signals, statements, or top connections.
    fn lower_testbench_duts(&mut self, name: &str, env: &HashMap<String, i64>) {
        let impls: Vec<&ast::ImplDecl> = self.impls.get(name).cloned().unwrap_or_default();
        // Every port a testbench name is connected to, across all DUTs — when
        // one name binds an `out` and `in` ports (a DUT feeding another, or
        // its own input), the out drives the ins as real hardware, so the
        // value propagates on every settle without runner involvement.
        let mut bindings: HashMap<String, Vec<(SignalId, Option<ast::Direction>)>> =
            HashMap::new();
        for im in &impls {
            for item in &im.items {
                if let ast::ImplItem::Let(l) = item {
                    if let Some(ast::Expr::Construct { ty: Some(cty), args, .. }) = &l.value {
                        if let Some(sub) = type_head_name(cty) {
                            let sub_path = format!("{name}.{}", l.name.text);
                            let mut sub_env = self.consts.clone();
                            sub_env.extend(self.construct_params(cty, env));
                            let sub_tenv = self.construct_type_params(cty, sub);
                            let sub_ports =
                                self.lower_body(sub, &sub_path, &sub_env, &sub_tenv, &HashMap::new());
                            for (port, value) in self.norm_conns(args, sub) {
                                // The testbench name the port binds to; a
                                // literal/expression connection has no name.
                                let Some(tbname) = expr_path(&value) else { continue };
                                if let Some(&(sig, dir)) = sub_ports.get(&port) {
                                    bindings.entry(tbname).or_default().push((sig, dir));
                                }
                            }
                        }
                    }
                }
            }
        }
        for ports in bindings.values() {
            let outs: Vec<SignalId> = ports
                .iter()
                .filter(|(_, d)| *d == Some(ast::Direction::Out))
                .map(|&(s, _)| s)
                .collect();
            let ins: Vec<SignalId> = ports
                .iter()
                .filter(|(_, d)| *d == Some(ast::Direction::In))
                .map(|&(s, _)| s)
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
                        target: i,
                        cond: None,
                        expr: Expr::Current(o),
                        ctx,
                    });
                }
            }
        }
    }

    /// Lower entity `ename`'s body, naming signals under `path` (the instance
    /// path — `Counter.count` at the top, `Add2.s1.a` for a sub-instance) in the
    /// width environment `env`. Sub-instances (`let s = Sub { .p = x, .. }`) are
    /// lowered recursively under `path.s` and their port connections become
    /// drivers. Returns each port's (signal, direction) so a parent can wire to
    /// it. Runs in a fresh name scope, restoring the caller's on return.
    fn lower_body(
        &mut self,
        ename: &str,
        path: &str,
        env: &HashMap<String, i64>,
        type_env: &HashMap<String, ast::Type>,
        aliases: &HashMap<String, SignalId>,
    ) -> HashMap<String, (SignalId, Option<ast::Direction>)> {
        let Some(edecl) = self.entities.get(ename).copied() else {
            return HashMap::new();
        };

        // Save the caller's scope; give this body a fresh one.
        let saved_locals = std::mem::take(&mut self.locals);
        let saved_enum = std::mem::take(&mut self.local_enum);
        let saved_struct = std::mem::take(&mut self.local_struct);
        let saved_char = std::mem::take(&mut self.local_char);
        let saved_array = std::mem::take(&mut self.local_array);
        let saved_numeric = std::mem::take(&mut self.local_numeric);
        let saved_env = std::mem::replace(&mut self.cur_env, env.clone());
        let saved_type_env = std::mem::replace(&mut self.cur_type_env, type_env.clone());

        // Ports (struct/array-typed ones flatten to leaves), then the port map.
        // An `inout` port aliased to a parent net reuses that net's signal
        // instead of allocating its own: the body's `pin = expr` then drives the
        // shared net (resolving across instances) and reads of `pin` read the
        // resolved value — Verilog's bidirectional-port model.
        for p in &edecl.ports {
            self.add_typed_signal(path, &p.name.text, &p.ty, env);
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
        for p in &edecl.ports {
            let dot = format!("{}.", p.name.text);
            let idx = format!("{}[", p.name.text);
            // A bus-mode port (`bus: out Stream::Source`) gives each leaf its own
            // direction from the mode impl (`out valid; in ready;`); a plain port
            // applies its single direction to every leaf.
            let mode = Self::mode_of(&p.ty).and_then(|k| self.mode_dirs.get(&k));
            for (k, &id) in &self.locals {
                if *k == p.name.text || k.starts_with(&dot) || k.starts_with(&idx) {
                    let dir = match mode {
                        Some(m) => k
                            .strip_prefix(&dot)
                            .and_then(|field| m.get(field).copied())
                            .or(p.dir),
                        None => p.dir,
                    };
                    ports.insert(k.clone(), (id, dir));
                }
            }
        }

        // `let` items: instance bindings are collected for recursion; the rest
        // become state signals.
        let impls: Vec<&ast::ImplDecl> = self.impls.get(ename).cloned().unwrap_or_default();
        let mut subinsts: Vec<(String, ast::Type, Vec<ast::ConnectArg>)> = Vec::new();
        // Generate loops (`for i in 0..n { let s = Sub { .. } }`) unroll here,
        // substituting the loop index into each instance's type args and
        // connections so the flattened element signals (`wires[i]`) resolve.
        for im in &impls {
            for item in &im.items {
                if let ast::ImplItem::Stmt(s) = item {
                    gather_generate(s, env, &[], &mut subinsts);
                }
            }
        }
        for im in &impls {
            for item in &im.items {
                if let ast::ImplItem::Let(l) = item {
                    // `let s = Sub { .. }`: a sub-instance, not a signal.
                    if let Some(ast::Expr::Construct { ty: Some(cty), args, .. }) = &l.value {
                        subinsts.push((l.name.text.clone(), cty.clone(), args.clone()));
                        continue;
                    }
                    // `let s: string = "hello";`: the literal sets the range.
                    let unconstrained = match &l.ty {
                        None => true,
                        Some(t) => matches!(
                            self.resolve_alias_shallow(t),
                            ast::Type::Indexed { index: None, .. }
                        ),
                    };
                    if unconstrained {
                        if let Some(ast::Expr::StrLit { text, .. }) = &l.value {
                            self.add_char_array(path, &l.name.text, text.chars().count());
                            continue;
                        }
                        // `let s: string = read_to_string("f.txt");` — the
                        // compiler reads the file; its length sets the range.
                        if let Some(fpath) =
                            l.value.as_ref().and_then(|v| Self::fs_read_call(v, "read_to_string"))
                        {
                            match std::fs::read_to_string(self.base_dir.join(fpath)) {
                                Ok(text) => {
                                    let chars: Vec<char> = text.chars().collect();
                                    self.add_char_array(path, &l.name.text, chars.len());
                                    for (i, c) in chars.iter().enumerate() {
                                        if let Some(&id) =
                                            self.locals.get(&format!("{}[{i}]", l.name.text))
                                        {
                                            self.out.signals[id.0 as usize].init = *c as u32 as u64;
                                        }
                                    }
                                }
                                Err(e) => self.sink.emit(siox_diag::Diagnostic::error(format!(
                                    "read_to_string(\"{fpath}\"): {e}"
                                ))),
                            }
                            continue;
                        }
                    }
                    if let Some(ty) = &l.ty {
                        self.add_typed_signal(path, &l.name.text, ty, env);
                    } else {
                        self.add_signal(path, &l.name.text, 0);
                    }
                    // `let rom: uint[8][N] = read("rom.bin");` — the compiler
                    // reads the file and bakes it into the element inits
                    // (little-endian packing for elements wider than a byte;
                    // a shorter file leaves the tail at 0; longer errors).
                    if let Some(fpath) =
                        l.value.as_ref().and_then(|v| Self::fs_read_call(v, "read"))
                    {
                        if let Some(indices) = self.local_array.get(&l.name.text).cloned() {
                            match std::fs::read(self.base_dir.join(fpath)) {
                                Ok(bytes) => {
                                    let ew = self
                                        .locals
                                        .get(&format!("{}[{}]", l.name.text, indices[0]))
                                        .map(|&id| self.out.signals[id.0 as usize].width)
                                        .unwrap_or(8)
                                        .max(1);
                                    let per = ew.div_ceil(8) as usize;
                                    if bytes.len() > per * indices.len() {
                                        self.sink.emit(siox_diag::Diagnostic::error(format!(
                                            "read(\"{fpath}\"): {} bytes do not fit `{}` \
                                             ({} elements x {per} bytes)",
                                            bytes.len(),
                                            l.name.text,
                                            indices.len()
                                        )));
                                    }
                                    for (n, i) in indices.iter().enumerate() {
                                        let mut v = 0u64;
                                        for b in 0..per {
                                            let byte = bytes.get(n * per + b).copied().unwrap_or(0);
                                            v |= (byte as u64) << (8 * b);
                                        }
                                        if let Some(&id) =
                                            self.locals.get(&format!("{}[{i}]", l.name.text))
                                        {
                                            let w = self.out.signals[id.0 as usize].width;
                                            let masked = if w > 0 && w < 64 {
                                                v & ((1u64 << w) - 1)
                                            } else {
                                                v
                                            };
                                            self.out.signals[id.0 as usize].init = masked;
                                        }
                                    }
                                }
                                Err(e) => self.sink.emit(siox_diag::Diagnostic::error(format!(
                                    "read(\"{fpath}\"): {e}"
                                ))),
                            }
                            continue;
                        }
                    }
                    // A constant initializer is the signal's reset value.
                    if let (Some(v), Some(&id)) = (&l.value, self.locals.get(&l.name.text)) {
                        if let Some(bits) = self.const_init_bits(v) {
                            let w = self.out.signals[id.0 as usize].width;
                            let masked = if w > 0 && w < 64 { bits & ((1u64 << w) - 1) } else { bits };
                            self.out.signals[id.0 as usize].init = masked;
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
            let Some(sub_ename) = type_head_name(cty) else { continue };
            let sub_path = format!("{path}.{inst}");
            let mut sub_env = self.consts.clone();
            sub_env.extend(self.construct_params(cty, env));
            let sub_type_env = self.construct_type_params(cty, sub_ename);

            // Resolve `inout` connections to the parent net they share *before*
            // lowering the child, so its port aliases to that net. A scalar
            // inout whose parent side isn't a plain signal is left un-aliased
            // (falls back to the in/out wiring below).
            // Normalized `(port, value)` connections (positional/shorthand
            // resolved), used both for inout aliasing and the wiring below.
            let norm = self.norm_conns(conns, sub_ename);
            let mut aliases: HashMap<String, SignalId> = HashMap::new();
            if let Some(decl) = self.entities.get(sub_ename).copied() {
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

            let sub_ports = self.lower_body(sub_ename, &sub_path, &sub_env, &sub_type_env, &aliases);
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
                            let ctx = self.next_ctx();
                            self.out.drivers.push(Driver { target, cond: None, expr: Expr::Current(child_id), ctx });
                        }
                    } else {
                        let expr = self.lower_expr(value);
                        let ctx = self.next_ctx();
                        self.out.drivers.push(Driver { target: child_id, cond: None, expr, ctx });
                    }
                    continue;
                }

                // A composite (struct/array) port: wire each leaf to the matching
                // leaf of the parent signal (`.s = link` -> `s.valid`<->`link.valid`).
                // The parent side must be a signal path.
                let Some(base) = expr_path(value) else { continue };
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
                    let ctx = self.next_ctx();
                    if dir == Some(ast::Direction::Out) {
                        self.out.drivers.push(Driver { target: parent_id, cond: None, expr: Expr::Current(child_id), ctx });
                    } else {
                        self.out.drivers.push(Driver { target: child_id, cond: None, expr: Expr::Current(parent_id), ctx });
                    }
                }
            }
        }

        // Behaviour: each impl block is one driver context (spec 3.14 —
        // override within, resolution across).
        for im in &impls {
            self.cur_ctx += 1;
            for item in &im.items {
                if let ast::ImplItem::Stmt(stmt) = item {
                    self.lower_stmt(stmt, None);
                }
            }
        }

        // Restore the caller's scope.
        self.locals = saved_locals;
        self.local_enum = saved_enum;
        self.local_struct = saved_struct;
        self.local_char = saved_char;
        self.local_array = saved_array;
        self.local_numeric = saved_numeric;
        self.cur_env = saved_env;
        self.cur_type_env = saved_type_env;
        ports
    }

    /// Concrete parameter bindings written on an instance type (`Counter<W=8>`).
    fn construct_params(&self, ty: &ast::Type, env: &HashMap<String, i64>) -> HashMap<String, i64> {
        let mut out = HashMap::new();
        if let ast::Type::Generic { args, .. } = ty {
            for a in args {
                if let ast::GenericArg::Named { name, value } = a {
                    if let Some(v) = eval_const(value, env) {
                        out.insert(name.text.clone(), v);
                    }
                }
            }
        }
        out
    }

    /// Type-parameter bindings for a generic entity instance (`Buf<uint[8]>` ->
    /// `T -> uint[8]`): the entity's bare type params (bound `None`), matched to
    /// the construct's generic args positionally or by name.
    fn construct_type_params(&self, ty: &ast::Type, ename: &str) -> HashMap<String, ast::Type> {
        let mut out = HashMap::new();
        let (Some(decl), ast::Type::Generic { args, .. }) = (self.entities.get(ename), ty) else {
            return out;
        };
        let type_params: Vec<&ast::Param> =
            decl.params.params.iter().filter(|p| p.bound.is_none()).collect();
        for (i, a) in args.iter().enumerate() {
            match a {
                ast::GenericArg::Named { name, value } => {
                    if type_params.iter().any(|p| p.name.text == name.text) {
                        if let Some(t) = expr_to_type(value) {
                            out.insert(name.text.clone(), t);
                        }
                    }
                }
                ast::GenericArg::Positional(e) => {
                    if let (Some(p), Some(t)) = (decl.params.params.get(i), expr_to_type(e)) {
                        if p.bound.is_none() {
                            out.insert(p.name.text.clone(), t);
                        }
                    }
                }
            }
        }
        out
    }

    /// Combinational-loop lint (W-P010): a combinational signal whose value
    /// depends on itself through only combinational drivers is a zero-delay
    /// cycle with no register to break it — it has no well-defined settled
    /// value (the engines stop it at an arbitrary point). Event-block
    /// (sequential) targets break a cycle, so only comb→comb edges count.
    fn lint_combinational_loops(&mut self) {
        use std::collections::{BTreeSet, HashMap, HashSet};
        let procs = self.out.processes();
        // Signals driven combinationally, and for each its comb dependencies
        // (reads that are themselves combinational targets).
        let comb_targets: HashSet<u32> = procs
            .iter()
            .filter_map(|p| match p.kind {
                ProcessKind::Comb { target, .. } => Some(target.0),
                _ => None,
            })
            .collect();
        let mut deps: HashMap<u32, Vec<u32>> = HashMap::new();
        for p in &procs {
            if let ProcessKind::Comb { target, .. } = p.kind {
                let e = deps.entry(target.0).or_default();
                for r in &p.reads {
                    if comb_targets.contains(&r.0) {
                        e.push(r.0);
                    }
                }
            }
        }
        // A signal on a cycle can reach itself. Report each such signal once.
        let reaches_self = |start: u32| -> bool {
            let mut stack = deps.get(&start).cloned().unwrap_or_default();
            let mut seen: HashSet<u32> = HashSet::new();
            while let Some(n) = stack.pop() {
                if n == start {
                    return true;
                }
                if seen.insert(n) {
                    if let Some(next) = deps.get(&n) {
                        stack.extend(next.iter().copied());
                    }
                }
            }
            false
        };
        let mut looped: BTreeSet<u32> = BTreeSet::new();
        for &t in &comb_targets {
            if reaches_self(t) {
                looped.insert(t);
            }
        }
        for t in looped {
            let path = self.out.signals[t as usize].path.clone();
            self.sink.emit(
                siox_diag::Diagnostic::warning(format!(
                    "`{path}` is in a combinational loop — its value depends on itself \
                     with no register in the path, so it has no settled value"
                ))
                .with_code(siox_diag::codes::COMBINATIONAL_LOOP)
                .help("break the loop with a clocked register, or an unconditional default"),
            );
        }
    }

    /// Possible-latch lint (W-P002): a *combinational* signal that is only ever
    /// assigned under a condition keeps its previous value when no condition
    /// holds — an inferred latch. We flag the clean case: a single driver
    /// context whose drivers are all conditional. Event-block (sequential)
    /// signals hold by design, and multi-context signals go through `Resolve`,
    /// so both are excluded to avoid false positives.
    fn lint_possible_latches(&mut self) {
        use std::collections::{BTreeMap, BTreeSet};
        // Sequential state: any signal a clocked block updates.
        let mut sequential: BTreeSet<u32> = BTreeSet::new();
        for eb in &self.out.event_blocks {
            for u in &eb.updates {
                sequential.insert(u.target.0);
            }
        }
        // Per signal: its driver contexts, and whether any driver is a default.
        let mut ctxs: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
        let mut has_default: BTreeMap<u32, bool> = BTreeMap::new();
        for d in &self.out.drivers {
            ctxs.entry(d.target.0).or_default().insert(d.ctx);
            let e = has_default.entry(d.target.0).or_insert(false);
            *e |= d.cond.is_none();
        }
        for (t, default) in &has_default {
            if *default
                || sequential.contains(t)
                || ctxs[t].len() > 1
                || self.lint_defaulted.contains(t)
            {
                continue;
            }
            let path = self.out.signals[*t as usize].path.clone();
            self.sink.emit(
                siox_diag::Diagnostic::warning(format!(
                    "`{path}` is only assigned under a condition, so it holds its \
                     previous value otherwise (inferred latch)"
                ))
                .with_code(siox_diag::codes::POSSIBLE_LATCH)
                .help("give it an unconditional default assignment"),
            );
        }
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
            by_target.entry(d.target.0).or_default().entry(d.ctx).or_default().push(i);
        }
        let mut replaced: Vec<(u32, Expr)> = Vec::new();
        for (t, ctxs) in &by_target {
            if ctxs.len() < 2 {
                continue;
            }
            let ty = self.sig_type.get(t).cloned().unwrap_or_default();
            let has_resolve = self.op_impls.contains_key(&("Resolve".to_string(), ty.clone()));
            let path = self.out.signals[*t as usize].path.clone();
            if !has_resolve {
                self.sink.emit(siox_diag::Diagnostic::error(format!(
                    "`{path}` is driven from {} parallel contexts, but `{ty}` is \
                     unresolved (no `impl Resolve`)",
                    ctxs.len()
                )));
                continue;
            }
            // Each context: fold its drivers (later overrides) over a 'Z' base.
            let mut contributions = Vec::new();
            for idxs in ctxs.values() {
                let mut acc = Expr::Logic('Z');
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
            // Pairwise resolve via the impl's inlined body.
            let mut it = contributions.into_iter();
            let mut folded = it.next().unwrap();
            for c in it {
                match self.inline_resolve(&ty, folded.clone(), c) {
                    Some(r) => folded = r,
                    None => {
                        self.sink.emit(siox_diag::Diagnostic::error(format!(
                            "could not inline `impl Resolve for {ty}` folding `{path}`"
                        )));
                        break;
                    }
                }
            }
            replaced.push((*t, folded));
        }
        for (t, expr) in replaced {
            self.out.drivers.retain(|d| d.target.0 != t);
            self.out.drivers.push(Driver {
                target: SignalId(t),
                cond: None,
                expr,
                ctx: 0,
            });
        }
    }

    /// Inline `impl Resolve for <ty>` over two already-lowered expressions.
    fn inline_resolve(&self, ty: &str, a: Expr, b: Expr) -> Option<Expr> {
        let fns = self.op_impls.get(&("Resolve".to_string(), ty.to_string()))?;
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

    /// A `read("path")` / `read_to_string("path")` initializer's literal
    /// path, when `e` is one (elaboration-time file reads, spec std::fs).
    fn fs_read_call<'e>(e: &'e ast::Expr, which: &str) -> Option<&'e str> {
        let ast::Expr::Call { callee, args, .. } = e else { return None };
        let ast::Expr::Path(p) = callee.as_ref() else { return None };
        if p.segments.len() != 1 || p.segments[0].text != which {
            return None;
        }
        match args.first() {
            Some(ast::Expr::StrLit { text, .. }) => Some(text),
            _ => None,
        }
    }

    /// The bit pattern of a constant `let` initializer: integers, logic
    /// literals, enum variants, booleans, and real literals (f64 bits).
    fn const_init_bits(&self, e: &ast::Expr) -> Option<u64> {
        match e {
            ast::Expr::Int { text, .. } if text.contains('.') => {
                text.parse::<f64>().ok().map(f64::to_bits)
            }
            ast::Expr::LogicLit { ch, .. } => Some(match ch {
                '1' | 'H' => 1,
                'Z' => 2,
                'X' | 'U' | 'W' => 3,
                _ => 0,
            }),
            ast::Expr::Bool { value, .. } => Some(*value as u64),
            ast::Expr::Path(p) if p.segments.len() >= 2 => self
                .enum_variants
                .get(&p.segments[0].text)
                .and_then(|m| m.get(&p.segments[1].text))
                .copied(),
            _ => eval_const_fns(e, &self.cur_env, &self.free_fns, 0).map(|v| v as u64),
        }
    }

    fn next_ctx(&mut self) -> u32 {
        self.cur_ctx += 1;
        self.cur_ctx
    }

    fn add_signal(&mut self, entity: &str, name: &str, width: u32) {
        let id = SignalId(self.out.signals.len() as u32);
        self.out.signals.push(Signal {
            path: format!("{entity}.{name}"),
            width,
            real: false,
            char: false,
            range: None,
            init: 0,
            enum_type: None,
        });
        self.locals.insert(name.to_string(), id);
    }

    /// Add a signal for `name: ty`, flattening composites into scalar leaves: a
    /// struct into one signal per field (`s.valid`), an array into one per
    /// element (`a[0]`). Nested composites recurse. An integer vector
    /// (`uint[8]`) stays a single scalar signal.
    fn add_typed_signal(&mut self, entity: &str, name: &str, ty: &ast::Type, env: &HashMap<String, i64>) {
        // A generic entity's type parameters (`T -> uint[8]`) substitute first,
        // so a port/signal typed `T` becomes its concrete type here.
        let subst_ty;
        let ty = if self.cur_type_env.is_empty() {
            ty
        } else {
            subst_ty = subst_type_params(ty, &self.cur_type_env);
            &subst_ty
        };
        // Substitute `using X = T;` aliases; an index applied to an alias of
        // an unconstrained array fills its hole (`string[5]` = `Char[5]`).
        let resolved;
        let ty = match ty {
            ast::Type::Path(p) if p.segments.len() == 1 => {
                match self.aliases.get(&p.segments[0].text) {
                    Some(t) => {
                        resolved = t.clone();
                        &resolved
                    }
                    None => ty,
                }
            }
            ast::Type::Indexed { base, index: Some(i), span } => {
                let inner = match base.as_ref() {
                    ast::Type::Path(p) if p.segments.len() == 1 => {
                        self.aliases.get(&p.segments[0].text)
                    }
                    _ => None,
                };
                match inner {
                    Some(ast::Type::Indexed { base: elem, index: None, .. }) => {
                        resolved = ast::Type::Indexed {
                            base: elem.clone(),
                            index: Some(i.clone()),
                            span: *span,
                        };
                        &resolved
                    }
                    _ => ty,
                }
            }
            _ => ty,
        };
        // An unconstrained array (`Char[]`) has no length to flatten with.
        if let ast::Type::Indexed { index: None, .. } = ty {
            self.sink.emit(
                siox_diag::Diagnostic::error(format!(
                    "unconstrained array type for `{name}`: the range must be set here                      (e.g. an explicit length)"
                ))
                .with_code(siox_diag::codes::TYPE_MISMATCH),
            );
            return;
        }
        // A genuine field-aggregate flattens to per-field signals; a struct
        // with no fields (a derived vector like `struct Byte : Logic[8]`) is a
        // scalar leaf and takes its inherited width below.
        if let Some(fields) = self.struct_fields(ty).filter(|f| !f.is_empty()) {
            if let ast::Type::Path(p) = ty {
                self.local_struct.insert(name.to_string(), p.segments[0].text.clone());
            }
            for (fname, fty) in fields {
                self.add_typed_signal(entity, &format!("{name}.{fname}"), &fty, env);
            }
        } else if let Some((elem, indices)) =
            array_of(ty, env, &self.const_ranges, &self.vector_families)
        {
            let elem = elem.clone();
            self.local_array.insert(name.to_string(), indices.clone());
            for i in indices {
                self.add_typed_signal(entity, &format!("{name}[{i}]"), &elem, env);
            }
        } else if let Some(w) = self.enum_width(ty) {
            if let ast::Type::Path(p) = ty {
                self.local_enum.insert(name.to_string(), p.segments[0].text.clone());
            }
            self.add_signal(entity, name, w);
            if let (ast::Type::Path(p), Some(&id)) = (ty, self.locals.get(name)) {
                self.sig_type.insert(id.0, p.segments[0].text.clone());
                // Record the enum type so consumers render variants symbolically.
                self.out.signals[id.0 as usize].enum_type = Some(p.segments[0].text.clone());
            }
        } else if let Some((w, is_real, range)) = self.ranged_numeric(ty) {
            // `integer<lo..hi>` stores in the smallest width covering the
            // range (two's complement when lo < 0); `real<..>` stays f64. The
            // bounds ride on the signal for the simulation's range checks.
            self.add_signal(entity, name, w);
            if let Some(&id) = self.locals.get(name) {
                if is_real {
                    self.out.signals[id.0 as usize].real = true;
                }
                self.out.signals[id.0 as usize].range = range;
            }
        } else {
            self.add_signal(entity, name, type_width(ty, env, &self.free_fns, &self.structs));
            // A Logic-vector family `F[N]` dispatches its operators to
            // `impl _ for F` (spec 3.25). uint/int are recognized the same way
            // as any user `struct F : Logic[]`.
            if let ast::Type::Indexed { base, .. } = ty {
                if let ast::Type::Path(p) = base.as_ref() {
                    let head = p.segments.last().map(|s| s.text.as_str()).unwrap_or("");
                    if self.vector_families.contains(head) {
                        self.local_numeric.insert(name.to_string(), head.to_string());
                        if let Some(&id) = self.locals.get(name) {
                            self.sig_type.insert(id.0, head.to_string());
                        }
                    }
                }
            }
            // A `real` slot holds f64 bits and takes float arithmetic; a
            // `Char` slot holds a symbol.
            if let ast::Type::Path(p) = ty {
                if p.segments.len() == 1 {
                    if let Some(&id) = self.locals.get(name) {
                        match p.segments[0].text.as_str() {
                            "real" => self.out.signals[id.0 as usize].real = true,
                            "Char" => {
                                self.out.signals[id.0 as usize].char = true;
                                self.local_char.insert(name.to_string());
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    /// The storage width of a value-range-constrained numeric type
    /// (`integer<lo..hi>` / `real<lo..hi>`), if `ty` is one. Returns
    /// `(width, is_real)`.
    fn ranged_numeric(&self, ty: &ast::Type) -> Option<(u32, bool, Option<(i64, i64)>)> {
        let ast::Type::Generic { base, args, .. } = ty else { return None };
        let ast::Type::Path(p) = base.as_ref() else { return None };
        let kind = p.segments.last().map(|s| s.text.as_str())?;
        if kind != "integer" && kind != "real" {
            return None;
        }
        let [ast::GenericArg::Positional(arg)] = args.as_slice() else { return None };
        if kind == "real" {
            return Some((64, true, None)); // range is a constraint, storage is f64
        }
        let (a, b) = match arg {
            ast::Expr::Range { lo, hi, .. } => {
                (eval_const(lo, &self.cur_env)?, eval_const(hi, &self.cur_env)?)
            }
            ast::Expr::Path(p) if p.segments.len() == 1 => {
                self.const_ranges.get(&p.segments[0].text).copied()?
            }
            _ => return None,
        };
        let (lo, hi) = (a.min(b), a.max(b));
        // Smallest width whose (signed, when lo < 0) domain covers [lo, hi].
        for w in 1..=64u32 {
            let fits = if lo < 0 {
                let half = 1i128 << (w - 1);
                (lo as i128) >= -half && (hi as i128) < half
            } else {
                (hi as i128) < (1i128 << w.min(63)) || w >= 64
            };
            if fits {
                return Some((w, false, Some((lo, hi))));
            }
        }
        Some((64, false, None))
    }

    /// The bit width of `ty` if it names a known enum.
    fn enum_width(&self, ty: &ast::Type) -> Option<u32> {
        match ty {
            ast::Type::Path(p) if p.segments.len() == 1 => {
                self.enum_reprs.get(&p.segments[0].text).copied()
            }
            _ => None,
        }
    }

    /// The `(field name, field type)` list if `ty` names a known struct —
    /// resolving generic applications (`Pair<uint[8]>`) and bus-mode views
    /// (`Stream::Source`, `Stream<uint[8]>::Source`, spec 3.19).
    /// Normalize an instance's connection args into `(port, value)` pairs:
    /// positional args (`Inv { a, b }`) bind by the sub-entity's port order,
    /// name shorthand (`.clk`) expands to `.clk = clk`, and explicit args pass
    /// through. The value is always concrete so downstream sites don't special-
    /// case shorthand/positional.
    fn norm_conns(&self, conns: &[ast::ConnectArg], ename: &str) -> Vec<(String, ast::Expr)> {
        let order: Vec<String> = self
            .entities
            .get(ename)
            .map(|d| d.ports.iter().map(|p| p.name.text.clone()).collect())
            .unwrap_or_default();
        conns
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let port = match &c.field {
                    Some(f) => f.text.clone(),
                    None => order.get(i).cloned()?,
                };
                let value = c.value.clone().unwrap_or_else(|| {
                    ast::Expr::Path(ast::Path {
                        segments: vec![ast::Ident { text: port.clone(), span: c.span }],
                        span: c.span,
                    })
                });
                Some((port, value))
            })
            .collect()
    }

    fn struct_fields(&self, ty: &ast::Type) -> Option<Vec<(String, ast::Type)>> {
        match ty {
            // A generic application: substitute the type parameters into the
            // base struct's field types.
            ast::Type::Generic { base, args, .. } => {
                let sname = type_head_name(base)?;
                let s = self.structs.get(sname)?;
                let mut subst: HashMap<String, ast::Type> = HashMap::new();
                for (param, arg) in s.params.params.iter().zip(args) {
                    if let ast::GenericArg::Positional(e) = arg {
                        if let Some(t) = expr_to_type(e) {
                            subst.insert(param.name.text.clone(), t);
                        }
                    }
                }
                let fields = self.raw_struct_fields(sname)?;
                Some(
                    fields
                        .into_iter()
                        .map(|(n, ft)| (n, subst_type_params(&ft, &subst)))
                        .collect(),
                )
            }
            // A bus-mode view reduces to its inner struct's fields — the inner
            // is a generic application (`Stream<uint[8]>::Source`) or a plain
            // `Struct::Mode` path.
            ast::Type::Mode { inner, .. } => match inner.as_ref() {
                ast::Type::Generic { .. } => self.struct_fields(inner),
                ast::Type::Path(p) => self.raw_struct_fields(p.segments.first()?.text.as_str()),
                _ => None,
            },
            ast::Type::Path(p) if p.segments.len() == 1 => {
                self.raw_struct_fields(&p.segments[0].text)
            }
            _ => None,
        }
    }

    /// The base-first field list of a struct named directly (no generics/mode).
    fn raw_struct_fields(&self, name: &str) -> Option<Vec<(String, ast::Type)>> {
        let s = self.structs.get(name)?;
        // Derived struct: inherited base fields come first (spec: derivation).
        let mut fields = match &s.base {
            Some(b) => self.struct_fields(b).unwrap_or_default(),
            None => Vec::new(),
        };
        fields.extend(s.fields.iter().map(|f| (f.name.text.clone(), f.ty.clone())));
        Some(fields)
    }

    /// The `(struct, mode)` names of a bus-mode type (`out Stream::Source` ->
    /// `("Stream", "Source")`), for looking up per-leaf directions.
    fn mode_of(ty: &ast::Type) -> Option<(String, String)> {
        if let ast::Type::Mode { inner, mode, .. } = ty {
            // Generic form `Stream<..>::Source`: the mode is the `Mode.mode`
            // ident and the struct is the inner's head.
            if let Some(m) = mode {
                return Some((type_head_name(inner)?.to_string(), m.text.clone()));
            }
            // Plain form `Stream::Source`: a two-segment inner path.
            if let ast::Type::Path(p) = inner.as_ref() {
                if p.segments.len() >= 2 {
                    return Some((p.segments[0].text.clone(), p.segments[1].text.clone()));
                }
            }
        }
        None
    }

    /// Lower a top-level (combinational-context) statement. `cond` accumulates
    /// the enclosing combinational conditions.
    fn lower_stmt(&mut self, stmt: &ast::Stmt, cond: Option<Expr>) {
        match stmt {
            ast::Stmt::Assign { target, value, after, span } => {
                // `after` delays are testbench stimulus, not synthesizable
                // hardware (Phase 1): reject rather than silently drop.
                if after.is_some() {
                    self.sink.emit(
                        siox_diag::Diagnostic::error(
                            "`after` delays are only allowed in #[test] testbenches (Phase 1)"
                                .to_string(),
                        )
                        .with_code(siox_diag::codes::TYPE_MISMATCH),
                    );
                }
                // Strict assignment width: a scalar signal target and a direct
                // signal-reference value must have equal, both-known widths
                // (spec 3.17 — no implicit resize). Arithmetic and conversions
                // are exempt (see `ref_width`); array/struct targets aren't in
                // `locals` so they fall through untouched.
                if let Some(tpath) = expr_path(target) {
                    if let Some(&tid) = self.locals.get(&tpath) {
                        let tw = self.out.signals[tid.0 as usize].width;
                        if let Some(sw) = self.ref_width(value) {
                            if tw > 0 && sw > 0 && tw != sw {
                                self.sink.emit(
                                    siox_diag::Diagnostic::error(format!(
                                        "width mismatch: `{tpath}` is {tw} bits but the \
                                         assigned value is {sw} bits"
                                    ))
                                    .with_code(siox_diag::codes::TYPE_MISMATCH)
                                    .at(*span)
                                    .help("widths must match; use a conversion \
                                           (`uint[N](x)` / `resize(x, N)`) to change width"),
                                );
                            }
                        }
                    }
                }
                // A struct-typed target takes one driver per flattened field
                // (struct copy, struct literal, or an inlined operator impl).
                if let Some(tpath) = expr_path(target) {
                    // Whole-array assignment: a string literal fills a Char
                    // array per element; an array of the same shape copies.
                    if let Some(indices) = self.local_array.get(&tpath).cloned() {
                        match value {
                            ast::Expr::StrLit { text, .. } => {
                                let chars: Vec<char> = text.chars().collect();
                                if chars.len() != indices.len() {
                                    self.sink.emit(siox_diag::Diagnostic::error(format!(
                                        "string literal length {} does not match `{tpath}` length {}",
                                        chars.len(),
                                        indices.len()
                                    )));
                                    return;
                                }
                                for (c, i) in chars.iter().zip(&indices) {
                                    if let Some(&sig) = self.locals.get(&format!("{tpath}[{i}]")) {
                                        self.out.drivers.push(Driver {
                                            target: sig,
                                            cond: cond.clone(),
                                            expr: Expr::Const(*c as u32 as u64),
                                            ctx: self.cur_ctx,
                                        });
                                    }
                                }
                                return;
                            }
                            // `a = [e0, e1, ...];` drives one element per value.
                            ast::Expr::Array { elems, .. } => {
                                if elems.len() != indices.len() {
                                    self.sink.emit(siox_diag::Diagnostic::error(format!(
                                        "array literal length {} does not match `{tpath}` length {}",
                                        elems.len(),
                                        indices.len()
                                    )));
                                    return;
                                }
                                for (e, i) in elems.iter().zip(&indices) {
                                    if let Some(&sig) = self.locals.get(&format!("{tpath}[{i}]")) {
                                        let expr = self.coerce_to_target(sig, self.lower_expr(e));
                                        self.out.drivers.push(Driver {
                                            target: sig,
                                            cond: cond.clone(),
                                            expr,
                                            ctx: self.cur_ctx,
                                        });
                                    }
                                }
                                return;
                            }
                            v => {
                                if let Some(vpath) = expr_path(v) {
                                    if let Some(vidx) = self.local_array.get(&vpath).cloned() {
                                        for (ti, vi) in indices.iter().zip(&vidx) {
                                            let t = self.locals.get(&format!("{tpath}[{ti}]"));
                                            let sv = self.locals.get(&format!("{vpath}[{vi}]"));
                                            if let (Some(&t), Some(&sv)) = (t, sv) {
                                                self.out.drivers.push(Driver {
                                                    target: t,
                                                    cond: cond.clone(),
                                                    expr: Expr::Current(sv),
                                                    ctx: self.cur_ctx,
                                                });
                                            }
                                        }
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    if self.local_struct.contains_key(&tpath) {
                        if let Val::Fields(fields) = self.lower_val_env(value, &HashMap::new()) {
                            for (fname, expr) in fields {
                                if let Some(&sig) = self.locals.get(&format!("{tpath}.{fname}")) {
                                    let expr = self.coerce_to_target(sig, expr);
                                    self.out.drivers.push(Driver {
                                        target: sig,
                                        cond: cond.clone(),
                                        expr,
                                        ctx: self.cur_ctx,
                                    });
                                }
                            }
                        }
                        return;
                    }
                }
                if let Some(target) = self.target_signal(target) {
                    let expr = self.coerce_to_target(target, self.lower_expr(value));
                    self.out.drivers.push(Driver { target, cond, expr, ctx: self.cur_ctx });
                } else if let Some(ups) = self.dynamic_write(target, value, &cond) {
                    for u in ups {
                        self.out.drivers.push(Driver {
                            target: u.target,
                            cond: u.cond,
                            expr: u.expr,
                            ctx: self.cur_ctx,
                        });
                    }
                } else if let Some((sig, hi, lo)) = self.slice_target(target) {
                    // Partial write: merge over the prior driver (`y = base;
                    // y[3..0] = a;`), else over 0.
                    let v = self.lower_expr(value);
                    let width = self.out.signals[sig.0 as usize].width;
                    let base = self
                        .out
                        .drivers
                        .iter()
                        .rev()
                        .find(|d| d.target == sig && d.ctx == self.cur_ctx && d.cond.is_none())
                        .map(|d| d.expr.clone())
                        .unwrap_or(Expr::Const(0));
                    let merged = self.merge_slice(base, hi, lo, v, width);
                    if let Some(d) = self
                        .out
                        .drivers
                        .iter_mut()
                        .rev()
                        .find(|d| d.target == sig && d.ctx == self.cur_ctx && d.cond.is_none())
                    {
                        d.expr = merged;
                    } else {
                        self.out.drivers.push(Driver { target: sig, cond, expr: merged, ctx: self.cur_ctx });
                    }
                } else if let ast::Expr::Concat { parts, .. } = target {
                    // `{hi, lo} = w;` unpacks the value MSB-first: each part
                    // takes its width's slice of the RHS.
                    let v = self.lower_expr(value);
                    let mut off: u32 = parts.iter().map(|p| self.ast_width(p)).sum();
                    for part in parts {
                        let w = self.ast_width(part);
                        let Some(t) = self.target_signal(part) else {
                            self.sink.emit(siox_diag::Diagnostic::error(
                                "each part of a concat assignment target must be a signal"
                                    .to_string(),
                            ));
                            continue;
                        };
                        let expr = Expr::Slice {
                            base: Box::new(v.clone()),
                            hi: off - 1,
                            lo: off - w,
                        };
                        self.out.drivers.push(Driver {
                            target: t,
                            cond: cond.clone(),
                            expr,
                            ctx: self.cur_ctx,
                        });
                        off -= w;
                    }
                } else {
                    // Anything else is a target shape the lowering doesn't
                    // understand — say so rather than silently dropping it.
                    self.sink.emit(siox_diag::Diagnostic::error(format!(
                        "cannot lower this assignment target: `{}`",
                        siox_syntax::pretty::expr_string(target)
                    )));
                }
            }
            ast::Stmt::If(iff) => {
                if expr_is_event(&iff.cond) {
                    // Event-controlled block (spec 3.11): the body's assignments
                    // become next-state updates (spec 3.13).
                    let condition = self.lower_expr(&iff.cond);
                    let mut updates = Vec::new();
                    self.lower_event_block(&iff.then, None, &mut updates);
                    // An `else` on an event block is unusual; lower it under the
                    // negated event for completeness.
                    if let Some(eb) = iff.else_.as_deref() {
                        let neg = Some(not(self.lower_expr(&iff.cond)));
                        self.lower_event_else(eb, neg, &mut updates);
                    }
                    self.out.event_blocks.push(EventBlock { condition, updates });
                } else {
                    // A signal assigned on every path through this if/else (a
                    // terminal `else` supplies the complement) is fully covered
                    // — not a latch — even though each driver is conditional.
                    // Mark it like a wildcard match arm so the possible-latch
                    // lint skips it.
                    for id in self.if_covered_targets(iff) {
                        self.lint_defaulted.insert(id);
                    }
                    // Combinational conditional: assignments become conditional
                    // drivers; the `else` adds the negated condition.
                    let c = self.lower_expr(&iff.cond);
                    let then_cond = Some(and(cond.clone(), c.clone()));
                    for s in &iff.then.stmts {
                        self.lower_stmt(s, then_cond.clone());
                    }
                    if let Some(eb) = iff.else_.as_deref() {
                        let else_cond = Some(and(cond, not(c)));
                        self.lower_combinational_else(eb, else_cond);
                    }
                }
            }
            ast::Stmt::Match(m) => {
                // Combinational match: each arm becomes conditional drivers
                // guarded by `scrutinee == variant` with first-match priority.
                let scrut = self.lower_expr(&m.scrutinee);
                let mut remaining = cond;
                for arm in &m.arms {
                    let mc = self.arm_match_cond(&arm.pattern, &scrut);
                    // A wildcard arm is the match's default branch: its direct
                    // assignments cover "everything else", so those targets are
                    // not latches even though the lowered driver is conditional.
                    if mc.is_none() {
                        for s in &arm.body.stmts {
                            if let ast::Stmt::Assign { target, .. } = s {
                                if let Some(id) = self.target_signal(target) {
                                    self.lint_defaulted.insert(id.0);
                                }
                            }
                        }
                    }
                    let fire = match &mc {
                        Some(c) => Some(and(remaining.clone(), c.clone())),
                        None => remaining.clone(),
                    };
                    for s in &arm.body.stmts {
                        self.lower_stmt(s, fire.clone());
                    }
                    remaining = match mc {
                        Some(c) => Some(and(remaining, not(c))),
                        None => Some(Expr::Const(0)),
                    };
                }
            }
            // A method call used as a statement (`s.send(v)`): inline the
            // method body as drivers on the receiver's signals (spec 3.20).
            ast::Stmt::Expr(ast::Expr::Call { callee, args, .. })
                if matches!(callee.as_ref(), ast::Expr::Field { .. }) =>
            {
                if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
                    self.lower_method_stmt(base, &field.text, args, cond);
                }
            }
            // Other statement forms (for, let, expr, return) are not lowered yet.
            _ => {}
        }
    }

    /// The condition under which a match arm fires: `scrut == <variant value>`
    /// for an enum path, `(scrut & mask) == value` for a bit pattern with `?`
    /// don't-cares (spec 3.22), or always (`None`) for a wildcard.
    /// Lower a match-*expression* to a first-match `Select` chain: the wildcard
    /// arm's value is the base `els`, and each earlier arm wraps it under its
    /// `scrutinee == pattern` guard.
    fn lower_match_expr(&self, scrutinee: &ast::Expr, arms: &[ast::MatchArm]) -> Expr {
        let scrut = self.lower_expr(scrutinee);
        let mut result: Option<Expr> = None;
        for arm in arms.iter().rev() {
            let val = arm.value_expr().map(|v| self.lower_expr(v)).unwrap_or(Expr::Unknown);
            match self.arm_match_cond(&arm.pattern, &scrut) {
                None => result = Some(val), // wildcard: the default branch
                Some(cond) => {
                    let els = result.take().unwrap_or(Expr::Unknown);
                    result = Some(Expr::Select {
                        cond: Box::new(cond),
                        then: Box::new(val),
                        els: Box::new(els),
                    });
                }
            }
        }
        result.unwrap_or(Expr::Unknown)
    }

    fn arm_match_cond(&self, pattern: &ast::Pattern, scrut: &Expr) -> Option<Expr> {
        match pattern {
            ast::Pattern::Path(p) if p.segments.len() >= 2 => {
                let disc = self
                    .enum_variants
                    .get(&p.segments[0].text)
                    .and_then(|m| m.get(&p.segments[1].text))
                    .copied()
                    .unwrap_or(0);
                Some(eq(scrut.clone(), Expr::Const(disc)))
            }
            ast::Pattern::BitPattern { text, .. } => {
                let (mask, value) = bit_pattern_mask(text)?;
                Some(eq(
                    Expr::Binary {
                        op: BinOp::And,
                        lhs: Box::new(scrut.clone()),
                        rhs: Box::new(Expr::Const(mask)),
                    },
                    Expr::Const(value),
                ))
            }
            // `A | B`: matches if any alternative matches (their conditions
            // OR-ed; a wildcard alternative makes the whole arm unconditional).
            ast::Pattern::Or { alts, .. } => {
                let mut acc: Option<Expr> = None;
                for a in alts {
                    match self.arm_match_cond(a, scrut) {
                        None => return None,
                        Some(c) => {
                            acc = Some(match acc {
                                Some(prev) => Expr::Binary {
                                    op: BinOp::Or,
                                    lhs: Box::new(prev),
                                    rhs: Box::new(c),
                                },
                                None => c,
                            })
                        }
                    }
                }
                acc
            }
            // An integer literal or inclusive range: `scrut == lo`, or
            // `lo <= scrut <= hi`.
            ast::Pattern::Range { lo, hi, .. } => {
                if lo == hi {
                    Some(eq(scrut.clone(), Expr::Const(*lo as u64)))
                } else {
                    let ge = Expr::Binary {
                        op: BinOp::Ge,
                        lhs: Box::new(scrut.clone()),
                        rhs: Box::new(Expr::Const(*lo as u64)),
                    };
                    let le = Expr::Binary {
                        op: BinOp::Le,
                        lhs: Box::new(scrut.clone()),
                        rhs: Box::new(Expr::Const(*hi as u64)),
                    };
                    Some(and(Some(ge), le))
                }
            }
            // A wildcard matches anything.
            _ => None,
        }
    }

    fn lower_combinational_else(&mut self, eb: &ast::ElseBranch, cond: Option<Expr>) {
        match eb {
            ast::ElseBranch::Block(b) => {
                for s in &b.stmts {
                    self.lower_stmt(s, cond.clone());
                }
            }
            ast::ElseBranch::If(inner) => {
                self.lower_stmt(&ast::Stmt::If(inner.clone()), cond);
            }
        }
    }

    /// Lower the body of an event-controlled block into next-state updates,
    /// accumulating the priority condition through nested `if`/`else`.
    fn lower_event_block(&mut self, block: &ast::Block, cond: Option<Expr>, out: &mut Vec<NextUpdate>) {
        for s in &block.stmts {
            match s {
                ast::Stmt::Assign { target, value, after, .. } => {
                    if after.is_some() {
                        self.sink.emit(
                            siox_diag::Diagnostic::error(
                                "`after` delays are only allowed in #[test] testbenches (Phase 1)"
                                    .to_string(),
                            )
                            .with_code(siox_diag::codes::TYPE_MISMATCH),
                        );
                    }
                    if let Some(target) = self.target_signal(target) {
                        let expr = self.lower_expr(value);
                        out.push(NextUpdate { target, cond: cond.clone(), expr });
                    } else if let Some(ups) = self.dynamic_write(target, value, &cond) {
                        out.extend(ups);
                    } else if let Some((sig, hi, lo)) = self.slice_target(target) {
                        // Register bit-field update: next(y) holds the other
                        // bits (read-modify-write on the current value).
                        let v = self.lower_expr(value);
                        let width = self.out.signals[sig.0 as usize].width;
                        let expr = self.merge_slice(Expr::Current(sig), hi, lo, v, width);
                        out.push(NextUpdate { target: sig, cond: cond.clone(), expr });
                    } else if let ast::Expr::Concat { parts, .. } = target {
                        // `{hi, lo} = w;` in a clocked block: each part takes
                        // its width's slice of the RHS, MSB-first.
                        let v = self.lower_expr(value);
                        let mut off: u32 = parts.iter().map(|p| self.ast_width(p)).sum();
                        for part in parts {
                            let w = self.ast_width(part);
                            let Some(t) = self.target_signal(part) else {
                                self.sink.emit(siox_diag::Diagnostic::error(
                                    "each part of a concat assignment target must be a signal"
                                        .to_string(),
                                ));
                                continue;
                            };
                            let expr = Expr::Slice {
                                base: Box::new(v.clone()),
                                hi: off - 1,
                                lo: off - w,
                            };
                            out.push(NextUpdate { target: t, cond: cond.clone(), expr });
                            off -= w;
                        }
                    } else {
                        self.sink.emit(siox_diag::Diagnostic::error(format!(
                            "cannot lower this assignment target: `{}`",
                            siox_syntax::pretty::expr_string(target)
                        )));
                    }
                }
                ast::Stmt::If(iff) => {
                    let c = self.lower_expr(&iff.cond);
                    self.lower_event_block(&iff.then, Some(and(cond.clone(), c.clone())), out);
                    if let Some(eb) = iff.else_.as_deref() {
                        let neg = Some(and(cond.clone(), not(c)));
                        self.lower_event_else(eb, neg, out);
                    }
                }
                ast::Stmt::Match(m) => {
                    let scrut = self.lower_expr(&m.scrutinee);
                    let mut remaining = cond.clone();
                    for arm in &m.arms {
                        let mc = self.arm_match_cond(&arm.pattern, &scrut);
                        let fire = match &mc {
                            Some(c) => Some(and(remaining.clone(), c.clone())),
                            None => remaining.clone(),
                        };
                        self.lower_event_block(&arm.body, fire, out);
                        remaining = match mc {
                            Some(c) => Some(and(remaining, not(c))),
                            None => Some(Expr::Const(0)),
                        };
                    }
                }
                _ => {}
            }
        }
    }

    fn lower_event_else(&mut self, eb: &ast::ElseBranch, cond: Option<Expr>, out: &mut Vec<NextUpdate>) {
        match eb {
            ast::ElseBranch::Block(b) => self.lower_event_block(b, cond, out),
            ast::ElseBranch::If(inner) => {
                let c = self.lower_expr(&inner.cond);
                self.lower_event_block(&inner.then, Some(and(cond.clone(), c.clone())), out);
                if let Some(eb) = inner.else_.as_deref() {
                    self.lower_event_else(eb, Some(and(cond, not(c))), out);
                }
            }
        }
    }

    /// The signal an assignment target refers to — a bare name or a struct-field
    /// path (`s.data`).
    /// A dynamic array read `mem[addr]`: select among the flattened element
    /// signals by the runtime index — `addr==0 ? mem[0] : addr==1 ? mem[1] :
    /// ... : mem[last]`. Out-of-range reads the last element (defined, not UB).
    fn lower_dynamic_read(&self, base: &ast::Expr, index: &ast::Expr) -> Option<Expr> {
        let bpath = expr_path(base)?;
        let indices = self.local_array.get(&bpath)?.clone();
        let (&last, rest) = indices.split_last()?;
        let idx = self.lower_expr(index);
        let elem = |i: i64| {
            self.locals
                .get(&format!("{bpath}[{i}]"))
                .map(|&s| Expr::Current(s))
                .unwrap_or(Expr::Unknown)
        };
        let mut acc = elem(last);
        for &i in rest.iter().rev() {
            acc = Expr::Select {
                cond: Box::new(Expr::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(idx.clone()),
                    rhs: Box::new(Expr::Const(i as u64)),
                }),
                then: Box::new(elem(i)),
                els: Box::new(acc),
            };
        }
        Some(acc)
    }

    /// A dynamic array write `mem[addr] = v`: update EVERY element,
    /// each gated by `addr == i` (and the enclosing condition). One element
    /// takes the new value; the rest hold (a `None` cond means unconditional,
    /// so we always attach the match condition).
    fn dynamic_write(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
        cond: &Option<Expr>,
    ) -> Option<Vec<NextUpdate>> {
        let ast::Expr::Index { base, index, .. } = target else { return None };
        let bpath = expr_path(base)?;
        let indices = self.local_array.get(&bpath)?.clone();
        let idx = self.lower_expr(index);
        let expr = self.lower_expr(value);
        let mut updates = Vec::new();
        for i in indices {
            let sig = *self.locals.get(&format!("{bpath}[{i}]"))?;
            let hit = Expr::Binary {
                op: BinOp::Eq,
                lhs: Box::new(idx.clone()),
                rhs: Box::new(Expr::Const(i as u64)),
            };
            updates.push(NextUpdate {
                target: sig,
                cond: Some(and(cond.clone(), hit)),
                expr: self.coerce_to_target(sig, expr.clone()),
            });
        }
        Some(updates)
    }

    /// A slice-assignment target `y[hi..lo]`: the base signal and the
    /// (normalized) bit range.
    fn slice_target(&self, target: &ast::Expr) -> Option<(SignalId, u32, u32)> {
        let ast::Expr::Index { base, index, .. } = target else { return None };
        let (a, b) = self.slice_bounds(index)?;
        let sig = *self.locals.get(&expr_path(base)?)?;
        Some((sig, a.max(b) as u32, a.min(b) as u32))
    }

    /// A partial (bit-slice) write as a read-modify-write over `base`:
    /// `(base & keep) | ((value & slice_mask) << lo)`, where `keep` clears the
    /// [hi..lo] window. `width` is the target signal's width.
    fn merge_slice(&self, base: Expr, hi: u32, lo: u32, value: Expr, width: u32) -> Expr {
        let slice_w = hi - lo + 1;
        let slice_mask = if slice_w >= 64 { u64::MAX } else { (1u64 << slice_w) - 1 };
        let full = if width == 0 || width >= 64 { u64::MAX } else { (1u64 << width) - 1 };
        let keep = full & !(slice_mask << lo);
        let kept = Expr::Binary {
            op: BinOp::And,
            lhs: Box::new(base),
            rhs: Box::new(Expr::Const(keep)),
        };
        let masked = Expr::Binary {
            op: BinOp::And,
            lhs: Box::new(value),
            rhs: Box::new(Expr::Const(slice_mask)),
        };
        let shifted = Expr::Binary {
            op: BinOp::Shl,
            lhs: Box::new(masked),
            rhs: Box::new(Expr::Const(lo as u64)),
        };
        Expr::Binary { op: BinOp::Or, lhs: Box::new(kept), rhs: Box::new(shifted) }
    }

    fn target_signal(&self, target: &ast::Expr) -> Option<SignalId> {
        expr_path(target).and_then(|p| self.locals.get(&p).copied())
    }

    /// Signals assigned on *every* path through an if/else — a terminal `else`
    /// supplies the complement, so these are fully covered and are not latches
    /// even though each driver is conditional. Without a terminal `else` the
    /// fall-through path assigns nothing, so nothing is covered. An
    /// event-controlled branch is sequential (not a combinational latch).
    fn if_covered_targets(&self, iff: &ast::IfStmt) -> std::collections::BTreeSet<u32> {
        use std::collections::BTreeSet;
        if expr_is_event(&iff.cond) {
            return BTreeSet::new();
        }
        let then = self.block_covered_targets(&iff.then);
        let els = match iff.else_.as_deref() {
            Some(ast::ElseBranch::Block(b)) => self.block_covered_targets(b),
            Some(ast::ElseBranch::If(inner)) => self.if_covered_targets(inner),
            None => return BTreeSet::new(),
        };
        then.intersection(&els).copied().collect()
    }

    /// Signals a block assigns on every path: its direct assignment targets,
    /// plus any target fully covered by a nested if/else.
    fn block_covered_targets(&self, b: &ast::Block) -> std::collections::BTreeSet<u32> {
        let mut out = std::collections::BTreeSet::new();
        for s in &b.stmts {
            match s {
                ast::Stmt::Assign { target, .. } => {
                    if let Some(id) = self.target_signal(target) {
                        out.insert(id.0);
                    }
                }
                ast::Stmt::If(inner) => out.extend(self.if_covered_targets(inner)),
                _ => {}
            }
        }
        out
    }

    fn lower_expr(&self, e: &ast::Expr) -> Expr {
        match e {
            ast::Expr::Call { callee, args, .. } => self
                .lower_conversion(callee, args, &HashMap::new())
                .or_else(|| self.lower_free_call(callee, args, &HashMap::new()))
                .or_else(|| match self.lower_method_call(callee, args, &HashMap::new()) {
                    Some(Val::Scalar(v)) => Some(v),
                    _ => None,
                })
                .or_else(|| match self.lower_from(callee, args, &HashMap::new()) {
                    Some(Val::Scalar(v)) => Some(v),
                    _ => None,
                })
                .unwrap_or(Expr::Unknown),
            // `if c { a } else { b }` is a mux: lower to a select.
            ast::Expr::IfExpr { cond, then, els, .. } => Expr::Select {
                cond: Box::new(self.lower_expr(cond)),
                then: Box::new(self.lower_expr(then)),
                els: Box::new(self.lower_expr(els)),
            },
            // A match-expression is a first-match `Select` chain over the arms.
            ast::Expr::Match { scrutinee, arms, .. } => self.lower_match_expr(scrutinee, arms),
            // A decimal point makes it a `real` literal (`1.5`).
            ast::Expr::Int { text, .. } if text.contains('.') => {
                Expr::Real(text.parse().unwrap_or(0.0))
            }
            ast::Expr::Int { text, .. } => Expr::Const(parse_int(text).unwrap_or(0)),
            // A suffix with an `impl Suffix` fn inlines it (scalar results
            // only here; struct results flow through `lower_val_env`).
            // Otherwise `1ns` / `10MHz` scale by the fixed fs/Hz table.
            ast::Expr::SuffixLit { text, suffix, .. } => match self.inline_suffix(e) {
                Some(Val::Scalar(v)) => v,
                Some(Val::Fields(_)) => Expr::Unknown,
                None => Expr::Const(
                    parse_int(text)
                        .map(|v| {
                            v.saturating_mul(ast::suffix_scale(&suffix.text).unwrap_or(1) as u64)
                        })
                        .unwrap_or(0),
                ),
            },
            ast::Expr::BitStrLit { base, digits, .. } => Expr::Const(
                u64::from_str_radix(digits, if *base == 'x' { 16 } else { 2 }).unwrap_or(0),
            ),
            ast::Expr::Bool { value, .. } => Expr::Const(*value as u64),
            ast::Expr::LogicLit { ch, .. } => Expr::Logic(*ch),
            ast::Expr::Path(p) if p.segments.len() == 1 => {
                let name = &p.segments[0].text;
                if let Some(id) = self.locals.get(name) {
                    return Expr::Current(*id);
                }
                // Module constants read as values (`x * PI`).
                if let Some(&v) = self.cur_env.get(name) {
                    return Expr::Const(v as u64);
                }
                if let Some(&f) = self.consts_real.get(name) {
                    return Expr::Real(f);
                }
                Expr::Unknown
            }
            // `Enum::Variant` lowers to its discriminant constant.
            ast::Expr::Path(p) if p.segments.len() >= 2 => self
                .enum_variants
                .get(&p.segments[0].text)
                .and_then(|m| m.get(&p.segments[1].text))
                .map(|&d| Expr::Const(d))
                .unwrap_or(Expr::Unknown),
            // A bit slice `base[a..b]` (constant bounds, possibly a named
            // range constant). Direction follows the written order: `7..4`
            // (descending) extracts MSB-first — the natural bit order —
            // while `4..7` (ascending) extracts with the bit order reversed.
            ast::Expr::Index { base, index, .. } if self.slice_bounds(index).is_some() => {
                let (a, b) = self.slice_bounds(index).unwrap();
                let lowered = self.lower_expr(base);
                if a >= b {
                    Expr::Slice { base: Box::new(lowered), hi: a as u32, lo: b as u32 }
                } else {
                    // Ascending: reassemble bits a..=b with significance
                    // reversed: source bit (a+k) lands at result bit (w-1-k).
                    let w = (b - a + 1) as u32;
                    let mut acc = Expr::Const(0);
                    for k in 0..w {
                        let bit = Expr::Slice {
                            base: Box::new(lowered.clone()),
                            hi: a as u32 + k,
                            lo: a as u32 + k,
                        };
                        let shifted = Expr::Binary {
                            op: BinOp::Shl,
                            lhs: Box::new(bit),
                            rhs: Box::new(Expr::Const((w - 1 - k) as u64)),
                        };
                        acc = Expr::Binary {
                            op: BinOp::Add,
                            lhs: Box::new(acc),
                            rhs: Box::new(shifted),
                        };
                    }
                    acc
                }
            }
            // A struct-field (`s.data`) or constant array-element (`a[2]`) access
            // resolves to its flattened signal; a *dynamic* array index
            // (`mem[addr]`) becomes a mux tree over the element signals.
            ast::Expr::Field { .. } | ast::Expr::Index { .. } => {
                if let Some(id) = expr_path(e).and_then(|p| self.locals.get(&p).copied()) {
                    return Expr::Current(id);
                }
                if let ast::Expr::Index { base, index, .. } = e {
                    if let Some(v) = self.lower_dynamic_read(base, index) {
                        return v;
                    }
                }
                Expr::Unknown
            }
            ast::Expr::SysAttr { base, attr, .. } => self.lower_sysattr(base, &attr.text),
            ast::Expr::Unary { op, rhs, .. } => {
                // `not` on an enum-typed operand inlines its impl (`impl
                // "not" for Logic`), like binary operators.
                if *op == ast::UnOp::Not {
                    if let Some(Val::Scalar(v)) = self.inline_unary("not", rhs) {
                        return v;
                    }
                    // "Boolean per bit": `not` on a vector-valued signal
                    // reference (name, field, element, slice) inverts every
                    // bit — lower to `x xor mask` so the engines need no
                    // width knowledge. A 1-bit operand keeps the boolean
                    // form (same 0<->1 either way), as do compound
                    // expressions (`not (a == b)`) and enum-typed signals
                    // (their `not` is the impl above, or undefined).
                    let is_vector_ref = match rhs.as_ref() {
                        // A slice is always a bit vector.
                        ast::Expr::Index { index, .. }
                            if self.slice_bounds(index).is_some() =>
                        {
                            true
                        }
                        ast::Expr::Path(_) | ast::Expr::Field { .. } | ast::Expr::Index { .. } => {
                            expr_path(rhs)
                                .and_then(|p| self.locals.get(&p))
                                .map(|&id| self.out.signals[id.0 as usize].enum_type.is_none())
                                .unwrap_or(false)
                        }
                        _ => false,
                    };
                    if is_vector_ref {
                        let w = self.ast_width(rhs);
                        if w > 1 && w <= 64 {
                            let mask =
                                if w == 64 { u64::MAX } else { (1u64 << w) - 1 };
                            return Expr::Binary {
                                op: BinOp::Sub,
                                lhs: Box::new(Expr::Const(mask)),
                                rhs: Box::new(self.lower_expr(rhs)),
                            };
                        }
                    }
                }
                Expr::Unary { op: lower_unop(*op), rhs: Box::new(self.lower_expr(rhs)) }
            }
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                // An operator on an enum/struct-typed operand inlines its
                // operator-trait impl body (spec 3.25); `==`/`!=` stay
                // built-in discriminant comparison unless `<=>` derives them.
                let op_str = siox_syntax::pretty::bin_op(op);
                if !matches!(op_str, "==" | "!=") {
                    if let Some(Val::Scalar(inlined)) =
                        self.inline_op(op_str, lhs, rhs, &HashMap::new())
                    {
                        return inlined;
                    }
                }
                if let Some(derived) = self.inline_cmp(op_str, lhs, rhs, &HashMap::new()) {
                    return derived;
                }
                let (mut l, mut r) = (self.lower_expr(lhs), self.lower_expr(rhs));
                // A character literal's identity comes from its counterpart's
                // type (`c == 'x'` with c: Char reads 'x' as Unicode).
                if let ast::Expr::LogicLit { ch, .. } = lhs.as_ref() {
                    if let Some(v) = self.typed_char_literal(*ch, rhs) {
                        l = v;
                    }
                }
                if let ast::Expr::LogicLit { ch, .. } = rhs.as_ref() {
                    if let Some(v) = self.typed_char_literal(*ch, lhs) {
                        r = v;
                    }
                }
                self.make_binary(op.clone(), l, r)
            }
            // `{a, b, c}`: fold into `(((0 << w_a) + a) << w_b) + b ...`. Parts
            // don't overlap, so `+` acts as bitwise-or. First part is the MSBs.
            ast::Expr::Concat { parts, .. } => {
                let mut acc = Expr::Const(0);
                for part in parts {
                    let w = self.ast_width(part);
                    let e = self.lower_expr(part);
                    let shifted =
                        Expr::Binary { op: BinOp::Shl, lhs: Box::new(acc), rhs: Box::new(Expr::Const(w as u64)) };
                    acc = Expr::Binary { op: BinOp::Add, lhs: Box::new(shifted), rhs: Box::new(e) };
                }
                acc
            }
            _ => Expr::Unknown,
        }
    }

    /// The bit width of a source expression, for sizing concatenations. A nested
    /// concat sums its parts; a slice is its span; a signal/field/element is its
    /// declared width; a literal is its minimal width.
    /// The width of a *direct width-bearing reference* on the RHS of an
    /// assignment — a signal name, struct field, constant array element, bit
    /// slice, or concatenation — for the strict assignment-width check. Returns
    /// `None` for everything else (arithmetic, literals, conversions, muxes,
    /// calls): those are exempt because operator results are not auto-widened
    /// (overflow wraps at the operand width; a different width is an explicit
    /// `resize`), so only signal-to-signal width equality is enforced.
    fn ref_width(&self, e: &ast::Expr) -> Option<u32> {
        match e {
            ast::Expr::Path(_) | ast::Expr::Field { .. } => {
                let p = expr_path(e)?;
                self.locals.get(&p).map(|&id| self.out.signals[id.0 as usize].width)
            }
            ast::Expr::Index { index, .. } if self.slice_bounds(index).is_some() => {
                let (a, b) = self.slice_bounds(index)?;
                Some((a.max(b) - a.min(b) + 1) as u32)
            }
            ast::Expr::Index { .. } => {
                // A constant element index (`v[2]`) reads its element signal.
                let p = expr_path(e)?;
                self.locals.get(&p).map(|&id| self.out.signals[id.0 as usize].width)
            }
            ast::Expr::Concat { parts, .. } => {
                Some(parts.iter().map(|p| self.ast_width(p)).sum())
            }
            _ => None,
        }
    }

    fn ast_width(&self, e: &ast::Expr) -> u32 {
        match e {
            ast::Expr::IfExpr { then, .. } => self.ast_width(then),
            // A conversion is as wide as its target (64 for kernel integer).
            ast::Expr::Call { callee, args, .. } => match callee.as_ref() {
                ast::Expr::Index { base, index, .. }
                    if expr_path(base)
                        .as_deref()
                        .is_some_and(|h| self.vector_families.contains(h)) =>
                {
                    eval_const(index, &self.cur_env).map(|w| w as u32).unwrap_or(64)
                }
                ast::Expr::Path(p)
                    if p.segments.len() == 1 && p.segments[0].text == "resize" =>
                {
                    args.get(1)
                        .and_then(|n| eval_const(n, &self.cur_env))
                        .map(|w| w as u32)
                        .unwrap_or(64)
                }
                _ => 64,
            },
            ast::Expr::Concat { parts, .. } => parts.iter().map(|p| self.ast_width(p)).sum(),
            ast::Expr::Index { index, .. } if self.slice_bounds(index).is_some() => {
                let (a, b) = self.slice_bounds(index).unwrap();
                (a.max(b) - a.min(b) + 1) as u32
            }
            ast::Expr::Int { text, .. } => {
                (u64::BITS - parse_int(text).unwrap_or(0).leading_zeros()).max(1)
            }
            // A bit-string literal has an explicit digit-count width.
            ast::Expr::BitStrLit { base, digits, .. } => {
                (digits.len() as u32 * if *base == 'x' { 4 } else { 1 }).max(1)
            }
            // A signal reference (name, struct field, constant array element).
            _ => expr_path(e)
                .and_then(|p| self.locals.get(&p))
                .map(|&id| self.out.signals[id.0 as usize].width)
                .unwrap_or(1),
        }
    }

    /// Inline the operator-trait impl body for `lhs OP rhs` when the left
    /// operand is an enum- or struct-typed local with a matching impl. The
    /// body must be a pure expression tree: `return e;` or `if c { .. } else
    /// { .. }` chains ending in returns (which become [`Expr::Select`], per
    /// field for struct values). `None` falls back to built-in lowering.
    // ponytail: operand types come from the outer locals, so `self + rhs`
    // nested *inside* an impl body doesn't re-inline; loops/match in bodies
    // unsupported until needed.
    fn inline_op(
        &self,
        op: &str,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let lhs_ty = self.operand_type_name(lhs)?;
        let rhs_ty = self.operand_type_name(rhs);
        // `a + b` dispatches to the Rust-style trait (`Add`), spec 3.25.
        let tr = siox_syntax::ast::op_trait_name(op).unwrap_or(op);
        let fns = self.op_impls.get(&(tr.to_string(), lhs_ty.clone()))?;

        // Overload selection. Each candidate's declared rhs type is the
        // impl's trait argument (`impl Add<integer>`) or the fn's rhs
        // parameter type, with `Self` reading as the impl target. Pass 1:
        // exact rhs match. Pass 2: an `integer` operand (a literal) coerces
        // to a Self-typed rhs (`a + 1`). A sole candidate is accepted only
        // when the rhs operand's type is unknown — never on a known mismatch
        // (so `10 + x` with x: uint does not inline a Complex impl).
        let declared = |f: &ast::FnDecl, rhs_arg: &Option<String>| -> Option<String> {
            let d = rhs_arg.clone().or_else(|| {
                f.params
                    .iter()
                    .find(|p| !p.is_self)
                    .and_then(|p| p.ty.as_ref())
                    .and_then(type_head_name)
                    .map(str::to_string)
            })?;
            Some(if d == "Self" { lhs_ty.clone() } else { d })
        };
        let f = match &rhs_ty {
            Some(r) => fns
                .iter()
                .find(|(f, a)| declared(f, a).as_deref() == Some(r.as_str()))
                .or_else(|| {
                    if r == "integer" {
                        fns.iter().find(|(f, a)| declared(f, a).as_deref() == Some(lhs_ty.as_str()))
                    } else {
                        None
                    }
                }),
            None => {
                if fns.len() == 1 {
                    fns.first()
                } else {
                    None
                }
            }
        };
        let (f, _) = f?;
        let body = f.body.as_ref()?;

        // Bind `self` to the left operand and the first named param to the
        // right — plus each operand's bit width, so a body can say
        // `self::width` (needed for e.g. sign-aware `int` comparison).
        let mut fenv: HashMap<String, Val> = HashMap::new();
        fenv.insert("self".to_string(), self.lower_val_env(lhs, env));
        fenv.insert("self::width".to_string(), Val::Scalar(Expr::Const(self.ast_width(lhs) as u64)));
        if let Some(p) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &p.name {
                fenv.insert(n.text.clone(), self.lower_val_env(rhs, env));
                fenv.insert(
                    format!("{}::width", n.text),
                    Val::Scalar(Expr::Const(self.ast_width(rhs) as u64)),
                );
            }
        }
        self.inline_block(&body.stmts, &fenv)
    }

    /// The written (left, right) constant bounds of a slice index: a range
    /// expression with const-evaluable bounds, or a named range constant.
    fn slice_bounds(&self, index: &ast::Expr) -> Option<(i64, i64)> {
        match index {
            ast::Expr::Range { lo, hi, .. } => Some((
                eval_const(lo, &self.cur_env)?,
                eval_const(hi, &self.cur_env)?,
            )),
            ast::Expr::Path(p) if p.segments.len() == 1 => {
                self.const_ranges.get(&p.segments[0].text).copied()
            }
            _ => None,
        }
    }

    /// One alias hop, for inspecting a declared type's shape.
    fn resolve_alias_shallow<'t>(&'t self, ty: &'t ast::Type) -> &'t ast::Type {
        if let ast::Type::Path(p) = ty {
            if p.segments.len() == 1 {
                if let Some(t) = self.aliases.get(&p.segments[0].text) {
                    return t;
                }
            }
        }
        ty
    }

    /// Declare `name` as a `Char[n]` array (string-literal inference).
    fn add_char_array(&mut self, entity: &str, name: &str, n: usize) {
        self.local_array.insert(name.to_string(), (0..n as i64).collect());
        for i in 0..n {
            let elem = format!("{name}[{i}]");
            self.add_signal(entity, &elem, 32);
            if let Some(&id) = self.locals.get(&elem) {
                self.out.signals[id.0 as usize].char = true;
            }
            self.local_char.insert(elem);
        }
    }

    /// Resolve a character literal against its counterpart's type (the
    /// literal has no identity of its own): a `Char` counterpart reads it
    /// through the Unicode table (code point); an enum counterpart reads it
    /// as the matching variant. `None` keeps the default logic-literal form.
    fn typed_char_literal(&self, c: char, other: &ast::Expr) -> Option<Expr> {
        let t = self.operand_type_name(other)?;
        if t == "Char" {
            return Some(Expr::Const(c as u32 as u64));
        }
        let vars = self.enum_variants.get(&t)?;
        vars.get(&format!("'{c}'")).map(|&d| Expr::Const(d))
    }

    /// Coerce a driven value to the target's representation: integer
    /// constants become f64 bits when the target signal is `real`.
    fn coerce_to_target(&self, target: SignalId, expr: Expr) -> Expr {
        let sig = &self.out.signals[target.0 as usize];
        if sig.char {
            if let Expr::Logic(c) = expr {
                return Expr::Const(c as u32 as u64);
            }
        }
        if sig.real {
            self.coerce_real(expr)
        } else {
            expr
        }
    }

    /// Whether a lowered expression produces f64-bit (`real`) values.
    fn is_real_expr(&self, e: &Expr) -> bool {
        match e {
            Expr::Real(_) => true,
            Expr::Current(id) | Expr::Old(id) => self.out.signals[id.0 as usize].real,
            Expr::Binary { op, .. } => {
                matches!(op, BinOp::FAdd | BinOp::FSub | BinOp::FMul | BinOp::FDiv)
            }
            Expr::Select { then, els, .. } => self.is_real_expr(then) || self.is_real_expr(els),
            Expr::CCall { f64_ret, .. } => *f64_ret,
            _ => false,
        }
    }

    /// Reinterpret an integer value flowing into a real context (`.re = 10`,
    /// `self.re + 3`, a constant-folded `10 + 0`) as its f64 form: constants
    /// convert, integer arithmetic becomes float arithmetic, selects recurse.
    fn coerce_real(&self, e: Expr) -> Expr {
        if self.is_real_expr(&e) {
            return e;
        }
        match e {
            Expr::Const(v) => Expr::Real(v as f64),
            Expr::Select { cond, then, els } => Expr::Select {
                cond,
                then: Box::new(self.coerce_real(*then)),
                els: Box::new(self.coerce_real(*els)),
            },
            Expr::Binary { op, lhs, rhs } => {
                let fop = match op {
                    BinOp::Add => Some(BinOp::FAdd),
                    BinOp::Sub => Some(BinOp::FSub),
                    BinOp::Mul => Some(BinOp::FMul),
                    BinOp::Div => Some(BinOp::FDiv),
                    _ => None,
                };
                match fop {
                    Some(f) => Expr::Binary {
                        op: f,
                        lhs: Box::new(self.coerce_real(*lhs)),
                        rhs: Box::new(self.coerce_real(*rhs)),
                    },
                    None => Expr::Binary { op, lhs, rhs },
                }
            }
            e => e,
        }
    }

    /// Build a binary node, switching `+ - * /` to float arithmetic (and
    /// coercing integer constants) when either operand is real. `==`/`!=`
    /// compare f64 bits exactly, which is right once constants are coerced.
    // ponytail: ordered compares (< <=) on real stay bitwise — wrong for
    // negative floats; add FLt/FLe when something needs them.
    fn make_binary(&self, op: ast::BinOp, lhs: Expr, rhs: Expr) -> Expr {
        if self.is_real_expr(&lhs) || self.is_real_expr(&rhs) {
            let (lhs, rhs) = (self.coerce_real(lhs), self.coerce_real(rhs));
            let op = match op {
                ast::BinOp::Add => BinOp::FAdd,
                ast::BinOp::Sub => BinOp::FSub,
                ast::BinOp::Mul => BinOp::FMul,
                ast::BinOp::Div => BinOp::FDiv,
                other => match lower_binop(other) {
                    Some(op) => op,
                    None => return Expr::Unknown,
                },
            };
            return Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        match lower_binop(op) {
            Some(op) => Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) },
            None => Expr::Unknown,
        }
    }

    /// Derive a comparison from the three-way `<=>` impl (spaceship, spec
    /// 3.25): `a < b` becomes `(a <=> b) == Ordering::Less`, etc. The impl
    /// returns std::ops' `Ordering { Less, Equal, Greater }` (0/1/2), so no
    /// signed arithmetic is needed. `None` when the operand type has no
    /// `<=>` impl — built-in comparison applies.
    fn inline_cmp(
        &self,
        op_str: &str,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Expr> {
        // (discriminant to compare against, negate?)
        let (want, ne) = match op_str {
            "<" => (0u64, false),  // == Less
            "==" => (1, false),    // == Equal
            ">" => (2, false),     // == Greater
            ">=" => (0, true),     // != Less
            "!=" => (1, true),     // != Equal
            "<=" => (2, true),     // != Greater
            _ => return None,
        };
        let Val::Scalar(cmp) = self.inline_op("<=>", lhs, rhs, env)? else { return None }; // -> Ord::cmp
        Some(Expr::Binary {
            op: if ne { BinOp::Ne } else { BinOp::Eq },
            lhs: Box::new(cmp),
            rhs: Box::new(Expr::Const(want)),
        })
    }

    /// Inline a unary operator impl (`not a`): binds only `self`.
    fn inline_unary(&self, op: &str, rhs: &ast::Expr) -> Option<Val> {
        let ty = self.operand_type_name(rhs)?;
        let tr = siox_syntax::ast::op_trait_name(op).unwrap_or(op);
        let fns = self.op_impls.get(&(tr.to_string(), ty))?;
        let (f, _) = fns.first()?;
        let body = f.body.as_ref()?;
        let mut env: HashMap<String, Val> = HashMap::new();
        env.insert("self".to_string(), self.lower_val_env(rhs, &HashMap::new()));
        env.insert(
            "self::width".to_string(),
            Val::Scalar(Expr::Const(self.ast_width(rhs) as u64)),
        );
        self.inline_block(&body.stmts, &env)
    }

    /// Synthesize a total derivation conversion `target(x)` when no explicit
    /// `From` impl exists (spec: derived types §14). Two total cases:
    ///  - enums connected by a derivation chain where every source variant
    ///    exists in the target — representation-identity (base-first
    ///    discriminants), so the value passes through unchanged;
    ///  - a source struct that derives (transitively) from the target struct
    ///    — project onto the inherited fields.
    fn derived_conversion(
        &self,
        target: &str,
        src: Option<&str>,
        arg: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let src = src?;
        // Enum case: chain-connected and source variants subset of target.
        if let (Some(sv), Some(tv)) =
            (self.enum_variants.get(src), self.enum_variants.get(target))
        {
            let connected =
                self.enum_ancestor(src, target) || self.enum_ancestor(target, src);
            let total = sv.keys().all(|v| tv.contains_key(v));
            if connected && total {
                return Some(self.lower_val_env(arg, env)); // identity
            }
            return None;
        }
        // Struct case: project a derived struct onto its base fields by
        // reading the source's per-field signals (a bare struct path isn't
        // itself a Val::Fields).
        if self.struct_derives_from(src, target) {
            let base = expr_path(arg)?;
            let fields = self
                .struct_field_names(target)
                .into_iter()
                .map(|n| {
                    let expr = self
                        .locals
                        .get(&format!("{base}.{n}"))
                        .map(|&id| Expr::Current(id))
                        .unwrap_or(Expr::Unknown);
                    (n, expr)
                })
                .collect();
            return Some(Val::Fields(fields));
        }
        None
    }

    /// Whether `anc` is a (transitive) enum-derivation ancestor of `name`.
    fn enum_ancestor(&self, anc: &str, name: &str) -> bool {
        let mut cur = name.to_string();
        let mut guard = 0;
        while let Some(b) = self.enum_bases.get(&cur) {
            if b == anc {
                return true;
            }
            cur = b.clone();
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        false
    }

    /// Whether struct `name` derives (transitively) from struct `base`.
    fn struct_derives_from(&self, name: &str, base: &str) -> bool {
        let mut cur = name.to_string();
        let mut guard = 0;
        while let Some(s) = self.structs.get(&cur) {
            let Some(b) = s.base.as_ref().and_then(type_head_name) else { return false };
            if b == base {
                return true;
            }
            cur = b.to_string();
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        false
    }

    /// A struct type's full (inherited + own) field names, base chain first.
    fn struct_field_names(&self, name: &str) -> Vec<String> {
        let Some(s) = self.structs.get(name) else { return Vec::new() };
        let mut out = match s.base.as_ref().and_then(type_head_name) {
            Some(b) => self.struct_field_names(b),
            None => Vec::new(),
        };
        out.extend(s.fields.iter().map(|f| f.name.text.clone()));
        out
    }

    /// `T(x)` on a named type: dispatch to `impl From<Source> for T`,
    /// selected by the argument's type (sole impl accepted for an unknown
    /// source). Struct-valued results come back as per-field values.
    fn lower_from(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let target = match callee {
            ast::Expr::Path(p) if p.segments.len() == 1 => p.segments[0].text.as_str(),
            _ => return None,
        };
        let arg = args.first()?;
        let src = self.operand_type_name(arg);
        // No explicit `impl From<src> for target`: try a derivation-total
        // conversion (spec: T(x) is auto for total derivations).
        let Some(fns) = self.op_impls.get(&("From".to_string(), target.to_string())) else {
            return self.derived_conversion(target, src.as_deref(), arg, env);
        };
        let declared = |f: &ast::FnDecl, a: &Option<String>| -> Option<String> {
            a.clone().or_else(|| {
                f.params
                    .iter()
                    .find(|p| !p.is_self)
                    .and_then(|p| p.ty.as_ref())
                    .and_then(type_head_name)
                    .map(str::to_string)
            })
        };
        let chosen = match &src {
            Some(sty) => fns.iter().find(|(f, a)| declared(f, a).as_deref() == Some(sty)),
            None => (fns.len() == 1).then(|| &fns[0]),
        };
        let (f, _) = match chosen {
            Some(c) => c,
            None => return self.derived_conversion(target, src.as_deref(), arg, env),
        };
        let body = f.body.as_ref()?;
        let mut fenv: HashMap<String, Val> = HashMap::new();
        if let Some(p) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &p.name {
                fenv.insert(n.text.clone(), self.lower_val_env(arg, env));
                fenv.insert(
                    format!("{}::width", n.text),
                    Val::Scalar(Expr::Const(self.ast_width(arg) as u64)),
                );
            }
        }
        self.inline_block(&body.stmts, &fenv)
    }

    /// Lower a call to a module-level `fn`: const-fold when every argument
    /// const-evaluates (so `clog2(DEPTH)` is a constant), else inline the
    /// body like an operator impl (params bound positionally, with
    /// `param::width` available). Depth-guarded against runaway recursion.
    fn lower_free_call(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Expr> {
        let name = match callee {
            ast::Expr::Path(p) if p.segments.len() == 1 => p.segments[0].text.as_str(),
            _ => return None,
        };
        let f = *self.free_fns.get(name)?;
        // A bodyless declaration is a foreign C function (`extern "C"`).
        if f.body.is_none() {
            let is_real = |t: &Option<ast::Type>| {
                matches!(t, Some(ast::Type::Path(p))
                    if p.segments.last().map(|s| s.text.as_str()) == Some("real"))
            };
            let f64_args = f
                .params
                .iter()
                .filter(|p| !p.is_self)
                .map(|p| is_real(&p.ty))
                .collect();
            let f64_ret = is_real(&f.ret);
            let args = args.iter().map(|a| self.lower_scalar_env(a, env)).collect();
            return Some(Expr::CCall { name: name.to_string(), args, f64_args, f64_ret });
        }
        // Constant arguments: run the body statically.
        let consts: Option<Vec<i64>> = args
            .iter()
            .map(|a| eval_const_fns(a, &self.cur_env, &self.free_fns, 0))
            .collect();
        if let Some(cs) = consts {
            let mut fenv = self.cur_env.clone();
            for (p, v) in f.params.iter().filter(|p| !p.is_self).zip(cs) {
                if let Some(n) = &p.name {
                    fenv.insert(n.text.clone(), v);
                }
            }
            if let Some(v) =
                eval_const_stmts(&f.body.as_ref()?.stmts, &fenv, &self.free_fns, 0)
            {
                return Some(Expr::Const(v as u64));
            }
        }
        // Dynamic arguments: inline the body as an expression tree.
        if self.inline_depth.get() > 16 {
            return None;
        }
        self.inline_depth.set(self.inline_depth.get() + 1);
        let mut fenv: HashMap<String, Val> = HashMap::new();
        // Saved param-family bindings to restore after this inline (nesting).
        let mut saved: Vec<(String, Option<String>)> = Vec::new();
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                fenv.insert(n.text.clone(), self.lower_val_env(a, env));
                fenv.insert(
                    format!("{}::width", n.text),
                    Val::Scalar(Expr::Const(self.ast_width(a) as u64)),
                );
                // Propagate the argument's family so the body dispatches
                // operators on the caller's concrete type.
                if let Some(fam) = self.operand_type_name(a) {
                    let prev = self.param_types.borrow_mut().insert(n.text.clone(), fam);
                    saved.push((n.text.clone(), prev));
                }
            }
        }
        let out = match f.body.as_ref().and_then(|b| self.inline_block(&b.stmts, &fenv)) {
            Some(Val::Scalar(v)) => Some(v),
            _ => None,
        };
        for (name, prev) in saved.into_iter().rev() {
            match prev {
                Some(v) => self.param_types.borrow_mut().insert(name, v),
                None => self.param_types.borrow_mut().remove(&name),
            };
        }
        self.inline_depth.set(self.inline_depth.get() - 1);
        out
    }

    /// Find a method `name` on type `ty`: an inherent-impl method
    /// (`impl T { fn name(self, ..) }`) or a trait-impl method
    /// (`impl Tr for T { fn name(self, ..) }`, held in `op_impls` keyed by
    /// trait+type). Inherent impls win; first match otherwise.
    fn find_method(&self, ty: &str, name: &str) -> Option<&'a ast::FnDecl> {
        if let Some(impls) = self.impls.get(ty) {
            for im in impls {
                for it in &im.items {
                    if let ast::ImplItem::Fn(f) = it {
                        if f.name.text == name {
                            return Some(f);
                        }
                    }
                }
            }
        }
        self.op_impls
            .iter()
            .filter(|((_, t), _)| t == ty)
            .flat_map(|(_, fns)| fns.iter())
            .find(|(f, _)| f.name.text == name)
            .map(|(f, _)| *f)
    }

    /// Lower a method call `recv.method(args)` (spec 3.20) by inlining the
    /// impl method's body: `self` binds to the receiver, each named parameter
    /// to its argument (mirroring [`Self::lower_free_call`]), and the receiver
    /// type is stashed under `param_types["self"]` so operators inside the body
    /// dispatch on the concrete type. Value-returning methods (`a.cmp(b)`,
    /// `s.can_send()`) inline to a [`Val`]; a body the inliner cannot express
    /// as a value (a statement method that drives signals) yields `None`.
    fn lower_method_call(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let ast::Expr::Field { base, field, .. } = callee else { return None };
        let ty = self.operand_type_name(base)?;
        let f = self.find_method(&ty, &field.text)?;
        let body = f.body.as_ref()?;
        if self.inline_depth.get() > 16 {
            return None;
        }
        self.inline_depth.set(self.inline_depth.get() + 1);
        let mut fenv: HashMap<String, Val> = HashMap::new();
        fenv.insert("self".to_string(), self.lower_val_env(base, env));
        fenv.insert(
            "self::width".to_string(),
            Val::Scalar(Expr::Const(self.ast_width(base) as u64)),
        );
        // Family bindings to restore after the inline (nesting-safe).
        let mut saved: Vec<(String, Option<String>)> = Vec::new();
        let self_prev = self.param_types.borrow_mut().insert("self".to_string(), ty.clone());
        saved.push(("self".to_string(), self_prev));
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                fenv.insert(n.text.clone(), self.lower_val_env(a, env));
                fenv.insert(
                    format!("{}::width", n.text),
                    Val::Scalar(Expr::Const(self.ast_width(a) as u64)),
                );
                if let Some(fam) = self.operand_type_name(a) {
                    let prev = self.param_types.borrow_mut().insert(n.text.clone(), fam);
                    saved.push((n.text.clone(), prev));
                }
            }
        }
        let out = self.inline_block(&body.stmts, &fenv);
        for (name, prev) in saved.into_iter().rev() {
            match prev {
                Some(v) => self.param_types.borrow_mut().insert(name, v),
                None => self.param_types.borrow_mut().remove(&name),
            };
        }
        self.inline_depth.set(self.inline_depth.get() - 1);
        out
    }

    /// Lower a method call used as a *statement* (`s.send(v)`): inline the
    /// method's body as drivers, substituting `self` -> receiver and each
    /// parameter -> its argument, so a body of `self.valid = '1'; self.data =
    /// value;` drives the receiver's flattened field signals. Returns `false`
    /// when the receiver's type or the method can't be resolved (the caller
    /// then leaves the statement to the existing fall-through).
    fn lower_method_stmt(
        &mut self,
        recv: &ast::Expr,
        method: &str,
        args: &[ast::Expr],
        cond: Option<Expr>,
    ) -> bool {
        let Some(ty) = self.operand_type_name(recv) else { return false };
        // `f` borrows the AST (`'a`), not `self`, so it survives the `&mut self`
        // lowering calls below.
        let Some(f) = self.find_method(&ty, method) else { return false };
        let Some(body) = f.body.as_ref() else { return false };
        let mut map: HashMap<String, ast::Expr> = HashMap::new();
        map.insert("self".to_string(), recv.clone());
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                map.insert(n.text.clone(), a.clone());
            }
        }
        let stmts: Vec<ast::Stmt> =
            body.stmts.iter().map(|s| subst_stmt_paths(s, &map)).collect();
        for s in &stmts {
            self.lower_stmt(s, cond.clone());
        }
        true
    }

    /// Lower a conversion expression (spec 3.17): `uint[16](x)` resizes,
    /// `int[8](x)` truncates, `integer(x)` crosses to the kernel word, and
    /// `resize(x, n)` is the family-preserving spelling (n const-evaluable —
    /// the language is static, so a value argument in width position is a
    /// generic argument). Semantics on the word IR: an `int`-family source
    /// sign-extends into the full word first (`v - 2^w` when the sign bit is
    /// set); the target width truncates via a slice; widening to `uint`
    /// zero-extends implicitly. `None` when `callee` is not a conversion.
    fn lower_conversion(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Expr> {
        // Target: (is_resize, family, width). Width None = kernel integer.
        let head = |e: &ast::Expr| match e {
            ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
            _ => None,
        };
        let (target_w, resize) = match callee {
            ast::Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == "integer" => {
                (None, false)
            }
            // `Char(n)`: a code point becomes a symbol (32-bit storage).
            ast::Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == "Char" => {
                (Some(32), false)
            }
            ast::Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == "resize" => {
                let n = args.get(1)?;
                let w = match self.lower_scalar_env(n, env) {
                    Expr::Const(c) => c as u32,
                    _ => eval_const(n, &self.cur_env)? as u32,
                };
                (Some(w), true)
            }
            ast::Expr::Index { base, index, .. }
                if head(base).as_deref().is_some_and(|h| self.vector_families.contains(h)) =>
            {
                let w = match self.lower_scalar_env(index, env) {
                    Expr::Const(c) => c as u32,
                    _ => eval_const(index, &self.cur_env)? as u32,
                };
                (Some(w), false)
            }
            _ => return None,
        };
        let _ = resize;
        let arg = args.first()?;
        // Conversions are a raw resize (zero-extend / truncate). Signed
        // widening is the library `std::bits::sext`, not the compiler's job.
        let v = self.lower_scalar_env(arg, env);
        Some(match target_w {
            Some(w) if w > 0 && w < 64 => Expr::Slice { base: Box::new(v), hi: w - 1, lo: 0 },
            _ => v,
        })
    }

    /// The type name an operand contributes to operator-impl lookup: a local's
    /// declared enum/struct, a suffix literal's target type, an enum variant's
    /// enum, or `integer` for a bare numeric literal.
    fn operand_type_name(&self, e: &ast::Expr) -> Option<String> {
        match e {
            ast::Expr::Int { .. } => Some("integer".to_string()),
            ast::Expr::SuffixLit { suffix, .. } => {
                self.suffix_impls.get(&suffix.text).map(|(ty, _)| ty.clone())
            }
            // A conversion expression `F[N](x)` / `F(x)` reads as its target
            // family, so operators on it dispatch correctly (`int[32](a) < ..`
            // uses int's signed Ord).
            ast::Expr::Call { callee, .. } => {
                let head = match callee.as_ref() {
                    ast::Expr::Index { base, .. } => expr_path(base),
                    ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
                    _ => None,
                }?;
                // A conversion reads as its target: a vector family
                // (`int[32](a)`) or an enum (`ULogic(b)` inside
                // `Logic(ULogic(b))`).
                (self.vector_families.contains(&head) || self.enum_variants.contains_key(&head))
                    .then_some(head)
            }
            ast::Expr::Path(p) if p.segments.len() >= 2 => self
                .enum_variants
                .contains_key(&p.segments[0].text)
                .then(|| p.segments[0].text.clone()),
            _ => {
                let p = expr_path(e)?;
                // A generic-fn parameter reads as its caller's concrete family.
                if let Some(fam) = self.param_types.borrow().get(&p) {
                    return Some(fam.clone());
                }
                if self.local_char.contains(&p) {
                    return Some("Char".to_string());
                }
                self.local_enum
                    .get(&p)
                    .or_else(|| self.local_struct.get(&p))
                    .or_else(|| self.local_numeric.get(&p))
                    .cloned()
            }
        }
    }

    /// The value a straight-line `return`/`if-else` block produces, or `None`
    /// if the block has statements the inliner cannot express as a value.
    fn inline_block(&self, stmts: &[ast::Stmt], env: &HashMap<String, Val>) -> Option<Val> {
        match stmts {
            [ast::Stmt::Return { value: Some(v), .. }, ..] => Some(self.lower_val_env(v, env)),
            [ast::Stmt::If(iff), rest @ ..] => {
                let cond = self.lower_scalar_env(&iff.cond, env);
                let then = self.inline_block(&iff.then.stmts, env)?;
                // The else value: an explicit else branch, or the statements
                // after the if.
                let els = match &iff.else_ {
                    Some(e) => match e.as_ref() {
                        ast::ElseBranch::Block(b) => self.inline_block(&b.stmts, env)?,
                        ast::ElseBranch::If(i) => {
                            self.inline_block(std::slice::from_ref(&ast::Stmt::If(i.clone())), env)?
                        }
                    },
                    None => self.inline_block(rest, env)?,
                };
                Some(select_val(cond, then, els))
            }
            _ => None,
        }
    }

    /// Lower an expression to a [`Val`], with fn parameters substituted from
    /// `env`. Struct-typed locals and struct literals become per-field values.
    fn lower_val_env(&self, e: &ast::Expr, env: &HashMap<String, Val>) -> Val {
        match e {
            // `self::width` inside an operator-impl body: the bound operand's
            // width (inline_op stashes it under the "param::attr" key).
            ast::Expr::SysAttr { base, attr, .. } => {
                if let Some(v) = expr_path(base)
                    .and_then(|p| env.get(&format!("{p}::{}", attr.text)))
                {
                    return v.clone();
                }
                Val::Scalar(self.lower_expr(e))
            }
            ast::Expr::IfExpr { cond, then, els, .. } => {
                let c = self.lower_scalar_env(cond, env);
                select_val(c, self.lower_val_env(then, env), self.lower_val_env(els, env))
            }
            ast::Expr::Call { callee, args, .. } => {
                match self
                    .lower_conversion(callee, args, env)
                    .or_else(|| self.lower_free_call(callee, args, env))
                {
                    Some(v) => Val::Scalar(v),
                    None => match self
                        .lower_method_call(callee, args, env)
                        .or_else(|| self.lower_from(callee, args, env))
                    {
                        Some(v) => v,
                        None => Val::Scalar(self.lower_expr(e)),
                    },
                }
            }
            ast::Expr::Path(p) if p.segments.len() == 1 => {
                let name = &p.segments[0].text;
                if let Some(v) = env.get(name) {
                    return v.clone();
                }
                if let Some(v) = self.struct_local_val(name) {
                    return v;
                }
                Val::Scalar(self.lower_expr(e))
            }
            // `self.re` where `self` is an env-bound struct value.
            ast::Expr::Field { base, field, .. } => {
                if let ast::Expr::Path(p) = base.as_ref() {
                    if p.segments.len() == 1 {
                        if let Some(Val::Fields(fs)) = env.get(&p.segments[0].text) {
                            let v = fs
                                .iter()
                                .find(|(n, _)| *n == field.text)
                                .map(|(_, e)| e.clone())
                                .unwrap_or(Expr::Unknown);
                            return Val::Scalar(v);
                        }
                    }
                }
                Val::Scalar(self.lower_expr(e))
            }
            // A struct literal (named or name-less): one value per field.
            // `.re` shorthand means `.re = re`; a positional arg binds to the
            // struct's field at that position (needs a named struct type).
            ast::Expr::Construct { ty, args, .. } => {
                let field_order: Option<Vec<String>> = ty
                    .as_ref()
                    .and_then(type_head_name)
                    .and_then(|n| self.raw_struct_fields(n))
                    .map(|fs| fs.into_iter().map(|(n, _)| n).collect());
                Val::Fields(
                    args.iter()
                        .enumerate()
                        .map(|(i, a)| {
                            let fname = match &a.field {
                                Some(f) => f.text.clone(),
                                None => field_order
                                    .as_ref()
                                    .and_then(|o| o.get(i).cloned())
                                    .unwrap_or_default(),
                            };
                            let v = match &a.value {
                                Some(v) => self.lower_scalar_env(v, env),
                                None => match env.get(&fname) {
                                    Some(Val::Scalar(e)) => e.clone(),
                                    _ => self
                                        .locals
                                        .get(&fname)
                                        .map(|&id| Expr::Current(id))
                                        .unwrap_or(Expr::Unknown),
                                },
                            };
                            (fname, v)
                        })
                        .collect(),
                )
            }
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                let op_str = siox_syntax::pretty::bin_op(op);
                if !matches!(op_str, "==" | "!=") {
                    if let Some(v) = self.inline_op(op_str, lhs, rhs, env) {
                        return v;
                    }
                }
                if let Some(derived) = self.inline_cmp(op_str, lhs, rhs, env) {
                    return Val::Scalar(derived);
                }
                let (l, r) = (self.lower_scalar_env(lhs, env), self.lower_scalar_env(rhs, env));
                Val::Scalar(self.make_binary(op.clone(), l, r))
            }
            ast::Expr::Unary { op, rhs, .. } => Val::Scalar(Expr::Unary {
                op: lower_unop(*op),
                rhs: Box::new(self.lower_scalar_env(rhs, env)),
            }),
            ast::Expr::SuffixLit { .. } => self.inline_suffix(e).unwrap_or_else(|| {
                Val::Scalar(self.lower_expr(e)) // fixed fs/Hz table fallback
            }),
            _ => Val::Scalar(self.lower_expr(e)),
        }
    }

    /// Inline the `impl Suffix for T` fn for a suffixed literal (`5i` ->
    /// `Complex::i(5)`): the fn's parameter binds to the literal value.
    fn inline_suffix(&self, e: &ast::Expr) -> Option<Val> {
        let ast::Expr::SuffixLit { text, suffix, .. } = e else { return None };
        let (_, f) = self.suffix_impls.get(&suffix.text)?;
        let body = f.body.as_ref()?;
        let mut env: HashMap<String, Val> = HashMap::new();
        if let Some(p) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &p.name {
                // A `real` parameter takes the literal's float value.
                let is_real = p.ty.as_ref().and_then(type_head_name) == Some("real");
                let v = if is_real {
                    Expr::Real(text.parse().unwrap_or(0.0))
                } else {
                    Expr::Const(parse_int(text).unwrap_or(0))
                };
                env.insert(n.text.clone(), Val::Scalar(v));
            }
        }
        self.inline_block(&body.stmts, &env)
    }

    fn lower_scalar_env(&self, e: &ast::Expr, env: &HashMap<String, Val>) -> Expr {
        match self.lower_val_env(e, env) {
            Val::Scalar(e) => e,
            Val::Fields(_) => Expr::Unknown, // a struct value has no scalar context
        }
    }

    /// The per-field value of a struct-typed local (`p` -> `p.re`, `p.im`).
    fn struct_local_val(&self, name: &str) -> Option<Val> {
        let sname = self.local_struct.get(name)?;
        let s = self.structs.get(sname)?;
        Some(Val::Fields(
            s.fields
                .iter()
                .map(|f| {
                    let sig = self.locals.get(&format!("{name}.{}", f.name.text));
                    (
                        f.name.text.clone(),
                        sig.map(|&id| Expr::Current(id)).unwrap_or(Expr::Unknown),
                    )
                })
                .collect(),
        ))
    }

    /// Lower a system attribute. `clk::rising`/`falling`/`edge` expand into
    /// `Event`/`Old`/`Current` so the scheduler needs no special knowledge.
    fn lower_sysattr(&self, base: &ast::Expr, attr: &str) -> Expr {
        // `xs::len` is elaboration-time metadata: the array's element count.
        if attr == "len" {
            if let Some(indices) =
                expr_path(base).and_then(|p| self.local_array.get(&p))
            {
                return Expr::Const(indices.len() as u64);
            }
            return Expr::Unknown;
        }
        // `x::width` is elaboration-time metadata too: the signal's bit width.
        if attr == "width" {
            if let Some(sig) = self.base_signal(base) {
                return Expr::Const(self.out.signals[sig.0 as usize].width as u64);
            }
            return Expr::Unknown;
        }
        let Some(sig) = self.base_signal(base) else { return Expr::Unknown };
        match attr {
            "event" | "edge" => Expr::Event(sig),
            "old" => Expr::Old(sig),
            // rising: Event(clk) && Old(clk) == '0' && Current(clk) == '1'
            "rising" => and3(
                Expr::Event(sig),
                eq(Expr::Old(sig), Expr::Logic('0')),
                eq(Expr::Current(sig), Expr::Logic('1')),
            ),
            // falling: Event(clk) && Old(clk) == '1' && Current(clk) == '0'
            "falling" => and3(
                Expr::Event(sig),
                eq(Expr::Old(sig), Expr::Logic('1')),
                eq(Expr::Current(sig), Expr::Logic('0')),
            ),
            _ => Expr::Unknown,
        }
    }

    fn base_signal(&self, base: &ast::Expr) -> Option<SignalId> {
        if let ast::Expr::Path(p) = base {
            if p.segments.len() == 1 {
                return self.locals.get(&p.segments[0].text).copied();
            }
        }
        None
    }
}

/// Collect every signal an IR expression reads (`Current`/`Old`/`Event`
/// leaves) into `out`, in first-seen order.
pub fn read_set(e: &Expr, out: &mut Vec<SignalId>) {
    match e {
        Expr::Current(id) | Expr::Old(id) | Expr::Event(id) => out.push(*id),
        Expr::CCall { args, .. } => {
            for a in args {
                read_set(a, out);
            }
        }
        Expr::Unary { rhs, .. } => read_set(rhs, out),
        Expr::Binary { lhs, rhs, .. } => {
            read_set(lhs, out);
            read_set(rhs, out);
        }
        Expr::Slice { base, .. } => read_set(base, out),
        Expr::Select { cond, then, els } => {
            read_set(cond, out);
            read_set(then, out);
            read_set(els, out);
        }
        Expr::Const(_) | Expr::Real(_) | Expr::Logic(_) | Expr::Unknown => {}
    }
}

fn dedup(v: &mut Vec<SignalId>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|id| seen.insert(*id));
}

/// Validation walk over an expression (see [`Design::validate`]).
fn check_expr(e: &Expr, n: u32, issues: &mut Vec<String>, ctx: &str) {
    match e {
        Expr::CCall { args, .. } => {
            for a in args {
                check_expr(a, n, issues, ctx);
            }
        }
        Expr::Current(id) | Expr::Old(id) | Expr::Event(id) => {
            if id.0 >= n {
                issues.push(format!("{ctx}: signal id {} out of range (n={n})", id.0));
            }
        }
        Expr::Unknown => issues.push(format!("{ctx}: contains an Unknown (unlowered) expression")),
        Expr::Unary { rhs, .. } => check_expr(rhs, n, issues, ctx),
        Expr::Binary { lhs, rhs, .. } => {
            check_expr(lhs, n, issues, ctx);
            check_expr(rhs, n, issues, ctx);
        }
        Expr::Slice { base, hi, lo } => {
            if lo > hi {
                issues.push(format!("{ctx}: slice bounds lo {lo} > hi {hi}"));
            }
            check_expr(base, n, issues, ctx);
        }
        Expr::Select { cond, then, els } => {
            check_expr(cond, n, issues, ctx);
            check_expr(then, n, issues, ctx);
            check_expr(els, n, issues, ctx);
        }
        Expr::Const(_) | Expr::Real(_) | Expr::Logic(_) => {}
    }
}

/// A unit of behaviour the scheduler dispatches, with its **sensitivity**
/// (the signals it reads) and **write set** (the signals it drives). This is
/// the process view the LLVM backend compiles and the interpreter dispatches
/// on (spec Stage 6 / the compiled-backend plan, B1).
#[derive(Clone, Debug)]
pub struct Process {
    pub kind: ProcessKind,
    /// Signals read by the process's conditions/expressions (sensitivity).
    pub reads: Vec<SignalId>,
    /// Signals the process drives.
    pub writes: Vec<SignalId>,
}

#[derive(Clone, Debug)]
pub enum ProcessKind {
    /// A combinational target, resolved from the drivers that target it, in
    /// source order (spec 3.14 last-writer-wins). `drivers` indexes
    /// `Design::drivers`.
    Comb { target: SignalId, drivers: Vec<usize> },
    /// A clocked event block. `block` indexes `Design::event_blocks`.
    Event { block: usize },
}

impl Design {
    /// Check the IR is well-formed enough for a backend to compile: signal
    /// ids in range, no `Unknown` (unlowered) expressions, concrete widths,
    /// and valid slice bounds. Returns a list of problems — empty means the
    /// design is safe to hand to codegen. Pure; callers decide how to react.
    pub fn validate(&self) -> Vec<String> {
        let n = self.signals.len() as u32;
        let mut issues = Vec::new();

        // Signals codegen actually touches (driven or read). An unreferenced
        // width-0 signal — e.g. an instance-binding `let` placeholder — is
        // harmless, so only flag unknown widths on referenced signals.
        let mut referenced: std::collections::HashSet<SignalId> = std::collections::HashSet::new();
        let collect = |e: &Expr| {
            let mut v = Vec::new();
            read_set(e, &mut v);
            v
        };
        for d in &self.drivers {
            referenced.insert(d.target);
            if let Some(c) = &d.cond {
                referenced.extend(collect(c));
            }
            referenced.extend(collect(&d.expr));
        }
        for eb in &self.event_blocks {
            referenced.extend(collect(&eb.condition));
            for u in &eb.updates {
                referenced.insert(u.target);
                if let Some(c) = &u.cond {
                    referenced.extend(collect(c));
                }
                referenced.extend(collect(&u.expr));
            }
        }
        for (i, s) in self.signals.iter().enumerate() {
            if s.width == 0 && referenced.contains(&SignalId(i as u32)) {
                issues.push(format!("signal `{}` has unknown width (0)", s.path));
            }
        }
        let target = |id: SignalId, what: &str, issues: &mut Vec<String>| {
            if id.0 >= n {
                issues.push(format!("{what}: target signal id {} out of range (n={n})", id.0));
            }
        };
        for (di, d) in self.drivers.iter().enumerate() {
            let ctx = format!("driver {di}");
            target(d.target, &ctx, &mut issues);
            if let Some(c) = &d.cond {
                check_expr(c, n, &mut issues, &format!("{ctx} cond"));
            }
            check_expr(&d.expr, n, &mut issues, &format!("{ctx} expr"));
        }
        for (bi, eb) in self.event_blocks.iter().enumerate() {
            check_expr(&eb.condition, n, &mut issues, &format!("event {bi} cond"));
            for (ui, u) in eb.updates.iter().enumerate() {
                let ctx = format!("event {bi} update {ui}");
                target(u.target, &ctx, &mut issues);
                if let Some(c) = &u.cond {
                    check_expr(c, n, &mut issues, &format!("{ctx} cond"));
                }
                check_expr(&u.expr, n, &mut issues, &format!("{ctx} expr"));
            }
        }
        issues
    }

    /// The process decomposition: one combinational process per driven signal
    /// (grouping its source-ordered drivers) and one per event block, each
    /// with its sensitivity and write set. Combinational targets keep their
    /// first-seen order so source-order override is preserved.
    pub fn processes(&self) -> Vec<Process> {
        let mut procs = Vec::new();

        // Group combinational drivers by target, first-seen order.
        let mut order: Vec<SignalId> = Vec::new();
        let mut by_target: std::collections::HashMap<SignalId, Vec<usize>> =
            std::collections::HashMap::new();
        for (i, d) in self.drivers.iter().enumerate() {
            by_target.entry(d.target).or_insert_with(|| {
                order.push(d.target);
                Vec::new()
            });
            by_target.get_mut(&d.target).unwrap().push(i);
        }
        for target in order {
            let drivers = by_target.remove(&target).unwrap();
            let mut reads = Vec::new();
            for &di in &drivers {
                let d = &self.drivers[di];
                if let Some(c) = &d.cond {
                    read_set(c, &mut reads);
                }
                read_set(&d.expr, &mut reads);
            }
            dedup(&mut reads);
            procs.push(Process { kind: ProcessKind::Comb { target, drivers }, reads, writes: vec![target] });
        }

        // One process per event block.
        for (bi, eb) in self.event_blocks.iter().enumerate() {
            let mut reads = Vec::new();
            read_set(&eb.condition, &mut reads);
            let mut writes = Vec::new();
            for u in &eb.updates {
                if let Some(c) = &u.cond {
                    read_set(c, &mut reads);
                }
                read_set(&u.expr, &mut reads);
                writes.push(u.target);
            }
            dedup(&mut reads);
            dedup(&mut writes);
            procs.push(Process { kind: ProcessKind::Event { block: bi }, reads, writes });
        }
        procs
    }

    /// Render normalized IR (backs `siox ir`).
    pub fn to_ir_string(&self) -> String {
        let mut out = String::new();
        for s in &self.signals {
            let w = if s.width == 0 { "?".to_string() } else { s.width.to_string() };
            out.push_str(&format!("signal {} : {w}\n", s.path));
        }
        for d in &self.drivers {
            let cond = match &d.cond {
                Some(c) => format!("  when {}", render(c, self)),
                None => String::new(),
            };
            out.push_str(&format!(
                "driver {} = {}{cond}\n",
                self.signals[d.target.0 as usize].path,
                render(&d.expr, self)
            ));
        }
        for eb in &self.event_blocks {
            out.push_str(&format!("event ({}):\n", render(&eb.condition, self)));
            for u in &eb.updates {
                let cond = match &u.cond {
                    Some(c) => format!("  when {}", render(c, self)),
                    None => String::new(),
                };
                out.push_str(&format!(
                    "    next {} = {}{cond}\n",
                    self.signals[u.target.0 as usize].path,
                    render(&u.expr, self)
                ));
            }
        }
        out
    }
}

// --- expression builders ----------------------------------------------------

/// Decode a bit-pattern literal (`b"01??"` / `x"A?"`, spec 3.22) into a
/// `(mask, value)` pair: an input matches when `input & mask == value`. `?`
/// digits are don't-cares (mask 0); a hex `?` masks its whole nibble. `_`
/// separators are ignored. `None` when the text isn't a well-formed pattern
/// (an invalid digit, or wider than 64 bits).
pub fn bit_pattern_mask(text: &str) -> Option<(u64, u64)> {
    let (base, digits) = match text.split_once('"') {
        Some((b, rest)) => (b, rest.trim_end_matches('"')),
        None => return None,
    };
    let per: u32 = match base {
        "b" => 1,
        "x" => 4,
        _ => return None,
    };
    let mut mask = 0u64;
    let mut value = 0u64;
    let mut bits = 0u32;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        bits += per;
        if bits > 64 {
            return None;
        }
        let (m, v) = match c {
            '?' => (0, 0),
            _ => {
                let d = c.to_digit(if per == 1 { 2 } else { 16 })? as u64;
                (((1u64 << per) - 1), d)
            }
        };
        mask = (mask << per) | m;
        value = (value << per) | v;
    }
    Some((mask, value))
}

fn not(e: Expr) -> Expr {
    Expr::Unary { op: UnOp::Not, rhs: Box::new(e) }
}

fn eq(lhs: Expr, rhs: Expr) -> Expr {
    Expr::Binary { op: BinOp::Eq, lhs: Box::new(lhs), rhs: Box::new(rhs) }
}

fn and3(a: Expr, b: Expr, c: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::And,
        lhs: Box::new(Expr::Binary { op: BinOp::And, lhs: Box::new(a), rhs: Box::new(b) }),
        rhs: Box::new(c),
    }
}

/// `and` of an optional accumulated condition with a new one.
fn and(acc: Option<Expr>, c: Expr) -> Expr {
    match acc {
        Some(a) => Expr::Binary { op: BinOp::And, lhs: Box::new(a), rhs: Box::new(c) },
        None => c,
    }
}

// --- rendering --------------------------------------------------------------

fn render(e: &Expr, d: &Design) -> String {
    match e {
        Expr::CCall { name, args, .. } => {
            let a = args.iter().map(|x| render(x, d)).collect::<Vec<_>>().join(", ");
            format!("{name}({a})")
        }
        Expr::Const(v) => v.to_string(),
        Expr::Real(x) => format!("{x}"),
        Expr::Logic(c) => format!("'{c}'"),
        Expr::Current(id) => d.signals[id.0 as usize].path.clone(),
        Expr::Old(id) => format!("Old({})", d.signals[id.0 as usize].path),
        Expr::Event(id) => format!("Event({})", d.signals[id.0 as usize].path),
        Expr::Unary { op, rhs } => format!("{}{}", un_sym(*op), paren(rhs, d)),
        Expr::Binary { op, lhs, rhs } => {
            format!("{} {} {}", paren(lhs, d), bin_sym(*op), paren(rhs, d))
        }
        Expr::Slice { base, hi, lo } => format!("{}[{hi}..{lo}]", paren(base, d)),
        Expr::Select { cond, then, els } => {
            format!("{} ? {} : {}", paren(cond, d), paren(then, d), paren(els, d))
        }
        Expr::Unknown => "?".to_string(),
    }
}

fn paren(e: &Expr, d: &Design) -> String {
    match e {
        Expr::Binary { .. } | Expr::Unary { .. } => format!("({})", render(e, d)),
        _ => render(e, d),
    }
}

fn un_sym(op: UnOp) -> &'static str {
    match op {
        UnOp::Not => "not ",
        UnOp::Neg => "-",
    }
}

fn bin_sym(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::And => "and",
        BinOp::Or => "or",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::FAdd => "+.",
        BinOp::FSub => "-.",
        BinOp::FMul => "*.",
        BinOp::FDiv => "/.",
    }
}

// --- helpers ----------------------------------------------------------------

/// Whether an expression depends on a `::event`-family system attribute, which
/// makes an enclosing `if` an event-controlled block (spec 3.11).
fn expr_is_event(e: &ast::Expr) -> bool {
    match e {
        ast::Expr::SysAttr { base, attr, .. } => {
            matches!(attr.text.as_str(), "event" | "rising" | "falling" | "edge")
                || expr_is_event(base)
        }
        ast::Expr::Unary { rhs, .. } => expr_is_event(rhs),
        ast::Expr::Binary { lhs, rhs, .. } => expr_is_event(lhs) || expr_is_event(rhs),
        ast::Expr::Field { base, .. } | ast::Expr::Index { base, .. } => expr_is_event(base),
        _ => false,
    }
}

fn lower_unop(op: AstUnOp) -> UnOp {
    match op {
        AstUnOp::Not => UnOp::Not,
        AstUnOp::Neg => UnOp::Neg,
    }
}

fn lower_binop(op: AstBinOp) -> Option<BinOp> {
    Some(match op {
        AstBinOp::Add => BinOp::Add,
        AstBinOp::Sub => BinOp::Sub,
        AstBinOp::Mul => BinOp::Mul,
        AstBinOp::Div => BinOp::Div,
        AstBinOp::And => BinOp::And,
        AstBinOp::Or => BinOp::Or,
        AstBinOp::Custom { .. } => return None,
        AstBinOp::Shl => BinOp::Shl,
        AstBinOp::Shr => BinOp::Shr,
        AstBinOp::Eq => BinOp::Eq,
        AstBinOp::Ne => BinOp::Ne,
        AstBinOp::Lt => BinOp::Lt,
        AstBinOp::Le => BinOp::Le,
        AstBinOp::Gt => BinOp::Gt,
        AstBinOp::Ge => BinOp::Ge,
    })
}

fn parse_int(text: &str) -> Option<u64> {
    let t = text.trim();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        u64::from_str_radix(b, 2).ok()
    } else {
        t.parse().ok()
    }
}

/// Bit width from a type annotation, substituting parameters from `env` (so
/// `uint[W]` with `W=8` is width 8). `0` means parametric / not yet known.
fn type_width(
    t: &ast::Type,
    env: &HashMap<String, i64>,
    fns: &HashMap<String, &ast::FnDecl>,
    structs: &HashMap<String, &ast::StructDecl>,
) -> u32 {
    match t {
        ast::Type::Path(p) => match p.segments.last().map(|s| s.text.as_str()) {
            Some("Bit") | Some("Logic") | Some("Bool") => 1,
            Some("real") => 64, // f64 bits
            Some("Char") => 32, // symbol storage (implementation detail)
            // A derived type inherits its base array's size/range: `struct Byte
            // : Logic[8]` is 8 bits, `struct Word : uint[16]` is 16 (spec:
            // nominal derivation reuses the base representation).
            Some(name) => structs
                .get(name)
                .and_then(|s| s.base.as_ref())
                .map(|b| type_width(b, env, fns, structs))
                .unwrap_or(0),
            None => 0,
        },
        // For `uint[8]` the index is the width; for `Logic[31..0]` it is the
        // span; unconstrained `T[]` stays width 0 ("set at use").
        ast::Type::Indexed { index: None, .. } => 0,
        ast::Type::Indexed { index: Some(index), .. } => match index.as_ref() {
            ast::Expr::Range { lo, hi, .. } => {
                match (eval_const_fns(lo, env, fns, 0), eval_const_fns(hi, env, fns, 0)) {
                    (Some(a), Some(b)) => (a - b).unsigned_abs() as u32 + 1,
                    _ => 0,
                }
            }
            e => eval_const_fns(e, env, fns, 0).map(|v| v.max(0) as u32).unwrap_or(0),
        },
        ast::Type::Generic { base, .. } | ast::Type::Mode { inner: base, .. } => {
            type_width(base, env, fns, structs)
        }
    }
}

/// Const-evaluate a width expression against a parameter environment.
fn eval_const(e: &ast::Expr, env: &HashMap<String, i64>) -> Option<i64> {
    eval_const_fns(e, env, &HashMap::new(), 0)
}

/// [`eval_const`] with module functions in scope: a call whose arguments
/// const-evaluate runs the function body statically (recursion allowed to a
/// bounded depth) — `clog2(DEPTH)` works in width positions.
pub fn eval_const_fns(
    e: &ast::Expr,
    env: &HashMap<String, i64>,
    fns: &HashMap<String, &ast::FnDecl>,
    depth: u32,
) -> Option<i64> {
    if depth > 64 {
        return None;
    }
    match e {
        ast::Expr::Int { text, .. } => parse_int(text).map(|v| v as i64),
        ast::Expr::Bool { value, .. } => Some(*value as i64),
        ast::Expr::Path(p) if p.segments.len() == 1 => env.get(&p.segments[0].text).copied(),
        ast::Expr::IfExpr { cond, then, els, .. } => {
            if eval_const_fns(cond, env, fns, depth + 1)? != 0 {
                eval_const_fns(then, env, fns, depth + 1)
            } else {
                eval_const_fns(els, env, fns, depth + 1)
            }
        }
        ast::Expr::Call { callee, args, .. } => {
            let name = match callee.as_ref() {
                ast::Expr::Path(p) if p.segments.len() == 1 => &p.segments[0].text,
                _ => return None,
            };
            // Kernel conversions are value-transparent in const context.
            if name == "integer" || name == "Char" {
                return eval_const_fns(args.first()?, env, fns, depth + 1);
            }
            let f = fns.get(name.as_str())?;
            let body = f.body.as_ref()?;
            let mut fenv = HashMap::new();
            for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
                let n = p.name.as_ref()?;
                fenv.insert(n.text.clone(), eval_const_fns(a, env, fns, depth + 1)?);
            }
            eval_const_stmts(&body.stmts, &fenv, fns, depth + 1)
        }
        ast::Expr::Unary { op, rhs, .. } => {
            let v = eval_const_fns(rhs, env, fns, depth + 1)?;
            Some(match op {
                ast::UnOp::Neg => -v,
                ast::UnOp::Not => (v == 0) as i64,
            })
        }
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            let (a, b) = (
                eval_const_fns(lhs, env, fns, depth + 1)?,
                eval_const_fns(rhs, env, fns, depth + 1)?,
            );
            Some(match op {
                ast::BinOp::Add => a + b,
                ast::BinOp::Sub => a - b,
                ast::BinOp::Mul => a * b,
                ast::BinOp::Div if b != 0 => a / b,
                ast::BinOp::Shl => a << b,
                ast::BinOp::Shr => a >> b,
                ast::BinOp::Eq => (a == b) as i64,
                ast::BinOp::Ne => (a != b) as i64,
                ast::BinOp::Lt => (a < b) as i64,
                ast::BinOp::Le => (a <= b) as i64,
                ast::BinOp::Gt => (a > b) as i64,
                ast::BinOp::Ge => (a >= b) as i64,
                ast::BinOp::And => (a != 0 && b != 0) as i64,
                ast::BinOp::Or => (a != 0 || b != 0) as i64,
                _ => return None,
            })
        }
        _ => None,
    }
}

/// Statically execute a const-fn body: `return`s and `if`/`else` chains.
pub fn eval_const_stmts(
    stmts: &[ast::Stmt],
    env: &HashMap<String, i64>,
    fns: &HashMap<String, &ast::FnDecl>,
    depth: u32,
) -> Option<i64> {
    for st in stmts {
        match st {
            ast::Stmt::Return { value, .. } => {
                return eval_const_fns(value.as_ref()?, env, fns, depth);
            }
            ast::Stmt::If(iff) => {
                if eval_const_fns(&iff.cond, env, fns, depth)? != 0 {
                    if let Some(v) = eval_const_stmts(&iff.then.stmts, env, fns, depth) {
                        return Some(v);
                    }
                } else {
                    match iff.else_.as_deref() {
                        Some(ast::ElseBranch::Block(b)) => {
                            if let Some(v) = eval_const_stmts(&b.stmts, env, fns, depth) {
                                return Some(v);
                            }
                        }
                        Some(ast::ElseBranch::If(inner)) => {
                            if let Some(v) =
                                eval_const_stmts(std::slice::from_ref(&ast::Stmt::If(inner.clone())), env, fns, depth)
                            {
                                return Some(v);
                            }
                        }
                        None => {}
                    }
                }
            }
            _ => return None,
        }
    }
    None
}

/// Build `enum name -> variant name -> discriminant`. Explicit `= n` values are
/// honoured; unspecified variants continue from the previous discriminant + 1.
/// Index every enum declaration by name (for base-chain resolution).
/// Array-derived Logic vector families (`struct F : Logic[]` / `: Bit[]`,
/// -> signedness. A bodyless struct whose base is an array of a bit scalar
/// (`struct uint : Logic[]`) IS a bit vector — no annotation needed, the shape
/// says so. Signedness is the `Signed` capability. uint/int are just members.
/// Every derived type's inherited width: `struct Byte : Logic[8]` -> 8,
/// `struct Word : Byte` -> 8 (following the base chain). A derived type reuses
/// its base array's size/range (spec: nominal derivation). Testbench evaluators
/// consult this so a local of a derived vector type masks to the right width.
pub fn derived_widths(modules: &[Module]) -> HashMap<String, u32> {
    let mut structs: HashMap<String, &ast::StructDecl> = HashMap::new();
    for m in modules {
        for it in &m.items {
            if let ast::Item::Struct(s) = it {
                structs.insert(s.name.text.clone(), s);
            }
        }
    }
    let (empty_env, empty_fns) = (HashMap::new(), HashMap::new());
    structs
        .iter()
        .filter_map(|(name, s)| {
            let w = s
                .base
                .as_ref()
                .map(|b| type_width(b, &empty_env, &empty_fns, &structs))
                .unwrap_or(0);
            (w > 0).then_some((name.clone(), w))
        })
        .collect()
}

pub fn vector_families(modules: &[Module]) -> std::collections::HashSet<String> {
    // The set of bit-vector families by shape. No signedness — that lives in
    // each type's operator impls. Computed to a fixpoint so a type deriving
    // from *another* vector family (`struct Byte : uint[8]`) is recognized too.
    let structs: Vec<&ast::StructDecl> = modules
        .iter()
        .flat_map(|m| &m.items)
        .filter_map(|it| match it {
            ast::Item::Struct(st) => Some(st),
            _ => None,
        })
        .collect();
    let mut out = std::collections::HashSet::new();
    loop {
        let mut changed = false;
        for st in &structs {
            if !out.contains(&st.name.text) && is_bit_vector_struct(st, &out) {
                out.insert(st.name.text.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    out
}

/// A bodyless struct deriving from an array whose element is a bit scalar
/// (`struct F : Logic[]` / `: Bit[]`) or an already-known vector family
/// (`struct Byte : uint[8]`) — a packed bit vector. This is what makes uint/int
/// and user vectors; the shape (and its base) is the definition, not an
/// attribute.
fn is_bit_vector_struct(st: &ast::StructDecl, families: &std::collections::HashSet<String>) -> bool {
    if !st.fields.is_empty() {
        return false;
    }
    let elem = match &st.base {
        Some(ast::Type::Indexed { base, .. }) => type_head_name(base),
        // A bare derived base (`struct Byte : uint`) reuses the base family.
        Some(ast::Type::Path(p)) => p.segments.last().map(|s| s.text.as_str()),
        _ => None,
    };
    matches!(elem, Some("Logic" | "Bit" | "ULogic"))
        || elem.is_some_and(|h| families.contains(h))
}

fn enum_index(modules: &[Module]) -> HashMap<String, &ast::EnumDecl> {
    let mut out = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Enum(e) = item {
                out.insert(e.name.text.clone(), e);
            }
        }
    }
    out
}

/// The `: Type` head name when it names another enum — i.e. a derivation
/// base rather than a numeric repr.
fn enum_base_name(e: &ast::EnumDecl, enums: &HashMap<String, &ast::EnumDecl>) -> Option<String> {
    let name = type_head_name(e.repr.as_ref()?)?;
    enums.contains_key(name).then(|| name.to_string())
}

/// An enum's effective variants, base chain first then its own declared ones
/// (spec: nominal derivation). `(name, explicit discriminant)`.
fn effective_variants(
    name: &str,
    enums: &HashMap<String, &ast::EnumDecl>,
    seen: &mut Vec<String>,
) -> Vec<(String, Option<i64>)> {
    let Some(e) = enums.get(name) else { return Vec::new() };
    if seen.iter().any(|n| n == name) {
        return Vec::new(); // cycle guard
    }
    seen.push(name.to_string());
    let mut out = match enum_base_name(e, enums) {
        Some(base) => effective_variants(&base, enums, seen),
        None => Vec::new(),
    };
    for v in &e.variants {
        let disc = match &v.value {
            Some(ast::Expr::Int { text, .. }) => parse_int(text).map(|n| n as i64),
            _ => None,
        };
        out.push((v.name.text.clone(), disc));
    }
    seen.pop();
    out
}

/// Every enum's `variant -> discriminant` map, *including inherited variants*
/// from a derivation base (`enum Extended : Base` gets Base's variants too).
/// Consumers (runner, native emitter) share this so derived-enum variant
/// references resolve identically.
pub fn enum_discriminants(modules: &[Module]) -> HashMap<String, HashMap<String, u64>> {
    let enums = enum_index(modules);
    let mut out = HashMap::new();
    for name in enums.keys() {
        let mut vars = HashMap::new();
        let mut next = 0u64;
        for (v, disc) in effective_variants(name, &enums, &mut Vec::new()) {
            let d = disc.map(|d| d as u64).unwrap_or(next);
            vars.insert(v, d);
            next = d + 1;
        }
        out.insert(name.clone(), vars);
    }
    out
}

/// The dotted signal path of a name, struct-field, or constant-index access:
/// `s` -> `"s"`, `s.data` -> `"s.data"`, `a[2]` -> `"a[2]"`. A dynamic index or
/// anything else (calls, slices) yields `None`.
/// Unroll a generate `for i in a..b { let s = Sub {..} }` into concrete
/// sub-instances, substituting the loop index into each instance's name, type
/// arguments, and connection expressions. Plain `let` instances inside the loop
/// body are handled too; nested loops recurse. Non-instance statements are
/// left for the behavioural pass.
fn gather_generate(
    s: &ast::Stmt,
    env: &HashMap<String, i64>,
    loop_idx: &[i64],
    out: &mut Vec<(String, ast::Type, Vec<ast::ConnectArg>)>,
) {
    match s {
        ast::Stmt::Let(l) => {
            if let Some(ast::Expr::Construct { ty: Some(cty), args, .. }) = &l.value {
                // A generated instance (inside a loop) gets the enclosing loop
                // indices appended for a unique name, matching the elaborator's
                // `<name>_<i>` convention.
                let name = if loop_idx.is_empty() {
                    l.name.text.clone()
                } else {
                    let idx: Vec<String> = loop_idx.iter().map(|v| v.to_string()).collect();
                    format!("{}_{}", l.name.text, idx.join("_"))
                };
                out.push((name, cty.clone(), args.clone()));
            }
        }
        // Instance-array element: `stage[i] = Sub { .. }` (index already
        // substituted). The rendered target (`stage[1]`) is the instance name,
        // matching the elaborator so `stage[i].port` reads line up.
        ast::Stmt::Assign { target, value: ast::Expr::Construct { ty: Some(cty), args, .. }, .. } => {
            if let Some(name) = expr_path(target) {
                out.push((name, cty.clone(), args.clone()));
            }
        }
        ast::Stmt::For { var, range: ast::Expr::Range { lo, hi, .. }, body, .. } => {
            if let (Some(a), Some(b)) = (eval_const(lo, env), eval_const(hi, env)) {
                for i in loop_range(a, b) {
                    let mut e = env.clone();
                    e.insert(var.text.clone(), i);
                    let mut idx = loop_idx.to_vec();
                    idx.push(i);
                    for st in &body.stmts {
                        // Substitute the loop index throughout the statement so
                        // `Sub<W=i>` and `wires[i]` become concrete before the
                        // instance is recorded.
                        let st = subst_stmt(st, &var.text, i);
                        gather_generate(&st, &e, &idx, out);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Substitute a bound integer for a single-segment path variable throughout a
/// statement (used to unroll generate loops).
/// Read a generic argument expression as a type: `uint[8]` (parsed as an index
/// expression) becomes the type `uint[8]`, a bare name becomes a path type.
/// Used to substitute a struct's type parameters (`Pair<uint[8]>`).
fn expr_to_type(e: &ast::Expr) -> Option<ast::Type> {
    match e {
        ast::Expr::Path(p) => Some(ast::Type::Path(p.clone())),
        ast::Expr::Index { base, index, span } => Some(ast::Type::Indexed {
            base: Box::new(expr_to_type(base)?),
            index: Some(index.clone()),
            span: *span,
        }),
        _ => None,
    }
}

/// Substitute type parameters (`T -> uint[8]`) in a type, recursing through
/// array/generic/mode wrappers.
fn subst_type_params(ty: &ast::Type, subst: &HashMap<String, ast::Type>) -> ast::Type {
    match ty {
        ast::Type::Path(p) if p.segments.len() == 1 => {
            subst.get(&p.segments[0].text).cloned().unwrap_or_else(|| ty.clone())
        }
        ast::Type::Indexed { base, index, span } => ast::Type::Indexed {
            base: Box::new(subst_type_params(base, subst)),
            index: index.clone(),
            span: *span,
        },
        ast::Type::Generic { base, args, span } => ast::Type::Generic {
            base: Box::new(subst_type_params(base, subst)),
            args: args.clone(),
            span: *span,
        },
        ast::Type::Mode { dir, inner, mode, span } => ast::Type::Mode {
            dir: *dir,
            inner: Box::new(subst_type_params(inner, subst)),
            mode: mode.clone(),
            span: *span,
        },
        _ => ty.clone(),
    }
}

/// Deep-clone a statement, replacing every bare single-segment path named in
/// `map` with its expression. Used to inline a method body: `self` maps to the
/// receiver and each parameter to its argument, so `self.valid = '1'` in a
/// method becomes `<recv>.valid = '1'` at the call site (spec 3.20). Public so
/// the testbench evaluators (siox-run, the native emitter) inline method calls
/// the same way hardware lowering does.
pub fn subst_stmt_paths(s: &ast::Stmt, map: &HashMap<String, ast::Expr>) -> ast::Stmt {
    use ast::Stmt;
    match s {
        Stmt::Assign { target, value, after, span } => Stmt::Assign {
            target: subst_expr_paths(target, map),
            value: subst_expr_paths(value, map),
            after: after.as_ref().map(|a| subst_expr_paths(a, map)),
            span: *span,
        },
        Stmt::If(iff) => Stmt::If(subst_if_paths(iff, map)),
        Stmt::Match(m) => Stmt::Match(ast::MatchStmt {
            scrutinee: subst_expr_paths(&m.scrutinee, map),
            arms: m
                .arms
                .iter()
                .map(|a| ast::MatchArm {
                    pattern: a.pattern.clone(),
                    body: subst_block_paths(&a.body, map),
                    span: a.span,
                })
                .collect(),
            span: m.span,
        }),
        Stmt::For { var, range, body, span } => Stmt::For {
            var: var.clone(),
            range: subst_expr_paths(range, map),
            body: subst_block_paths(body, map),
            span: *span,
        },
        Stmt::Let(l) => {
            let mut l = l.clone();
            l.value = l.value.as_ref().map(|v| subst_expr_paths(v, map));
            Stmt::Let(l)
        }
        Stmt::Expr(e) => Stmt::Expr(subst_expr_paths(e, map)),
        Stmt::Return { value, span } => Stmt::Return {
            value: value.as_ref().map(|v| subst_expr_paths(v, map)),
            span: *span,
        },
    }
}

fn subst_block_paths(b: &ast::Block, map: &HashMap<String, ast::Expr>) -> ast::Block {
    ast::Block {
        stmts: b.stmts.iter().map(|s| subst_stmt_paths(s, map)).collect(),
        span: b.span,
    }
}

fn subst_if_paths(iff: &ast::IfStmt, map: &HashMap<String, ast::Expr>) -> ast::IfStmt {
    ast::IfStmt {
        cond: subst_expr_paths(&iff.cond, map),
        then: subst_block_paths(&iff.then, map),
        else_: iff.else_.as_ref().map(|e| {
            Box::new(match e.as_ref() {
                ast::ElseBranch::Block(b) => ast::ElseBranch::Block(subst_block_paths(b, map)),
                ast::ElseBranch::If(i) => ast::ElseBranch::If(subst_if_paths(i, map)),
            })
        }),
        span: iff.span,
    }
}

/// Deep-clone an expression, replacing every bare single-segment path named in
/// `map` with its mapped expression (the value-side counterpart of
/// [`subst_stmt_paths`]).
pub fn subst_expr_paths(e: &ast::Expr, map: &HashMap<String, ast::Expr>) -> ast::Expr {
    use ast::Expr;
    let sub = |x: &Expr| Box::new(subst_expr_paths(x, map));
    match e {
        Expr::Path(p) if p.segments.len() == 1 => {
            map.get(&p.segments[0].text).cloned().unwrap_or_else(|| e.clone())
        }
        Expr::Field { base, field, span } => {
            Expr::Field { base: sub(base), field: field.clone(), span: *span }
        }
        Expr::SysAttr { base, attr, span } => {
            Expr::SysAttr { base: sub(base), attr: attr.clone(), span: *span }
        }
        Expr::Index { base, index, span } => {
            Expr::Index { base: sub(base), index: sub(index), span: *span }
        }
        Expr::Range { lo, hi, span } => Expr::Range { lo: sub(lo), hi: sub(hi), span: *span },
        Expr::Unary { op, rhs, span } => Expr::Unary { op: *op, rhs: sub(rhs), span: *span },
        Expr::Binary { op, lhs, rhs, span } => {
            Expr::Binary { op: op.clone(), lhs: sub(lhs), rhs: sub(rhs), span: *span }
        }
        Expr::IfExpr { cond, then, els, span } => {
            Expr::IfExpr { cond: sub(cond), then: sub(then), els: sub(els), span: *span }
        }
        Expr::Call { callee, args, bang, span } => Expr::Call {
            callee: sub(callee),
            args: args.iter().map(|a| subst_expr_paths(a, map)).collect(),
            bang: *bang,
            span: *span,
        },
        Expr::Concat { parts, span } => Expr::Concat {
            parts: parts.iter().map(|p| subst_expr_paths(p, map)).collect(),
            span: *span,
        },
        Expr::Array { elems, span } => Expr::Array {
            elems: elems.iter().map(|e| subst_expr_paths(e, map)).collect(),
            span: *span,
        },
        Expr::Construct { ty, args, span } => Expr::Construct {
            ty: ty.clone(),
            args: args
                .iter()
                .map(|a| ast::ConnectArg {
                    field: a.field.clone(),
                    value: a.value.as_ref().map(|v| subst_expr_paths(v, map)),
                    span: a.span,
                })
                .collect(),
            span: *span,
        },
        other => other.clone(),
    }
}

fn subst_stmt(s: &ast::Stmt, var: &str, val: i64) -> ast::Stmt {
    match s {
        ast::Stmt::Let(l) => {
            let mut l = l.clone();
            l.value = l.value.as_ref().map(|v| subst_expr(v, var, val));
            ast::Stmt::Let(l)
        }
        ast::Stmt::For { var: v, range, body, span } => ast::Stmt::For {
            var: v.clone(),
            range: subst_expr(range, var, val),
            body: {
                let mut b = body.clone();
                b.stmts = b.stmts.iter().map(|st| subst_stmt(st, var, val)).collect();
                b
            },
            span: *span,
        },
        // `stage[i] = Sub { .x = w[i] }`: substitute in both the indexed target
        // and the construct, so instance-array elements unroll concretely.
        ast::Stmt::Assign { target, value, after, span } => ast::Stmt::Assign {
            target: subst_expr(target, var, val),
            value: subst_expr(value, var, val),
            after: after.as_ref().map(|a| subst_expr(a, var, val)),
            span: *span,
        },
        other => other.clone(),
    }
}

/// Deep-clone an expression, replacing every bare `var` reference with the
/// integer literal `val`. Also rewrites index/type-argument expressions.
fn subst_expr(e: &ast::Expr, var: &str, val: i64) -> ast::Expr {
    use ast::Expr;
    let sub = |x: &Expr| Box::new(subst_expr(x, var, val));
    match e {
        Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == var => {
            Expr::Int { text: val.to_string(), span: p.span }
        }
        Expr::Field { base, field, span } => {
            Expr::Field { base: sub(base), field: field.clone(), span: *span }
        }
        Expr::SysAttr { base, attr, span } => {
            Expr::SysAttr { base: sub(base), attr: attr.clone(), span: *span }
        }
        Expr::Index { base, index, span } => {
            Expr::Index { base: sub(base), index: sub(index), span: *span }
        }
        Expr::Range { lo, hi, span } => Expr::Range { lo: sub(lo), hi: sub(hi), span: *span },
        // Fold constant arithmetic so a substituted index like `wires[i+1]`
        // becomes the literal `wires[2]` that `expr_path` can resolve.
        Expr::Unary { op, rhs, span } => {
            let n = Expr::Unary { op: *op, rhs: sub(rhs), span: *span };
            fold_const(n, *span)
        }
        Expr::Binary { op, lhs, rhs, span } => {
            let n = Expr::Binary { op: op.clone(), lhs: sub(lhs), rhs: sub(rhs), span: *span };
            fold_const(n, *span)
        }
        Expr::IfExpr { cond, then, els, span } => Expr::IfExpr {
            cond: sub(cond),
            then: sub(then),
            els: sub(els),
            span: *span,
        },
        Expr::Call { callee, args, bang, span } => Expr::Call {
            callee: sub(callee),
            args: args.iter().map(|a| subst_expr(a, var, val)).collect(),
            bang: *bang,
            span: *span,
        },
        Expr::Concat { parts, span } => Expr::Concat {
            parts: parts.iter().map(|p| subst_expr(p, var, val)).collect(),
            span: *span,
        },
        Expr::Array { elems, span } => Expr::Array {
            elems: elems.iter().map(|e| subst_expr(e, var, val)).collect(),
            span: *span,
        },
        Expr::Construct { ty, args, span } => Expr::Construct {
            ty: ty.as_ref().map(|t| subst_type(t, var, val)),
            args: args
                .iter()
                .map(|a| ast::ConnectArg {
                    field: a.field.clone(),
                    value: a.value.as_ref().map(|v| subst_expr(v, var, val)),
                    span: a.span,
                })
                .collect(),
            span: *span,
        },
        other => other.clone(),
    }
}

/// The values a `for i in lo..hi` loop visits. Range endpoints are **inclusive
/// and directional**, matching bit slices and array ranges elsewhere in the
/// language: `0..2` yields 0,1,2 and `2..0` yields 2,1,0.
pub fn loop_range(a: i64, b: i64) -> Vec<i64> {
    if a <= b {
        (a..=b).collect()
    } else {
        (b..=a).rev().collect()
    }
}

/// Collapse a now-constant arithmetic node to an integer literal, so unrolled
/// index expressions resolve as plain `Int`s. Non-constant nodes pass through.
fn fold_const(e: ast::Expr, span: siox_diag::Span) -> ast::Expr {
    match eval_const(&e, &HashMap::new()) {
        Some(v) => ast::Expr::Int { text: v.to_string(), span },
        None => e,
    }
}

/// Substitute the loop index into a type's index/generic-argument expressions.
fn subst_type(t: &ast::Type, var: &str, val: i64) -> ast::Type {
    match t {
        ast::Type::Indexed { base, index, span } => ast::Type::Indexed {
            base: Box::new(subst_type(base, var, val)),
            index: index.as_ref().map(|i| Box::new(subst_expr(i, var, val))),
            span: *span,
        },
        ast::Type::Generic { base, args, span } => ast::Type::Generic {
            base: Box::new(subst_type(base, var, val)),
            args: args
                .iter()
                .map(|a| match a {
                    ast::GenericArg::Positional(e) => {
                        ast::GenericArg::Positional(subst_expr(e, var, val))
                    }
                    ast::GenericArg::Named { name, value } => ast::GenericArg::Named {
                        name: name.clone(),
                        value: subst_expr(value, var, val),
                    },
                })
                .collect(),
            span: *span,
        },
        ast::Type::Mode { dir, inner, mode, span } => ast::Type::Mode {
            dir: *dir,
            inner: Box::new(subst_type(inner, var, val)),
            mode: mode.clone(),
            span: *span,
        },
        ast::Type::Path(_) => t.clone(),
    }
}

fn expr_path(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        ast::Expr::Field { base, field, .. } => {
            Some(format!("{}.{}", expr_path(base)?, field.text))
        }
        ast::Expr::Index { base, index, .. } => match index.as_ref() {
            ast::Expr::Int { text, .. } => Some(format!("{}[{}]", expr_path(base)?, parse_int(text)?)),
            _ => None,
        },
        _ => None,
    }
}

/// The `(element type, length)` if `ty` is an array — an `Indexed` type whose
/// base is *not* an integer (`Bit[4]`), as opposed to a vector (`uint[8]`).
/// The element type and **ordered element indices** of an array type.
/// A width-only index (`Bit[4]`) is ascending `0..=3`; a range keeps its
/// written direction (`Logic[7..0]` yields 7,6,...,0). A single-segment path
/// as the index may name a range constant.
fn array_of<'t>(
    ty: &'t ast::Type,
    env: &HashMap<String, i64>,
    const_ranges: &HashMap<String, (i64, i64)>,
    families: &std::collections::HashSet<String>,
) -> Option<(&'t ast::Type, Vec<i64>)> {
    let ast::Type::Indexed { base, index: Some(index), .. } = ty else { return None };
    // A Logic-vector family (uint/int/user) `F[N]` is one N-bit signal, not an
    // N-element array — but only when the base is DIRECTLY the family (`uint`),
    // not when it is itself indexed (`uint[8][4]` is an array of vectors).
    let base_is_family = matches!(base.as_ref(), ast::Type::Path(p)
        if p.segments.last().map(|s| s.text.as_str()).is_some_and(|h| families.contains(h)));
    if is_int_type(base) || base_is_family {
        return None;
    }
    let bounds = match index.as_ref() {
        ast::Expr::Range { lo, hi, .. } => {
            Some((eval_const(lo, env)?, eval_const(hi, env)?))
        }
        ast::Expr::Path(p) if p.segments.len() == 1 => {
            const_ranges.get(&p.segments[0].text).copied()
        }
        _ => None,
    };
    let indices = match bounds {
        Some((a, b)) if a <= b => (a..=b).collect(),
        Some((a, b)) => (b..=a).rev().collect(),
        None => (0..eval_const(index, env).unwrap_or(0).max(0)).collect(),
    };
    Some((base, indices))
}

/// The kernel `integer` scalar (a bare word). uint/int are NOT here — they
/// are `#[vector]` families recognized via the family set, not by name.
fn is_int_type(ty: &ast::Type) -> bool {
    matches!(ty, ast::Type::Path(p)
        if p.segments.last().map(|s| s.text.as_str()) == Some("integer"))
}

/// Build `enum name -> bit width`: the `repr` width if given (`enum S: uint[2]`),
/// else the bits needed for the variant count.
fn enum_reprs(modules: &[Module]) -> HashMap<String, u32> {
    let empty = HashMap::new();
    let enums = enum_index(modules);
    let mut out = HashMap::new();
    for (name, e) in &enums {
        // A numeric `: repr` sets the width explicitly; otherwise the width
        // covers the effective variant count (inherited + declared).
        let w = if e.repr.is_some() && enum_base_name(e, &enums).is_none() {
            type_width(e.repr.as_ref().unwrap(), &empty, &HashMap::new(), &HashMap::new())
        } else {
            let n = effective_variants(name, &enums, &mut Vec::new()).len().max(1) as u32;
            if n <= 1 { 1 } else { u32::BITS - (n - 1).leading_zeros() }
        };
        out.insert(name.clone(), w);
    }
    out
}

fn has_attr(e: &ast::EntityDecl, name: &str) -> bool {
    e.attrs
        .iter()
        .any(|a| a.name.segments.last().map(|s| s.text.as_str()) == Some(name))
}

fn type_head_name(t: &ast::Type) -> Option<&str> {
    match t {
        ast::Type::Path(p) => p.segments.first().map(|s| s.text.as_str()),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => type_head_name(base),
        ast::Type::Mode { inner, .. } => type_head_name(inner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use siox_diag::FileId;

    fn lower_src(src: &str) -> Design {
        // uint/int are library types (attribute-marked vectors), not seeded.
        let src = format!(
            "{src}\nstruct uint : Logic[];\nstruct int : Logic[];\n"
        );
        let src = src.as_str();
        let mut sink = DiagnosticSink::new();
        let module = siox_syntax::parse_module(FileId(0), src, &mut sink);
        assert_eq!(sink.error_count(), 0, "parse errors:\n{src}");
        let modules = std::slice::from_ref(&module);
        let resolved = siox_resolve::resolve(modules, &mut sink);
        let typed = siox_types::check(modules, &resolved, &mut sink);
        let hier = siox_elab::elaborate(modules, &typed, &mut sink);
        lower(modules, &hier, &mut sink)
    }

    fn lower_diags(src: &str) -> Vec<String> {
        let src = format!("{src}\nstruct uint : Logic[];\nstruct int : Logic[];\n");
        let mut sink = DiagnosticSink::new();
        let module = siox_syntax::parse_module(FileId(0), &src, &mut sink);
        let modules = std::slice::from_ref(&module);
        let resolved = siox_resolve::resolve(modules, &mut sink);
        let typed = siox_types::check(modules, &resolved, &mut sink);
        let hier = siox_elab::elaborate(modules, &typed, &mut sink);
        let _ = lower(modules, &hier, &mut sink);
        sink.diagnostics().iter().map(|d| format!("{:?}: {}", d.code, d.message)).collect()
    }

    #[test]
    fn bit_pattern_masks() {
        assert_eq!(bit_pattern_mask("b\"01??\""), Some((0b1100, 0b0100)));
        assert_eq!(bit_pattern_mask("b\"0000_11??\""), Some((0b11111100, 0b00001100)));
        assert_eq!(bit_pattern_mask("x\"A?\""), Some((0xF0, 0xA0)));
        assert_eq!(bit_pattern_mask("x\"?3\""), Some((0x0F, 0x03)));
        assert_eq!(bit_pattern_mask("b\"2\""), None); // bad binary digit
    }

    #[test]
    fn if_else_mux_is_not_a_latch() {
        // A signal assigned in both the `if` and the `else` is fully covered —
        // no possible-latch warning — but one assigned only in the `if` is.
        let covered = lower_diags(
            "module m;\n\
             entity M { in c: Bit; in a: uint[8]; in b: uint[8]; out y: uint[8]; }\n\
             impl M { if c { y = a; } else { y = b; } }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let c: Bit; let a: uint[8]; let b: uint[8]; let y: uint[8];\n\
               let dut = M { .c, .a, .b, .y };\n\
             }\n",
        );
        assert!(
            !covered.iter().any(|d| d.contains("inferred latch")),
            "if/else mux wrongly flagged: {covered:?}"
        );

        let latch = lower_diags(
            "module m;\n\
             entity M { in c: Bit; in a: uint[8]; out y: uint[8]; }\n\
             impl M { if c { y = a; } }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let c: Bit; let a: uint[8]; let y: uint[8];\n\
               let dut = M { .c, .a, .y };\n\
             }\n",
        );
        assert!(
            latch.iter().any(|d| d.contains("inferred latch")),
            "true latch (no else) should warn: {latch:?}"
        );
    }

    #[test]
    fn strict_assignment_width_mismatch() {
        // A parameterized width (`uint[W]`) the type checker can't see resolves
        // at elaboration; assigning a 16-bit signal to an 8-bit target is then a
        // width mismatch surfaced by IR lowering.
        let bad = lower_diags(
            "module m;\n\
             entity E { in b: uint[W]; out y: uint[8]; }\n\
             impl E { y = b; }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let b: uint[16]; let y: uint[8];\n\
               let dut = E<W=16> { .b, .y };\n\
             }\n",
        );
        assert!(bad.iter().any(|d| d.contains("width mismatch")), "{bad:?}");

        // A matching-width slice of the same signal is fine — the value width
        // (8) equals the target (8).
        let ok = lower_diags(
            "module m;\n\
             entity E { in b: uint[W]; out y: uint[8]; }\n\
             impl E { y = b[7..0]; }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let b: uint[16]; let y: uint[8];\n\
               let dut = E<W=16> { .b, .y };\n\
             }\n",
        );
        assert!(!ok.iter().any(|d| d.contains("width mismatch")), "{ok:?}");
    }

    #[test]
    fn combinational_loop_lint() {
        // `t = t + a;` is a zero-delay self-cycle -> flagged; a plain chain
        // (`y = x + 1`) is not.
        let diags = lower_diags(
            "module m;\n\
             entity L { in a: uint[8]; out y: uint[8]; }\n\
             impl L { let t: uint[8]; t = t + a; y = t; }\n\
             #[top] entity Top {}\n\
             impl Top { let a: uint[8]; let y: uint[8]; let d = L { .a, .y }; }\n",
        );
        let loops: Vec<&String> = diags.iter().filter(|d| d.contains("W-P010")).collect();
        assert!(!loops.is_empty(), "self-cycle flagged: {diags:?}");
        assert!(loops.iter().any(|d| d.contains(".t")), "names t: {loops:?}");

        let ok = lower_diags(
            "module m;\n\
             entity C { in x: uint[8]; out y: uint[8]; }\n\
             impl C { y = x + 1; }\n\
             #[top] entity Top {}\n\
             impl Top { let x: uint[8]; let y: uint[8]; let d = C { .x, .y }; }\n",
        );
        assert!(!ok.iter().any(|d| d.contains("W-P010")), "no false positive: {ok:?}");
    }

    #[test]
    fn possible_latch_lint() {
        // `y` is only assigned under a condition (inferred latch); `z` has an
        // unconditional default and must not be flagged.
        let diags = lower_diags(
            "module m;\n\
             entity L { in c: Logic; in a: uint[8]; out y: uint[8]; out z: uint[8]; }\n\
             impl L { if c == '1' { y = a; } z = a; }\n\
             #[top] entity Top {}\n\
             impl Top { let c: Logic; let a: uint[8]; let y: uint[8]; let z: uint[8];\n\
               let d = L { .c, .a, .y, .z }; }\n",
        );
        let latch: Vec<&String> = diags.iter().filter(|d| d.contains("W-P002")).collect();
        assert_eq!(latch.len(), 1, "exactly one latch warning: {diags:?}");
        assert!(latch[0].contains(".y"), "flags y, not z: {latch:?}");
    }

    #[test]
    fn enum_signals_carry_symbols() {
        // A Logic-typed signal records its enum type, and the design exports the
        // discriminant -> symbol map (with std's char-variant names) so
        // consumers can print `'X'` instead of `3`.
        let d = lower_src(
            "module m;\n\
             enum Logic { '0', '1', 'Z', 'X' }\n\
             enum State { Idle, Run }\n\
             entity E { in a: Logic; out s: State; }\n\
             impl E { s = State::Idle; }\n\
             #[top] entity Top {}\n\
             impl Top { let a: Logic; let s: State; let e = E { .a, .s }; }\n",
        );
        let sig = |p: &str| d.signals.iter().find(|s| s.path == p).unwrap();
        assert_eq!(sig("Top.e.a").enum_type.as_deref(), Some("Logic"));
        assert_eq!(sig("Top.e.s").enum_type.as_deref(), Some("State"));
        assert_eq!(d.enum_syms["Logic"].get(&3).map(String::as_str), Some("'X'"));
        assert_eq!(d.enum_syms["State"].get(&0).map(String::as_str), Some("Idle"));
    }

    const COUNTER: &str = "module m;\n\
        entity Counter<W: integer> {\n\
          in clk: Bit;\n\
          in rst: Logic;\n\
          in en: Bit;\n\
          out count: uint[W];\n\
        }\n\
        impl Counter<W: integer> {\n\
          let value: uint[W] = 0;\n\
          if clk::rising {\n\
            if rst == '1' {\n\
              value = 0;\n\
            } else if en {\n\
              value = value + 1;\n\
            }\n\
          }\n\
          count = value;\n\
        }\n\
        #[test]\n\
        entity H {}\n\
        impl H {\n\
          let clk: Bit = '0';\n\
          let rst: Logic = '1';\n\
          let en: Bit = '1';\n\
          let count: uint[8];\n\
          let dut = Counter<W = 8> { .clk, .rst, .en, .count };\n\
        }\n";

    #[test]
    fn lowers_signals_driver_and_event_block() {
        let d = lower_src(COUNTER);
        // Counter signals: clk, rst, en, count, value. The instance's `W = 8`
        // makes the parametric `uint[W]` widths concrete.
        let count = d.signals.iter().find(|s| s.path == "H.dut.count").unwrap();
        assert_eq!(count.width, 8);
        assert!(d.signals.iter().any(|s| s.path == "H.dut.value"));
        // One combinational driver: count = value.
        assert_eq!(d.drivers.len(), 1);
        // One event block (clk::rising) with two next-state updates.
        assert_eq!(d.event_blocks.len(), 1);
        assert_eq!(d.event_blocks[0].updates.len(), 2);
    }

    #[test]
    fn lowers_nested_instances_with_connections() {
        // Add2 instantiates two Add1s wired through `mid`. Each instance must
        // get its own signals, and every port connection must become a driver.
        let src = "module m;\n\
            entity Add1 { in a: uint[8]; out y: uint[8]; }\n\
            impl Add1 { y = a + 1; }\n\
            entity Add2 { in a: uint[8]; out y: uint[8]; }\n\
            impl Add2 {\n\
              let mid: uint[8];\n\
              let s1 = Add1 { .a = a, .y = mid };\n\
              let s2 = Add1 { .a = mid, .y = y };\n\
            }\n\
            #[test] entity T {}\n\
            impl T {\n\
              let a: uint[8] = 10;\n\
              let y: uint[8];\n\
              let dut = Add2 { .a, .y };\n\
            }\n";
        let d = lower_src(src);
        let id = |path: &str| d.signals.iter().position(|s| s.path == path).map(|i| SignalId(i as u32));
        // Two distinct Add1 instances, each with its own signals.
        assert!(id("T.dut.s1.a").is_some() && id("T.dut.s1.y").is_some());
        assert!(id("T.dut.s2.a").is_some() && id("T.dut.s2.y").is_some());
        // Every connection is a driver: `in` ports read the parent, `out`
        // ports drive it.
        let wired = |target: &str, source: &str| {
            let (t, s) = (id(target).unwrap(), id(source).unwrap());
            d.drivers
                .iter()
                .any(|dr| dr.target == t && matches!(&dr.expr, Expr::Current(x) if *x == s))
        };
        assert!(wired("T.dut.s1.a", "T.dut.a"), "s1.a <- a");
        assert!(wired("T.dut.mid", "T.dut.s1.y"), "mid <- s1.y");
        assert!(wired("T.dut.s2.a", "T.dut.mid"), "s2.a <- mid");
        assert!(wired("T.dut.y", "T.dut.s2.y"), "y <- s2.y");
    }

    #[test]
    fn if_expression_lowers_to_select() {
        let d = lower_src(
            "module m;\n\
             entity Mux { in sel: Bit; in a: uint[8]; in b: uint[8]; out y: uint[8]; }\n\
             impl Mux { y = if sel { a } else { b }; }\n\
             #[test] entity T {}\n\
             impl T { let sel: Bit; let a: uint[8]; let b: uint[8]; let y: uint[8];\n\
               let dut = Mux { .sel, .a, .b, .y }; }\n",
        );
        let y = d.signals.iter().position(|s| s.path == "T.dut.y").map(|i| SignalId(i as u32)).unwrap();
        let dr = d.drivers.iter().find(|dr| dr.target == y).unwrap();
        assert!(matches!(&dr.expr, Expr::Select { .. }), "if-expression must lower to a select");
    }

    #[test]
    fn validate_accepts_good_and_flags_bad_ir() {
        // A lowered counter is well-formed.
        assert!(lower_src(COUNTER).validate().is_empty());

        let sig = |w: u32| Signal { path: "s".into(), width: w, real: false, char: false, range: None, init: 0, enum_type: None };
        // Out-of-range signal id, an Unknown, a bad slice, and a width-0 signal.
        let bad = Design {
            signals: vec![sig(0)], // width 0 -> flagged
            drivers: vec![Driver {
                target: SignalId(9), // out of range
                cond: Some(Expr::Unknown),
                expr: Expr::Slice { base: Box::new(Expr::Current(SignalId(0))), hi: 1, lo: 3 },
                ctx: 0,
            }],
            event_blocks: vec![],
            enum_syms: HashMap::new(),
            base_dir: Default::default(),
        };
        let issues = bad.validate();
        assert!(issues.iter().any(|i| i.contains("unknown width")), "{issues:?}");
        assert!(issues.iter().any(|i| i.contains("out of range")), "{issues:?}");
        assert!(issues.iter().any(|i| i.contains("Unknown")), "{issues:?}");
        assert!(issues.iter().any(|i| i.contains("slice bounds")), "{issues:?}");
    }

    #[test]
    fn processes_carry_sensitivity_and_write_sets() {
        let d = lower_src(COUNTER);
        let sig = |path: &str| {
            SignalId(d.signals.iter().position(|s| s.path == path).unwrap() as u32)
        };
        let procs = d.processes();
        // A combinational process for `count = value` and one event process.
        let comb = procs
            .iter()
            .find(|p| matches!(&p.kind, ProcessKind::Comb { target, .. } if *target == sig("H.dut.count")))
            .unwrap();
        assert_eq!(comb.reads, vec![sig("H.dut.value")]);
        assert_eq!(comb.writes, vec![sig("H.dut.count")]);

        let event = procs
            .iter()
            .find(|p| matches!(p.kind, ProcessKind::Event { .. }))
            .unwrap();
        // Sensitive to clk (edge condition), rst and en (update guards),
        // value (increment). Writes value.
        for s in ["H.dut.clk", "H.dut.rst", "H.dut.en", "H.dut.value"] {
            assert!(event.reads.contains(&sig(s)), "event not sensitive to {s}");
        }
        assert_eq!(event.writes, vec![sig("H.dut.value")]);
    }

    #[test]
    fn composite_and_enum_signals_flatten_with_widths() {
        let d = lower_src(
            "module m;\n\
             enum S: uint[2] { A, B, C }\n\
             struct P { flag: Bit, val: uint[8] }\n\
             entity E { in p: P; in a: Bit[3]; out s: S; }\n\
             impl E {}\n\
             #[top] entity H {}\n\
             impl H { let p: P; let a: Bit[3]; let s: S; let dut = E { .p, .a, .s }; }\n",
        );
        let width = |path: &str| d.signals.iter().find(|x| x.path == path).map(|x| x.width);
        assert_eq!(width("H.dut.p.flag"), Some(1)); // struct field
        assert_eq!(width("H.dut.p.val"), Some(8));
        assert_eq!(width("H.dut.a[0]"), Some(1)); // array element
        assert_eq!(width("H.dut.a[2]"), Some(1));
        assert_eq!(width("H.dut.s"), Some(2)); // enum repr width
    }

    #[test]
    fn partial_bit_slice_write() {
        // `y = 0; y[3..0] = a` merges: low nibble = a, high bits held from 0.
        let d = lower_src(
            "module m;\n\
             entity E { in a: uint[4]; out y: uint[8]; }\n\
             impl E { y = 0; y[3..0] = a; }\n\
             #[top] entity H {}\n\
             impl H { let a: uint[4]; let y: uint[8]; let dut = E { .a, .y }; }\n",
        );
        // The y driver should be a read-modify-write (an Or of a masked base
        // and a shifted value), not a bare assignment.
        let dr = d.drivers.iter().find(|dr| d.signals[dr.target.0 as usize].path == "H.dut.y").unwrap();
        assert!(matches!(dr.expr, Expr::Binary { op: BinOp::Or, .. }), "slice write merges: {:?}", dr.expr);
    }

    #[test]
    fn derived_enum_width_covers_inherited_variants() {
        // Ext has 4 effective variants (A, B inherited + C, D) -> 2 bits.
        let d = lower_src(
            "module m;\n\
             enum Base { A, B }\n\
             enum Ext : Base { C, D }\n\
             entity E { out x: Ext; }\n\
             impl E { x = Ext::A; }\n\
             #[top] entity H {}\n\
             impl H { let x: Ext; let dut = E { .x }; }\n",
        );
        let sig = d.signals.iter().find(|s| s.path == "H.dut.x").unwrap();
        assert_eq!(sig.width, 2, "inherited variants widen the enum");
    }

    #[test]
    fn derived_struct_inherits_base_fields() {
        // Packet flattens Header's fields (base-first) then its own.
        let d = lower_src(
            "module m;\n\
             struct Header { valid: Bit, kind: uint[4] }\n\
             struct Packet : Header { data: uint[8] }\n\
             entity E { out p: Packet; }\n\
             impl E {}\n\
             #[top] entity H {}\n\
             impl H { let p: Packet; let dut = E { .p }; }\n",
        );
        let width = |path: &str| d.signals.iter().find(|x| x.path == path).map(|x| x.width);
        assert_eq!(width("H.dut.p.valid"), Some(1), "inherited field");
        assert_eq!(width("H.dut.p.kind"), Some(4), "inherited field");
        assert_eq!(width("H.dut.p.data"), Some(8), "own field");
    }

    #[test]
    fn same_variant_enum_derivation_is_representation_identical() {
        // A bodyless derivation keeps the base's width and discriminants.
        let d = lower_src(
            "module m;\n\
             enum Base { A, B, C }\n\
             enum Alias : Base;\n\
             entity E { out x: Alias; }\n\
             impl E { x = Alias::B; }\n\
             #[top] entity H {}\n\
             impl H { let x: Alias; let dut = E { .x }; }\n",
        );
        let sig = d.signals.iter().find(|s| s.path == "H.dut.x").unwrap();
        assert_eq!(sig.width, 2, "3 variants -> 2 bits, same as base");
    }

    #[test]
    fn rising_lowers_to_event_old_current() {
        let d = lower_src(COUNTER);
        let rendered = d.to_ir_string();
        // clk::rising expands into the explicit Event/Old/Current form.
        assert!(rendered.contains("Event(H.dut.clk)"));
        assert!(rendered.contains("Old(H.dut.clk) == '0'"));
        assert!(rendered.contains("H.dut.clk == '1'"));
        // The combinational driver and the next-state updates are present.
        assert!(rendered.contains("driver H.dut.count = H.dut.value"));
        assert!(rendered.contains("next H.dut.value = 0"));
    }

    #[test]
    fn priority_conditions_accumulate() {
        let d = lower_src(COUNTER);
        let u = &d.event_blocks[0].updates;
        // First update guarded by rst == '1'.
        assert!(matches!(&u[0].cond, Some(Expr::Binary { op: BinOp::Eq, .. })));
        // Second guarded by the negation AND en.
        assert!(matches!(&u[1].cond, Some(Expr::Binary { op: BinOp::And, .. })));
    }
}
