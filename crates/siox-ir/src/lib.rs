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
}

/// A combinational driver: `signal = expr` under `cond` (spec 3.14 source-order
/// override is resolved during lowering into a priority chain).
#[derive(Clone, Debug)]
pub struct Driver {
    pub target: SignalId,
    pub cond: Option<Expr>,
    pub expr: Expr,
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
    Nand,
    Or,
    Nor,
    Xor,
    Xnor,
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

/// Lower the elaborated design into simulation IR.
pub fn lower(modules: &[Module], hier: &Hierarchy, sink: &mut DiagnosticSink) -> Design {
    let mut l = Lowering::new(sink);
    l.collect(modules);
    l.enum_variants = enum_discriminants(modules);
    l.enum_reprs = enum_reprs(modules);

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
    l.out
}

struct Lowering<'a> {
    sink: &'a mut DiagnosticSink,
    entities: HashMap<String, &'a ast::EntityDecl>,
    impls: HashMap<String, Vec<&'a ast::ImplDecl>>,
    /// Entity name -> its instance's concrete parameter values.
    entity_params: HashMap<String, HashMap<String, i64>>,
    /// Enum name -> variant name -> discriminant value.
    enum_variants: HashMap<String, HashMap<String, u64>>,
    /// Struct name -> its declaration (for flattening struct signals).
    structs: HashMap<String, &'a ast::StructDecl>,
    /// Enum name -> its bit width (repr, or bits for the variant count).
    enum_reprs: HashMap<String, u32>,
    /// (trait name, target type) -> the impl's fns with the impl's declared
    /// rhs type (the `integer` in `impl Add<integer> for T`; `None` reads as
    /// `Self`). Overloads select by that rhs, or the fn's rhs parameter type.
    op_impls: HashMap<(String, String), Vec<(&'a ast::FnDecl, Option<String>)>>,
    /// Literal suffix -> (target type, fn), for suffix inlining.
    suffix_impls: HashMap<String, (String, &'a ast::FnDecl)>,
    /// Module-level integer constants (`const N: integer = 4`).
    consts: HashMap<String, i64>,
    /// Module-level range constants (`const BYTE: range = 7..0`), as written
    /// (left, right) so direction is preserved.
    const_ranges: HashMap<String, (i64, i64)>,
    /// Type aliases (`using Word = uint[32]`).
    aliases: HashMap<String, ast::Type>,
    /// The active entity's width environment (consts + instance params),
    /// for const-evaluating slice bounds during expression lowering.
    cur_env: HashMap<String, i64>,
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
            entities: HashMap::new(),
            impls: HashMap::new(),
            entity_params: HashMap::new(),
            enum_variants: HashMap::new(),
            structs: HashMap::new(),
            enum_reprs: HashMap::new(),
            op_impls: HashMap::new(),
            suffix_impls: HashMap::new(),
            consts: HashMap::new(),
            const_ranges: HashMap::new(),
            aliases: HashMap::new(),
            cur_env: HashMap::new(),
            out: Design::default(),
            locals: HashMap::new(),
            local_enum: HashMap::new(),
            local_struct: HashMap::new(),
            local_char: std::collections::HashSet::new(),
            local_array: HashMap::new(),
        }
    }

    fn collect(&mut self, modules: &'a [Module]) {
        for m in modules {
            for item in &m.items {
                match item {
                    ast::Item::Entity(e) => {
                        self.entities.insert(e.name.text.clone(), e);
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
                        }
                    }
                    ast::Item::Using(u) => {
                        if let ast::UsingKind::Alias { name, ty } = &u.kind {
                            self.aliases.insert(name.text.clone(), ty.clone());
                        }
                    }
                    ast::Item::Impl(im) if im.trait_.is_none() => {
                        if let Some(name) = type_head_name(&im.target) {
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
                                let rhs_arg = im.trait_args.first().and_then(|a| match a {
                                    ast::GenericArg::Positional(ast::Expr::Path(p)) => {
                                        p.segments.last().map(|s| s.text.clone())
                                    }
                                    _ => None,
                                });
                                for it in &im.items {
                                    if let ast::ImplItem::Fn(f) = it {
                                        self.op_impls
                                            .entry((tr.text.clone(), ty.to_string()))
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
        self.lower_body(name, name, &env);
    }

    /// Lower each `let inst = Sub { .. }` DUT of a testbench into its own
    /// namespace `<testbench>.<inst>.*` (with the DUT's internal logic and
    /// sub-instances). No testbench signals, statements, or top connections.
    fn lower_testbench_duts(&mut self, name: &str, env: &HashMap<String, i64>) {
        let impls: Vec<&ast::ImplDecl> = self.impls.get(name).cloned().unwrap_or_default();
        for im in &impls {
            for item in &im.items {
                if let ast::ImplItem::Let(l) = item {
                    if let Some(ast::Expr::Construct { ty: Some(cty), .. }) = &l.value {
                        if let Some(sub) = type_head_name(cty) {
                            let sub_path = format!("{name}.{}", l.name.text);
                            let mut sub_env = self.consts.clone();
                            sub_env.extend(self.construct_params(cty, env));
                            self.lower_body(sub, &sub_path, &sub_env);
                        }
                    }
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
        let saved_env = std::mem::replace(&mut self.cur_env, env.clone());

        // Ports (struct/array-typed ones flatten to leaves), then the port map.
        for p in &edecl.ports {
            self.add_typed_signal(path, &p.name.text, &p.ty, env);
        }
        let ports: HashMap<String, (SignalId, Option<ast::Direction>)> = edecl
            .ports
            .iter()
            .filter_map(|p| self.locals.get(&p.name.text).map(|&id| (p.name.text.clone(), (id, p.dir))))
            .collect();

        // `let` items: instance bindings are collected for recursion; the rest
        // become state signals.
        let impls: Vec<&ast::ImplDecl> = self.impls.get(ename).cloned().unwrap_or_default();
        let mut subinsts: Vec<(String, ast::Type, Vec<ast::ConnectArg>)> = Vec::new();
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
                    }
                    if let Some(ty) = &l.ty {
                        self.add_typed_signal(path, &l.name.text, ty, env);
                    } else {
                        self.add_signal(path, &l.name.text, 0);
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
            let sub_ports = self.lower_body(sub_ename, &sub_path, &sub_env);
            for c in conns {
                let Some(&(child_id, dir)) = sub_ports.get(&c.field.text) else { continue };
                // `.p` shorthand means the parent signal `p`.
                let value = c.value.clone().unwrap_or_else(|| {
                    ast::Expr::Path(ast::Path { segments: vec![c.field.clone()], span: c.field.span })
                });
                if dir == Some(ast::Direction::Out) {
                    if let Some(target) = self.target_signal(&value) {
                        self.out.drivers.push(Driver { target, cond: None, expr: Expr::Current(child_id) });
                    }
                } else {
                    let expr = self.lower_expr(&value);
                    self.out.drivers.push(Driver { target: child_id, cond: None, expr });
                }
            }
        }

        // Behaviour: each bare statement is a driver or an event block.
        for im in &impls {
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
        self.cur_env = saved_env;
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

    fn add_signal(&mut self, entity: &str, name: &str, width: u32) {
        let id = SignalId(self.out.signals.len() as u32);
        self.out.signals.push(Signal { path: format!("{entity}.{name}"), width, real: false, char: false });
        self.locals.insert(name.to_string(), id);
    }

    /// Add a signal for `name: ty`, flattening composites into scalar leaves: a
    /// struct into one signal per field (`s.valid`), an array into one per
    /// element (`a[0]`). Nested composites recurse. An integer vector
    /// (`uint[8]`) stays a single scalar signal.
    fn add_typed_signal(&mut self, entity: &str, name: &str, ty: &ast::Type, env: &HashMap<String, i64>) {
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
        if let Some(fields) = self.struct_fields(ty) {
            if let ast::Type::Path(p) = ty {
                self.local_struct.insert(name.to_string(), p.segments[0].text.clone());
            }
            for (fname, fty) in fields {
                self.add_typed_signal(entity, &format!("{name}.{fname}"), &fty, env);
            }
        } else if let Some((elem, indices)) = array_of(ty, env, &self.const_ranges) {
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
        } else if let Some((w, is_real)) = self.ranged_numeric(ty) {
            // `integer<lo..hi>` stores in the smallest width covering the
            // range (two's complement when lo < 0); `real<..>` stays f64.
            self.add_signal(entity, name, w);
            if is_real {
                if let Some(&id) = self.locals.get(name) {
                    self.out.signals[id.0 as usize].real = true;
                }
            }
        } else {
            self.add_signal(entity, name, type_width(ty, env));
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
    fn ranged_numeric(&self, ty: &ast::Type) -> Option<(u32, bool)> {
        let ast::Type::Generic { base, args, .. } = ty else { return None };
        let ast::Type::Path(p) = base.as_ref() else { return None };
        let kind = p.segments.last().map(|s| s.text.as_str())?;
        if kind != "integer" && kind != "real" {
            return None;
        }
        let [ast::GenericArg::Positional(arg)] = args.as_slice() else { return None };
        if kind == "real" {
            return Some((64, true)); // range is a constraint, storage is f64
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
                return Some((w, false));
            }
        }
        Some((64, false))
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

    /// The `(field name, field type)` list if `ty` names a known struct.
    fn struct_fields(&self, ty: &ast::Type) -> Option<Vec<(String, ast::Type)>> {
        if let ast::Type::Path(p) = ty {
            if p.segments.len() == 1 {
                if let Some(s) = self.structs.get(&p.segments[0].text) {
                    return Some(s.fields.iter().map(|f| (f.name.text.clone(), f.ty.clone())).collect());
                }
            }
        }
        None
    }

    /// Lower a top-level (combinational-context) statement. `cond` accumulates
    /// the enclosing combinational conditions.
    fn lower_stmt(&mut self, stmt: &ast::Stmt, cond: Option<Expr>) {
        match stmt {
            ast::Stmt::Assign { target, value, after, .. } => {
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
                                    });
                                }
                            }
                        }
                        return;
                    }
                }
                if let Some(target) = self.target_signal(target) {
                    let expr = self.coerce_to_target(target, self.lower_expr(value));
                    self.out.drivers.push(Driver { target, cond, expr });
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
            // Other statement forms (for, let, expr, return) are not lowered yet.
            _ => {}
        }
    }

    /// The condition under which a match arm fires: `scrut == <variant value>`
    /// for an enum path, or always (`None`) for a wildcard.
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
            // Wildcard and (for now) bit patterns match anything.
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
    fn target_signal(&self, target: &ast::Expr) -> Option<SignalId> {
        expr_path(target).and_then(|p| self.locals.get(&p).copied())
    }

    fn lower_expr(&self, e: &ast::Expr) -> Expr {
        match e {
            // `if c { a } else { b }` is a mux: lower to a select.
            ast::Expr::IfExpr { cond, then, els, .. } => Expr::Select {
                cond: Box::new(self.lower_expr(cond)),
                then: Box::new(self.lower_expr(then)),
                els: Box::new(self.lower_expr(els)),
            },
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
            ast::Expr::Path(p) if p.segments.len() == 1 => self
                .locals
                .get(&p.segments[0].text)
                .map(|id| Expr::Current(*id))
                .unwrap_or(Expr::Unknown),
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
            // resolves to its flattened signal.
            ast::Expr::Field { .. } | ast::Expr::Index { .. } => expr_path(e)
                .and_then(|p| self.locals.get(&p).copied())
                .map(Expr::Current)
                .unwrap_or(Expr::Unknown),
            ast::Expr::SysAttr { base, attr, .. } => self.lower_sysattr(base, &attr.text),
            ast::Expr::Unary { op, rhs, .. } => {
                // `not` on an enum-typed operand inlines its impl (`impl
                // "not" for Logic`), like binary operators.
                if *op == ast::UnOp::Not {
                    if let Some(Val::Scalar(v)) = self.inline_unary("not", rhs) {
                        return v;
                    }
                }
                Expr::Unary { op: lower_unop(*op), rhs: Box::new(self.lower_expr(rhs)) }
            }
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                // An operator on an enum/struct-typed operand inlines its
                // operator-trait impl body (spec 3.25); `==`/`!=` stay
                // built-in discriminant comparison unless `<=>` derives them.
                let op_str = siox_syntax::pretty::bin_op(*op);
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
                self.make_binary(*op, l, r)
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
    fn ast_width(&self, e: &ast::Expr) -> u32 {
        match e {
            ast::Expr::IfExpr { then, .. } => self.ast_width(then),
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

        // Overload selection: the impl's declared rhs (`impl Add<integer>`)
        // wins; otherwise the fn's rhs parameter type. `Self` (or no trait
        // arg) reads as the impl target; an unknown rhs accepts a sole
        // candidate.
        let (f, _) = fns
            .iter()
            .find(|(f, rhs_arg)| {
                let declared = rhs_arg.as_deref().or_else(|| {
                    f.params
                        .iter()
                        .find(|p| !p.is_self)
                        .and_then(|p| p.ty.as_ref())
                        .and_then(type_head_name)
                });
                match (declared, &rhs_ty) {
                    (Some("Self"), Some(r)) => *r == lhs_ty,
                    (Some(dt), Some(r)) => dt == r,
                    _ => fns.len() == 1,
                }
            })
            .or_else(|| if fns.len() == 1 { fns.first() } else { None })?;
        let body = f.body.as_ref()?;

        // Bind `self` to the left operand and the first named param to the right.
        let mut fenv: HashMap<String, Val> = HashMap::new();
        fenv.insert("self".to_string(), self.lower_val_env(lhs, env));
        if let Some(p) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &p.name {
                fenv.insert(n.text.clone(), self.lower_val_env(rhs, env));
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
                other => lower_binop(other),
            };
            return Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Expr::Binary { op: lower_binop(op), lhs: Box::new(lhs), rhs: Box::new(rhs) }
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
        self.inline_block(&body.stmts, &env)
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
            ast::Expr::Path(p) if p.segments.len() >= 2 => self
                .enum_variants
                .contains_key(&p.segments[0].text)
                .then(|| p.segments[0].text.clone()),
            _ => {
                let p = expr_path(e)?;
                if self.local_char.contains(&p) {
                    return Some("Char".to_string());
                }
                self.local_enum.get(&p).or_else(|| self.local_struct.get(&p)).cloned()
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
            ast::Expr::IfExpr { cond, then, els, .. } => {
                let c = self.lower_scalar_env(cond, env);
                select_val(c, self.lower_val_env(then, env), self.lower_val_env(els, env))
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
            // `.re` shorthand means `.re = re`.
            ast::Expr::Construct { args, .. } => Val::Fields(
                args.iter()
                    .map(|a| {
                        let v = match &a.value {
                            Some(v) => self.lower_scalar_env(v, env),
                            None => match env.get(&a.field.text) {
                                Some(Val::Scalar(e)) => e.clone(),
                                _ => self
                                    .locals
                                    .get(&a.field.text)
                                    .map(|&id| Expr::Current(id))
                                    .unwrap_or(Expr::Unknown),
                            },
                        };
                        (a.field.text.clone(), v)
                    })
                    .collect(),
            ),
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                let op_str = siox_syntax::pretty::bin_op(*op);
                if !matches!(op_str, "==" | "!=") {
                    if let Some(v) = self.inline_op(op_str, lhs, rhs, env) {
                        return v;
                    }
                }
                if let Some(derived) = self.inline_cmp(op_str, lhs, rhs, env) {
                    return Val::Scalar(derived);
                }
                let (l, r) = (self.lower_scalar_env(lhs, env), self.lower_scalar_env(rhs, env));
                Val::Scalar(self.make_binary(*op, l, r))
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
        BinOp::Nand => "nand",
        BinOp::Or => "or",
        BinOp::Nor => "nor",
        BinOp::Xor => "xor",
        BinOp::Xnor => "xnor",
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

fn lower_binop(op: AstBinOp) -> BinOp {
    match op {
        AstBinOp::Add => BinOp::Add,
        AstBinOp::Sub => BinOp::Sub,
        AstBinOp::Mul => BinOp::Mul,
        AstBinOp::Div => BinOp::Div,
        AstBinOp::And => BinOp::And,
        AstBinOp::Nand => BinOp::Nand,
        AstBinOp::Or => BinOp::Or,
        AstBinOp::Nor => BinOp::Nor,
        AstBinOp::Xor => BinOp::Xor,
        AstBinOp::Xnor => BinOp::Xnor,
        AstBinOp::Shl => BinOp::Shl,
        AstBinOp::Shr => BinOp::Shr,
        AstBinOp::Eq => BinOp::Eq,
        AstBinOp::Ne => BinOp::Ne,
        AstBinOp::Lt => BinOp::Lt,
        AstBinOp::Le => BinOp::Le,
        AstBinOp::Gt => BinOp::Gt,
        AstBinOp::Ge => BinOp::Ge,
    }
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
fn type_width(t: &ast::Type, env: &HashMap<String, i64>) -> u32 {
    match t {
        ast::Type::Path(p) => match p.segments.last().map(|s| s.text.as_str()) {
            Some("Bit") | Some("Logic") | Some("Clock") | Some("Bool") => 1,
            Some("real") => 64, // f64 bits
            Some("Char") => 32, // symbol storage (implementation detail)
            _ => 0,
        },
        // For `uint[8]` the index is the width; for `Logic[31..0]` it is the
        // span; unconstrained `T[]` stays width 0 ("set at use").
        ast::Type::Indexed { index: None, .. } => 0,
        ast::Type::Indexed { index: Some(index), .. } => match index.as_ref() {
            ast::Expr::Range { lo, hi, .. } => match (eval_const(lo, env), eval_const(hi, env)) {
                (Some(a), Some(b)) => (a - b).unsigned_abs() as u32 + 1,
                _ => 0,
            },
            e => eval_const(e, env).map(|v| v.max(0) as u32).unwrap_or(0),
        },
        ast::Type::Generic { base, .. } | ast::Type::Mode { inner: base, .. } => {
            type_width(base, env)
        }
    }
}

/// Const-evaluate a width expression against a parameter environment.
fn eval_const(e: &ast::Expr, env: &HashMap<String, i64>) -> Option<i64> {
    match e {
        ast::Expr::Int { text, .. } => parse_int(text).map(|v| v as i64),
        ast::Expr::Path(p) if p.segments.len() == 1 => env.get(&p.segments[0].text).copied(),
        ast::Expr::Unary { op, rhs, .. } => {
            let v = eval_const(rhs, env)?;
            Some(match op {
                ast::UnOp::Neg => -v,
                ast::UnOp::Not => !v,
            })
        }
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            let (a, b) = (eval_const(lhs, env)?, eval_const(rhs, env)?);
            Some(match op {
                ast::BinOp::Add => a + b,
                ast::BinOp::Sub => a - b,
                ast::BinOp::Mul => a * b,
                ast::BinOp::Div if b != 0 => a / b,
                ast::BinOp::Shl => a << b,
                ast::BinOp::Shr => a >> b,
                _ => return None,
            })
        }
        _ => None,
    }
}

/// Build `enum name -> variant name -> discriminant`. Explicit `= n` values are
/// honoured; unspecified variants continue from the previous discriminant + 1.
fn enum_discriminants(modules: &[Module]) -> HashMap<String, HashMap<String, u64>> {
    let mut out = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Enum(e) = item {
                let mut vars = HashMap::new();
                let mut next = 0u64;
                for v in &e.variants {
                    let disc = match &v.value {
                        Some(ast::Expr::Int { text, .. }) => parse_int(text).unwrap_or(next),
                        _ => next,
                    };
                    vars.insert(v.name.text.clone(), disc);
                    next = disc + 1;
                }
                out.insert(e.name.text.clone(), vars);
            }
        }
    }
    out
}

/// The dotted signal path of a name, struct-field, or constant-index access:
/// `s` -> `"s"`, `s.data` -> `"s.data"`, `a[2]` -> `"a[2]"`. A dynamic index or
/// anything else (calls, slices) yields `None`.
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
) -> Option<(&'t ast::Type, Vec<i64>)> {
    let ast::Type::Indexed { base, index: Some(index), .. } = ty else { return None };
    if is_int_type(base) {
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

fn is_int_type(ty: &ast::Type) -> bool {
    matches!(ty, ast::Type::Path(p)
        if matches!(p.segments.last().map(|s| s.text.as_str()), Some("uint" | "int" | "integer")))
}

/// Build `enum name -> bit width`: the `repr` width if given (`enum S: uint[2]`),
/// else the bits needed for the variant count.
fn enum_reprs(modules: &[Module]) -> HashMap<String, u32> {
    let empty = HashMap::new();
    let mut out = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Enum(e) = item {
                let w = match &e.repr {
                    Some(t) => type_width(t, &empty),
                    None => {
                        let n = e.variants.len().max(1) as u32;
                        if n <= 1 { 1 } else { u32::BITS - (n - 1).leading_zeros() }
                    }
                };
                out.insert(e.name.text.clone(), w);
            }
        }
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
        let mut sink = DiagnosticSink::new();
        let module = siox_syntax::parse_module(FileId(0), src, &mut sink);
        assert_eq!(sink.error_count(), 0, "parse errors:\n{src}");
        let modules = std::slice::from_ref(&module);
        let resolved = siox_resolve::resolve(modules, &mut sink);
        let typed = siox_types::check(modules, &resolved, &mut sink);
        let hier = siox_elab::elaborate(modules, &typed, &mut sink);
        lower(modules, &hier, &mut sink)
    }

    const COUNTER: &str = "module m;\n\
        entity Counter<W: integer> {\n\
          in clk: Clock;\n\
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
          let clk: Logic = '0';\n\
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

        let sig = |w: u32| Signal { path: "s".into(), width: w, real: false, char: false };
        // Out-of-range signal id, an Unknown, a bad slice, and a width-0 signal.
        let bad = Design {
            signals: vec![sig(0)], // width 0 -> flagged
            drivers: vec![Driver {
                target: SignalId(9), // out of range
                cond: Some(Expr::Unknown),
                expr: Expr::Slice { base: Box::new(Expr::Current(SignalId(0))), hi: 1, lo: 3 },
            }],
            event_blocks: vec![],
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
