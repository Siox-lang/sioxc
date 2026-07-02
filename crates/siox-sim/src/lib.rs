//! Event-driven simulator core for siox Phase 1 (spec Stage 7) plus the test
//! runner / assertions (spec Stage 8).
//!
//! Each signal has a current value, an `old` value (the value before this
//! settle), and an `event` flag (changed during this settle). `settle` runs the
//! delta cycle:
//! 1. mark `::event` for signals the stimulus changed (`current != old`)
//! 2. evaluate combinational drivers to a fixpoint (committing changes, marking
//!    events as values move)
//! 3. fire event-controlled blocks whose condition is true, computing
//!    next-state updates from the pre-commit values (spec 3.13)
//! 4. commit the next-state updates, then re-settle combinational logic
//! 5. roll `old <- current` and clear events, ready for the next step
//!
//! Event blocks fire once per `settle` (one clock edge per step), which covers
//! the Phase-1 single-clock designs; cascaded event domains are a follow-up.

use std::collections::HashMap;

use siox_diag::Span;
use siox_elab::Hierarchy;
use siox_ir::{BinOp, Design, Expr, SignalId, UnOp};
use siox_syntax::ast;
use siox_syntax::Module;

/// A signal storage slot — the backend value representation. `u64` is the
/// fast default; `u128` carries signals wider than 64 bits and is
/// register-pair native on 64-bit targets (no software emulation). Floats
/// stay f64 bits in the low 64 either way: no mainstream CPU has scalar
/// f128/f256 hardware (AVX widths are SIMD lanes, not precision).
pub trait Slot: Copy + PartialEq + PartialOrd + core::fmt::Debug + Default {
    const BITS: u32;
    fn from_u64(v: u64) -> Self;
    fn to_u64(self) -> u64;
    fn to_u128(self) -> u128;
    fn wrapping_add(self, o: Self) -> Self;
    fn wrapping_sub(self, o: Self) -> Self;
    fn wrapping_mul(self, o: Self) -> Self;
    fn checked_div(self, o: Self) -> Option<Self>;
    fn wrapping_shl(self, n: u32) -> Self;
    fn wrapping_shr(self, n: u32) -> Self;
    fn bitand(self, o: Self) -> Self;
    fn is_zero(self) -> bool;
    fn one() -> Self;
    fn wrapping_neg(self) -> Self;
}

macro_rules! impl_slot {
    ($t:ty, $bits:expr) => {
        impl Slot for $t {
            const BITS: u32 = $bits;
            fn from_u64(v: u64) -> Self {
                v as $t
            }
            fn to_u64(self) -> u64 {
                self as u64
            }
            fn to_u128(self) -> u128 {
                self as u128
            }
            fn wrapping_add(self, o: Self) -> Self {
                <$t>::wrapping_add(self, o)
            }
            fn wrapping_sub(self, o: Self) -> Self {
                <$t>::wrapping_sub(self, o)
            }
            fn wrapping_mul(self, o: Self) -> Self {
                <$t>::wrapping_mul(self, o)
            }
            fn checked_div(self, o: Self) -> Option<Self> {
                <$t>::checked_div(self, o)
            }
            fn wrapping_shl(self, n: u32) -> Self {
                <$t>::wrapping_shl(self, n)
            }
            fn wrapping_shr(self, n: u32) -> Self {
                <$t>::wrapping_shr(self, n)
            }
            fn bitand(self, o: Self) -> Self {
                self & o
            }
            fn is_zero(self) -> bool {
                self == 0
            }
            fn one() -> Self {
                1
            }
            fn wrapping_neg(self) -> Self {
                <$t>::wrapping_neg(self)
            }
        }
    };
}

impl_slot!(u64, 64);
impl_slot!(u128, 128);

/// Which slot width a run uses. `Auto` picks `u128` only when the design
/// declares signals wider than 64 bits — precision costs speed, so the fast
/// slot stays the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotWidth {
    Auto,
    W64,
    W128,
}

/// Whether any signal in the design outgrows 64-bit slots.
pub fn needs_wide(design: &Design) -> bool {
    design.signals.iter().any(|s| s.width > 64)
}

/// Per-signal runtime state: current value, previous value, and event flag.
#[derive(Clone, Copy, Debug, Default)]
pub struct SignalState<S: Slot = u64> {
    pub current: S,
    pub old: S,
    pub event: bool,
}

/// Simulation kernel, generic over the backend slot width.
pub struct Simulator<'a, S: Slot = u64> {
    design: &'a Design,
    state: Vec<SignalState<S>>,
    /// Simulation time in femtoseconds.
    time_fs: u64,
}

/// A combinational fixpoint that fails to converge after this many iterations is
/// treated as stable (oscillation guard).
const MAX_DELTAS: usize = 10_000;

impl<'a, S: Slot> Simulator<'a, S> {
    pub fn new(design: &'a Design) -> Self {
        let state = vec![SignalState::default(); design.signals.len()];
        Simulator { design, state, time_fs: 0 }
    }

    /// The id of a signal by its hierarchical path, e.g. `Counter.count`.
    pub fn signal(&self, path: &str) -> Option<SignalId> {
        self.design.signals.iter().position(|s| s.path == path).map(|i| SignalId(i as u32))
    }

    /// Drive a signal (stimulus). Call `settle` afterwards to propagate.
    /// The `u64` API covers stimulus values; wider slots widen internally.
    pub fn set(&mut self, sig: SignalId, value: u64) {
        self.set_slot(sig, S::from_u64(value));
    }

    fn set_slot(&mut self, sig: SignalId, value: S) {
        let i = sig.0 as usize;
        self.state[i].current = mask(value, self.design.signals[i].width);
    }

    /// Read a signal's current value (low 64 bits; see [`Self::read_wide`]).
    pub fn read(&self, sig: SignalId) -> u64 {
        self.state[sig.0 as usize].current.to_u64()
    }

    /// Read a signal's full value, whatever the slot width.
    pub fn read_wide(&self, sig: SignalId) -> u128 {
        self.state[sig.0 as usize].current.to_u128()
    }

    pub fn time_fs(&self) -> u64 {
        self.time_fs
    }

    /// Advance simulation time and settle.
    pub fn advance(&mut self, fs: u64) {
        self.time_fs += fs;
        self.settle();
    }

    /// Run delta cycles until the design is stable.
    pub fn settle(&mut self) {
        // 1. events from stimulus / the previous commit.
        for s in &mut self.state {
            s.event = s.current != s.old;
        }

        // 2. combinational fixpoint.
        self.settle_combinational();

        // 3. fire event-controlled blocks once, computing next-state from the
        //    pre-commit values.
        let mut next: Vec<(usize, S)> = Vec::new();
        for eb in &self.design.event_blocks {
            if !self.eval(&eb.condition).is_zero() {
                for u in &eb.updates {
                    if self.cond_true(&u.cond) {
                        next.push((u.target.0 as usize, self.eval(&u.expr)));
                    }
                }
            }
        }

        // 4. commit next-state, then re-settle combinational logic.
        let mut committed = false;
        for (i, v) in next {
            let v = mask(v, self.design.signals[i].width);
            if self.state[i].current != v {
                self.state[i].current = v;
                self.state[i].event = true;
                committed = true;
            }
        }
        if committed {
            self.settle_combinational();
        }

        // 5. roll old <- current, clear events.
        for s in &mut self.state {
            s.old = s.current;
            s.event = false;
        }
    }

    /// Evaluate combinational drivers until no signal changes (spec 3.14 source
    /// order: later drivers override earlier within a pass).
    fn settle_combinational(&mut self) {
        for _ in 0..MAX_DELTAS {
            let mut next: Vec<S> = self.state.iter().map(|s| s.current).collect();
            for d in &self.design.drivers {
                if self.cond_true(&d.cond) {
                    next[d.target.0 as usize] = self.eval(&d.expr);
                }
            }
            let mut changed = false;
            for (i, &v) in next.iter().enumerate() {
                let v = mask(v, self.design.signals[i].width);
                if self.state[i].current != v {
                    self.state[i].current = v;
                    self.state[i].event = true;
                    changed = true;
                }
            }
            if !changed {
                return;
            }
        }
    }

    fn cond_true(&self, cond: &Option<Expr>) -> bool {
        match cond {
            None => true,
            Some(e) => !self.eval(e).is_zero(),
        }
    }

    /// Evaluate an IR expression against the current state.
    fn eval(&self, e: &Expr) -> S {
        match e {
            Expr::Const(v) => S::from_u64(*v),
            Expr::Real(x) => S::from_u64(x.to_bits()),
            Expr::Logic(c) => S::from_u64(logic_value(*c)),
            Expr::Current(id) => self.state[id.0 as usize].current,
            Expr::Old(id) => self.state[id.0 as usize].old,
            Expr::Event(id) => S::from_u64(self.state[id.0 as usize].event as u64),
            Expr::Unary { op, rhs } => {
                let a = self.eval(rhs);
                match op {
                    UnOp::Not => S::from_u64(a.is_zero() as u64),
                    UnOp::Neg => a.wrapping_neg(),
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let a = self.eval(lhs);
                let b = self.eval(rhs);
                apply_binop(*op, a, b)
            }
            // `base[hi..lo]`: shift out the low bits, keep `hi-lo+1` of them.
            Expr::Slice { base, hi, lo } => {
                mask(self.eval(base).wrapping_shr(*lo), hi - lo + 1)
            }
            Expr::Select { cond, then, els } => {
                if !self.eval(cond).is_zero() {
                    self.eval(then)
                } else {
                    self.eval(els)
                }
            }
            Expr::Unknown => S::from_u64(0),
        }
    }
}

/// Truncate a value to a signal's bit width (arithmetic wraps at `2^width`).
/// Width `0` (unknown) or `>= slot bits` leaves the value unchanged.
fn mask<S: Slot>(value: S, width: u32) -> S {
    if width == 0 || width >= S::BITS {
        value
    } else {
        value.bitand(S::one().wrapping_shl(width).wrapping_sub(S::one()))
    }
}

/// Logic literal encoding, aligned with std::logic's `enum Logic { '0', '1',
/// 'Z', 'X' }` declaration order so literal comparisons match enum-typed
/// signals. In a 1-bit (intrinsic two-value) signal, 'Z'/'X' are simply never
/// equal — unknown states need the 2-bit enum representation.
fn logic_value(c: char) -> u64 {
    match c {
        '1' | 'H' => 1,
        'Z' => 2,
        'X' | 'U' | 'W' => 3,
        _ => 0,
    }
}

/// `and`/`or`/... are evaluated as logical (boolean) operators in Phase 1, which
/// is correct for conditions; bitwise-on-vectors is a later, width-aware concern.
fn apply_binop<S: Slot>(op: BinOp, a: S, b: S) -> S {
    let (la, lb) = (!a.is_zero(), !b.is_zero());
    let bool_s = |v: bool| S::from_u64(v as u64);
    match op {
        BinOp::Add => a.wrapping_add(b),
        BinOp::Sub => a.wrapping_sub(b),
        BinOp::Mul => a.wrapping_mul(b),
        BinOp::Div => a.checked_div(b).unwrap_or_else(|| S::from_u64(0)),
        BinOp::Shl => a.wrapping_shl(b.to_u64() as u32),
        BinOp::Shr => a.wrapping_shr(b.to_u64() as u32),
        BinOp::And => bool_s(la && lb),
        BinOp::Nand => bool_s(!(la && lb)),
        BinOp::Or => bool_s(la || lb),
        BinOp::Nor => bool_s(!(la || lb)),
        BinOp::Xor => bool_s(la ^ lb),
        BinOp::Xnor => bool_s(!(la ^ lb)),
        BinOp::Eq => bool_s(a == b),
        BinOp::Ne => bool_s(a != b),
        // Float arithmetic on f64-bit values (`real` operands, low 64 bits).
        BinOp::FAdd => S::from_u64((f64::from_bits(a.to_u64()) + f64::from_bits(b.to_u64())).to_bits()),
        BinOp::FSub => S::from_u64((f64::from_bits(a.to_u64()) - f64::from_bits(b.to_u64())).to_bits()),
        BinOp::FMul => S::from_u64((f64::from_bits(a.to_u64()) * f64::from_bits(b.to_u64())).to_bits()),
        BinOp::FDiv => S::from_u64((f64::from_bits(a.to_u64()) / f64::from_bits(b.to_u64())).to_bits()),
        BinOp::Lt => bool_s(a < b),
        BinOp::Le => bool_s(a <= b),
        BinOp::Gt => bool_s(a > b),
        BinOp::Ge => bool_s(a >= b),
    }
}

/// Result of running a `#[test]` entity (spec Stage 8).
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    /// Failure message when an assertion fails.
    pub failure: Option<String>,
    /// Span of the failing assertion, for `file:line:col` rendering.
    pub span: Option<Span>,
}

/// A snapshot of every signal's value at one simulation time, recorded during a
/// traced run for waveform export (spec Stage 9).
pub struct Sample {
    pub time_fs: u64,
    /// One value per signal, widened to u128 so any slot width fits.
    pub values: Vec<u128>,
}

/// Half a clock period, in femtoseconds — the time `tick` advances per edge.
const HALF_PERIOD: u64 = 5_000_000; // 5 ns

/// Discover and run every `#[test]` entity, driving its stimulus through the
/// simulator and evaluating its assertions (spec Stage 8).
///
/// Phase-1 scope: a test entity instantiates one or more DUTs and drives them
/// via `tick`/`wait`/assignments; its signals are aliased to the DUTs' signals
/// through the elaborated connections. The interpreted stimulus statements are
/// `let` initial values, assignments, `tick(clk)`, `wait`, `for` over a static
/// range, `if`, and `assert!(cond, "msg")`.
/// `filter`, when given, runs only the `#[test]` entities whose name contains it.
pub fn run_tests(
    modules: &[Module],
    hier: &Hierarchy,
    design: &Design,
    filter: Option<&str>,
) -> Vec<TestResult> {
    run_tests_with(modules, hier, design, filter, SlotWidth::Auto)
}

/// [`run_tests`] with an explicit backend slot width. `Auto` selects 128-bit
/// slots only when the design has signals wider than 64 bits.
pub fn run_tests_with(
    modules: &[Module],
    hier: &Hierarchy,
    design: &Design,
    filter: Option<&str>,
    slot: SlotWidth,
) -> Vec<TestResult> {
    if wide_run(slot, design) {
        run_tests_impl::<u128>(modules, hier, design, filter)
    } else {
        run_tests_impl::<u64>(modules, hier, design, filter)
    }
}

fn wide_run(slot: SlotWidth, design: &Design) -> bool {
    match slot {
        SlotWidth::W64 => false,
        SlotWidth::W128 => true,
        SlotWidth::Auto => needs_wide(design),
    }
}

fn run_tests_impl<S: Slot>(
    modules: &[Module],
    hier: &Hierarchy,
    design: &Design,
    filter: Option<&str>,
) -> Vec<TestResult> {
    let (entities, impls) = collect_defs(modules);
    let enums = enum_discriminants(modules);
    let mut results = Vec::new();
    for &root in &hier.roots {
        let inst = hier.instance(root);
        let is_test = entities.get(inst.entity.as_str()).is_some_and(|e| has_attr(e, "test"));
        let selected = filter.map_or(true, |f| inst.entity.contains(f));
        if is_test && selected {
            let body = impls.get(inst.entity.as_str()).cloned().unwrap_or_default();
            results.push(run_one::<S>(&inst.entity, root, hier, design, &body, &enums, false).0);
        }
    }
    results
}

/// Run the first `#[test]` entity (optionally name-filtered), recording a signal
/// sample at every simulation step for waveform export (spec Stage 9).
pub fn run_test_traced(
    modules: &[Module],
    hier: &Hierarchy,
    design: &Design,
    filter: Option<&str>,
) -> Option<(TestResult, Vec<Sample>)> {
    if needs_wide(design) {
        return run_test_traced_impl::<u128>(modules, hier, design, filter);
    }
    run_test_traced_impl::<u64>(modules, hier, design, filter)
}

fn run_test_traced_impl<S: Slot>(
    modules: &[Module],
    hier: &Hierarchy,
    design: &Design,
    filter: Option<&str>,
) -> Option<(TestResult, Vec<Sample>)> {
    let (entities, impls) = collect_defs(modules);
    let enums = enum_discriminants(modules);
    for &root in &hier.roots {
        let inst = hier.instance(root);
        let is_test = entities.get(inst.entity.as_str()).is_some_and(|e| has_attr(e, "test"));
        let selected = filter.map_or(true, |f| inst.entity.contains(f));
        if is_test && selected {
            let body = impls.get(inst.entity.as_str()).cloned().unwrap_or_default();
            return Some(run_one::<S>(&inst.entity, root, hier, design, &body, &enums, true));
        }
    }
    None
}

/// Build `enum name -> variant name -> discriminant` from the parsed modules.
fn enum_discriminants(modules: &[Module]) -> HashMap<String, HashMap<String, u64>> {
    let mut out = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Enum(e) = item {
                let mut vars = HashMap::new();
                let mut next = 0u64;
                for v in &e.variants {
                    let disc = match &v.value {
                        Some(ast::Expr::Int { text, .. }) => parse_u64(text),
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

type Defs<'a> = (HashMap<&'a str, &'a ast::EntityDecl>, HashMap<&'a str, Vec<&'a ast::ImplDecl>>);

fn collect_defs(modules: &[Module]) -> Defs<'_> {
    let mut entities = HashMap::new();
    let mut impls: HashMap<&str, Vec<&ast::ImplDecl>> = HashMap::new();
    for m in modules {
        for item in &m.items {
            match item {
                ast::Item::Entity(e) => {
                    entities.insert(e.name.text.as_str(), e);
                }
                ast::Item::Impl(im) if im.trait_.is_none() => {
                    if let Some(n) = type_head_name(&im.target) {
                        impls.entry(n).or_default().push(im);
                    }
                }
                _ => {}
            }
        }
    }
    (entities, impls)
}

#[allow(clippy::too_many_arguments)]
fn run_one<S: Slot>(
    name: &str,
    root: siox_elab::InstanceId,
    hier: &Hierarchy,
    design: &Design,
    body: &[&ast::ImplDecl],
    enums: &HashMap<String, HashMap<String, u64>>,
    record: bool,
) -> (TestResult, Vec<Sample>) {
    // Map this test's local signal names to design signals via the connections
    // of the DUTs it instantiates (`.clk = clk` aliases `clk` to `DUT.clk`). A
    // struct port flattens to per-field entries (`.p = p` -> `p.valid`, ...).
    let mut map: HashMap<String, SignalId> = HashMap::new();
    for &child_id in &hier.instance(root).children {
        let child = hier.instance(child_id);
        for c in &child.connections {
            let prefix = format!("{}.{}", child.entity, c.port);
            for (i, sig) in design.signals.iter().enumerate() {
                let id = SignalId(i as u32);
                if sig.path == prefix {
                    map.insert(c.signal.clone(), id);
                } else if let Some(suffix) = sig.path.strip_prefix(&prefix) {
                    // A struct field (`.valid`) or array element (`[0]`) leaf.
                    if suffix.starts_with('.') || suffix.starts_with('[') {
                        map.insert(format!("{}{suffix}", c.signal), id);
                    }
                }
            }
        }
    }

    let mut tb = Testbench::<S> {
        sim: Simulator::new(design),
        map,
        enums,
        failure: None,
        record,
        samples: Vec::new(),
    };

    // Apply initial `let` values, then settle and record the starting state.
    for im in body {
        for item in &im.items {
            if let ast::ImplItem::Let(l) = item {
                match &l.value {
                    // A named construct is an instance; elaboration handled it.
                    Some(ast::Expr::Construct { ty: Some(_), .. }) => {}
                    // A name-less struct literal initialises each field signal.
                    Some(ast::Expr::Construct { args, .. }) => {
                        for c in args {
                            if let Some(v) = &c.value {
                                let field = format!("{}.{}", l.name.text, c.field.text);
                                let val = tb.eval_for(&field, v);
                                tb.set_name(&field, val);
                            }
                        }
                    }
                    Some(value) => {
                        let v = tb.eval_for(&l.name.text, value);
                        tb.set_name(&l.name.text, v);
                    }
                    None => {}
                }
            }
        }
    }
    tb.sim.settle();
    tb.sample();

    // Run the stimulus.
    for im in body {
        for item in &im.items {
            if let ast::ImplItem::Stmt(s) = item {
                tb.exec(s);
                if tb.failure.is_some() {
                    break;
                }
            }
        }
        if tb.failure.is_some() {
            break;
        }
    }

    let result = match tb.failure {
        Some((msg, span)) => {
            TestResult { name: name.to_string(), passed: false, failure: Some(msg), span: Some(span) }
        }
        None => TestResult { name: name.to_string(), passed: true, failure: None, span: None },
    };
    (result, tb.samples)
}

/// Interprets a testbench's stimulus statements against a running simulator.
struct Testbench<'a, S: Slot = u64> {
    sim: Simulator<'a, S>,
    /// Test-local signal name -> design signal id.
    map: HashMap<String, SignalId>,
    /// Enum name -> variant -> discriminant, for evaluating `Enum::Variant`.
    enums: &'a HashMap<String, HashMap<String, u64>>,
    failure: Option<(String, Span)>,
    /// When set, a sample is recorded after each simulation step.
    record: bool,
    samples: Vec<Sample>,
}

impl<S: Slot> Testbench<'_, S> {
    fn set_name(&mut self, name: &str, value: S) {
        if let Some(&id) = self.map.get(name) {
            self.sim.set_slot(id, value);
        }
    }

    /// Evaluate a stimulus value for a named target: `real` targets take the
    /// value's f64 bits (`a.re = 3` stores 3.0).
    fn eval_for(&self, name: &str, e: &ast::Expr) -> S {
        let real = self
            .map
            .get(name)
            .map(|&id| self.sim.design.signals[id.0 as usize].real)
            .unwrap_or(false);
        if real {
            S::from_u64(self.eval_real(e).to_bits())
        } else {
            self.eval(e)
        }
    }

    /// Record the full signal vector at the current simulation time.
    fn sample(&mut self) {
        if self.record {
            let values = self.sim.state.iter().map(|s| s.current.to_u128()).collect();
            self.samples.push(Sample { time_fs: self.sim.time_fs, values });
        }
    }

    fn exec(&mut self, s: &ast::Stmt) {
        match s {
            ast::Stmt::Assign { target, value, .. } => {
                if let Some(path) = expr_path(target) {
                    // A struct literal assigns each field of a flattened
                    // struct local (`a = { .re = 3, .im = 4 };`).
                    if let ast::Expr::Construct { args, .. } = value {
                        for arg in args {
                            let field = format!("{path}.{}", arg.field.text);
                            let v = arg
                                .value
                                .as_ref()
                                .map(|v| self.eval_for(&field, v))
                                .unwrap_or_else(|| S::from_u64(0));
                            self.set_name(&field, v);
                        }
                    } else {
                        let v = self.eval_for(&path, value);
                        self.set_name(&path, v);
                    }
                }
                self.sim.settle();
                self.sample();
            }
            ast::Stmt::Expr(ast::Expr::Call { callee, args, bang, span }) => {
                self.exec_call(callee, args, *bang, *span);
            }
            ast::Stmt::For { range, body, .. } => {
                for _ in 0..self.range_count(range) {
                    for s in &body.stmts {
                        self.exec(s);
                        if self.failure.is_some() {
                            return;
                        }
                    }
                }
            }
            ast::Stmt::If(iff) => {
                let branch = if !self.eval(&iff.cond).is_zero() {
                    Some(&iff.then.stmts)
                } else {
                    match iff.else_.as_deref() {
                        Some(ast::ElseBranch::Block(b)) => Some(&b.stmts),
                        Some(ast::ElseBranch::If(_)) => None, // else-if: skip for now
                        None => None,
                    }
                };
                if let Some(stmts) = branch {
                    for s in stmts {
                        self.exec(s);
                        if self.failure.is_some() {
                            return;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn exec_call(&mut self, callee: &ast::Expr, args: &[ast::Expr], bang: bool, span: Span) {
        let name = match callee {
            ast::Expr::Path(p) => p.segments.first().map(|s| s.text.as_str()).unwrap_or(""),
            _ => "",
        };
        match name {
            // tick(clk): a full clock cycle — rising edge, half period, falling
            // edge, half period.
            "tick" => {
                if let Some(id) = args.first().and_then(|a| self.signal_of(a)) {
                    self.sim.set(id, 1);
                    self.sim.settle();
                    self.sample();
                    self.sim.advance(HALF_PERIOD);
                    self.sim.set(id, 0);
                    self.sim.settle();
                    self.sample();
                    self.sim.advance(HALF_PERIOD);
                }
            }
            // wait <duration>: advance simulation time.
            "wait" => {
                self.sim.advance(duration_fs(args));
                self.sample();
            }
            // assert!(cond, "msg"): record the first failure.
            "assert" if bang => {
                let ok = args.first().map(|c| !self.eval(c).is_zero()).unwrap_or(true);
                if !ok {
                    let msg = args
                        .get(1)
                        .and_then(str_lit)
                        .unwrap_or_else(|| "assertion failed".to_string());
                    self.failure = Some((msg, span));
                }
            }
            _ => {}
        }
    }

    fn signal_of(&self, e: &ast::Expr) -> Option<SignalId> {
        if let ast::Expr::Path(p) = e {
            if p.segments.len() == 1 {
                return self.map.get(&p.segments[0].text).copied();
            }
        }
        None
    }

    fn range_count(&self, range: &ast::Expr) -> u64 {
        if let ast::Expr::Range { lo, hi, .. } = range {
            let (a, b) = (self.eval(lo).to_u64(), self.eval(hi).to_u64());
            return b.saturating_sub(a);
        }
        0
    }

    /// Evaluate an AST expression against the simulator via the signal map.
    fn eval(&self, e: &ast::Expr) -> S {
        match e {
            ast::Expr::Int { text, .. } => S::from_u64(parse_u64(text)),
            ast::Expr::SuffixLit { text, suffix, .. } => S::from_u64(
                parse_u64(text).saturating_mul(ast::suffix_scale(&suffix.text).unwrap_or(1) as u64),
            ),
            ast::Expr::BitStrLit { base, digits, .. } => {
                let radix = if *base == 'x' { 16 } else { 2 };
                S::from_u64(u64::from_str_radix(digits, radix).unwrap_or(0))
            }
            ast::Expr::Bool { value, .. } => S::from_u64(*value as u64),
            ast::Expr::LogicLit { ch, .. } => S::from_u64(logic_value(*ch)),
            ast::Expr::Path(p) if p.segments.len() == 1 => self
                .map
                .get(&p.segments[0].text)
                .map(|&id| self.sim.state[id.0 as usize].current)
                .unwrap_or_else(|| S::from_u64(0)),
            // `Enum::Variant` evaluates to its discriminant.
            ast::Expr::Path(p) if p.segments.len() >= 2 => S::from_u64(
                self.enums
                    .get(&p.segments[0].text)
                    .and_then(|m| m.get(&p.segments[1].text))
                    .copied()
                    .unwrap_or(0),
            ),
            // A struct-field (`p.data`) or array-element (`a[2]`) read resolves
            // through the flattened map.
            ast::Expr::Field { .. } | ast::Expr::Index { .. } => expr_path(e)
                .and_then(|p| self.map.get(&p))
                .map(|&id| self.sim.state[id.0 as usize].current)
                .unwrap_or_else(|| S::from_u64(0)),
            ast::Expr::Unary { op, rhs, .. } => {
                let a = self.eval(rhs);
                match op {
                    ast::UnOp::Not => S::from_u64(a.is_zero() as u64),
                    ast::UnOp::Neg => a.wrapping_neg(),
                }
            }
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                // A real operand switches to float semantics: integer literal
                // counterparts coerce, so `z.re == 10` compares 10.0.
                if self.is_real_operand(lhs) || self.is_real_operand(rhs) {
                    let a = self.eval_real(lhs);
                    let b = self.eval_real(rhs);
                    return match lower_ast_binop(*op) {
                        BinOp::Add => S::from_u64((a + b).to_bits()),
                        BinOp::Sub => S::from_u64((a - b).to_bits()),
                        BinOp::Mul => S::from_u64((a * b).to_bits()),
                        BinOp::Div => S::from_u64((a / b).to_bits()),
                        BinOp::Eq => S::from_u64((a == b) as u64),
                        BinOp::Ne => S::from_u64((a != b) as u64),
                        BinOp::Lt => S::from_u64((a < b) as u64),
                        BinOp::Le => S::from_u64((a <= b) as u64),
                        BinOp::Gt => S::from_u64((a > b) as u64),
                        BinOp::Ge => S::from_u64((a >= b) as u64),
                        other => apply_binop(other, S::from_u64(a.to_bits()), S::from_u64(b.to_bits())),
                    };
                }
                apply_binop(lower_ast_binop(*op), self.eval(lhs), self.eval(rhs))
            }
            _ => S::from_u64(0),
        }
    }

    /// Whether a stimulus expression reads a `real` signal.
    fn is_real_operand(&self, e: &ast::Expr) -> bool {
        expr_path(e)
            .and_then(|p| self.map.get(&p))
            .map(|&id| self.sim.design.signals[id.0 as usize].real)
            .unwrap_or(false)
    }

    /// The f64 value of a stimulus operand: real signals read their bits,
    /// integer/decimal literals parse as floats, everything else converts.
    fn eval_real(&self, e: &ast::Expr) -> f64 {
        match e {
            ast::Expr::Int { text, .. } => text.parse().unwrap_or(0.0),
            _ if self.is_real_operand(e) => f64::from_bits(self.eval(e).to_u64()),
            _ => self.eval(e).to_u64() as f64,
        }
    }
}

/// The dotted signal path of a name, struct-field, or constant-index access
/// (`p.data`, `a[2]`).
fn expr_path(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        ast::Expr::Field { base, field, .. } => {
            Some(format!("{}.{}", expr_path(base)?, field.text))
        }
        ast::Expr::Index { base, index, .. } => match index.as_ref() {
            ast::Expr::Int { text, .. } => Some(format!("{}[{}]", expr_path(base)?, parse_u64(text))),
            _ => None,
        },
        _ => None,
    }
}

fn has_attr(e: &ast::EntityDecl, name: &str) -> bool {
    e.attrs.iter().any(|a| a.name.segments.last().map(|s| s.text.as_str()) == Some(name))
}

fn type_head_name(t: &ast::Type) -> Option<&str> {
    match t {
        ast::Type::Path(p) => p.segments.first().map(|s| s.text.as_str()),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => type_head_name(base),
        ast::Type::Mode { inner, .. } => type_head_name(inner),
    }
}

fn str_lit(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::StrLit { text, .. } => Some(text.clone()),
        _ => None,
    }
}

/// The femtosecond duration of a `wait` argument like `10.ns` (parsed as a field
/// access `10 . ns`) or a suffixed literal `10ns`. Unknown forms default to a
/// half period.
fn duration_fs(args: &[ast::Expr]) -> u64 {
    match args.first() {
        Some(ast::Expr::Field { base, field, .. }) => {
            if let ast::Expr::Int { text, .. } = base.as_ref() {
                let scale = ast::suffix_scale(&field.text).unwrap_or(1_000_000);
                return parse_u64(text) * scale as u64;
            }
            HALF_PERIOD
        }
        Some(ast::Expr::SuffixLit { text, suffix, .. }) => {
            parse_u64(text) * ast::suffix_scale(&suffix.text).unwrap_or(1_000_000) as u64
        }
        _ => HALF_PERIOD,
    }
}

fn parse_u64(text: &str) -> u64 {
    let t = text.trim();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).unwrap_or(0)
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        u64::from_str_radix(b, 2).unwrap_or(0)
    } else {
        t.parse().unwrap_or(0)
    }
}

fn lower_ast_binop(op: ast::BinOp) -> BinOp {
    use ast::BinOp as A;
    match op {
        A::Add => BinOp::Add,
        A::Sub => BinOp::Sub,
        A::Mul => BinOp::Mul,
        A::Div => BinOp::Div,
        A::And => BinOp::And,
        A::Nand => BinOp::Nand,
        A::Or => BinOp::Or,
        A::Nor => BinOp::Nor,
        A::Xor => BinOp::Xor,
        A::Xnor => BinOp::Xnor,
        A::Shl => BinOp::Shl,
        A::Shr => BinOp::Shr,
        A::Eq => BinOp::Eq,
        A::Ne => BinOp::Ne,
        A::Lt => BinOp::Lt,
        A::Le => BinOp::Le,
        A::Gt => BinOp::Gt,
        A::Ge => BinOp::Ge,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use siox_diag::FileId;

    use siox_diag::DiagnosticSink;

    /// Lower a source string through the full frontend into IR.
    fn lower(src: &str) -> Design {
        let mut sink = DiagnosticSink::new();
        let module = siox_syntax::parse_module(FileId(0), src, &mut sink);
        assert_eq!(sink.error_count(), 0, "parse errors:\n{src}");
        let modules = std::slice::from_ref(&module);
        let resolved = siox_resolve::resolve(modules, &mut sink);
        let typed = siox_types::check(modules, &resolved, &mut sink);
        let hier = siox_elab::elaborate(modules, &typed, &mut sink);
        siox_ir::lower(modules, &hier, &mut sink)
    }

    /// Lower + run the test entities in a source string.
    fn run(src: &str) -> Vec<TestResult> {
        let mut sink = DiagnosticSink::new();
        let module = siox_syntax::parse_module(FileId(0), src, &mut sink);
        assert_eq!(sink.error_count(), 0, "parse errors:\n{src}");
        let modules = std::slice::from_ref(&module);
        let resolved = siox_resolve::resolve(modules, &mut sink);
        let typed = siox_types::check(modules, &resolved, &mut sink);
        let hier = siox_elab::elaborate(modules, &typed, &mut sink);
        let design = siox_ir::lower(modules, &hier, &mut sink);
        run_tests(modules, &hier, &design, None)
    }

    const COUNTER_TEST: &str = "module m;\n\
        entity Counter<W: integer> {\n\
          in clk: Clock; in rst: Logic; in en: Bit; out count: uint[W];\n\
        }\n\
        impl Counter<W: integer> {\n\
          let value: uint[W] = 0;\n\
          if clk::rising {\n\
            if rst == '1' { value = 0; } else if en { value = value + 1; }\n\
          }\n\
          count = value;\n\
        }\n\
        #[test]\n\
        entity CounterTest {}\n\
        impl CounterTest {\n\
          let clk: Logic = '0';\n\
          let rst: Logic = '1';\n\
          let en: Bit = '1';\n\
          let count: uint[8];\n\
          let dut = Counter<W = 8> { .clk, .rst, .en, .count };\n\
          wait 10.ns;\n\
          rst = '0';\n\
          for i in 0..10 { tick(clk); }\n\
          PLACEHOLDER\n\
        }\n";

    #[test]
    fn passing_test_entity_reports_pass() {
        let src = COUNTER_TEST.replace("PLACEHOLDER", "assert!(count == 10, \"should be 10\");");
        let results = run(&src);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "CounterTest");
        assert!(results[0].passed, "failure: {:?}", results[0].failure);
    }

    fn assert_test_passes(src: &str) {
        let results = run(src);
        assert_eq!(results.len(), 1);
        assert!(results[0].passed, "{}: {:?}", results[0].name, results[0].failure);
    }

    #[test]
    fn simulates_a_combinational_mux() {
        // `y = b`, then `y = a` when `sel` — later driver wins (spec 3.14).
        assert_test_passes(
            "module m;\n\
             entity Mux { in sel: Bit; in a: Bit; in b: Bit; out y: Bit; }\n\
             impl Mux { y = b; if sel { y = a; } }\n\
             #[test] entity T {}\n\
             impl T {\n\
               let sel: Bit = '0'; let a: Bit = '1'; let b: Bit = '0'; let y: Bit;\n\
               let dut = Mux { .sel, .a, .b, .y };\n\
               assert!(y == '0', \"sel=0 -> b\");\n\
               sel = '1';\n\
               assert!(y == '1', \"sel=1 -> a\");\n\
             }\n",
        );
    }

    #[test]
    fn simulates_an_fsm_with_match() {
        // Idle --(start)--> Run --> Done --> Idle, driven by a `match` on an enum.
        assert_test_passes(
            "module m;\n\
             enum State: uint[2] { Idle = 0, Run = 1, Done = 2 }\n\
             entity Fsm { in clk: Clock; in start: Bit; out st: State; }\n\
             impl Fsm {\n\
               let s: State = State::Idle;\n\
               if clk::rising {\n\
                 match s {\n\
                   State::Idle => { if start { s = State::Run; } }\n\
                   State::Run => { s = State::Done; }\n\
                   _ => { s = State::Idle; }\n\
                 }\n\
               }\n\
               st = s;\n\
             }\n\
             #[test] entity T {}\n\
             impl T {\n\
               let clk: Logic = '0'; let start: Bit = '0'; let st: State;\n\
               let dut = Fsm { .clk, .start, .st };\n\
               tick(clk);\n\
               assert!(st == State::Idle, \"stays idle\");\n\
               start = '1';\n\
               tick(clk);\n\
               assert!(st == State::Run, \"-> run\");\n\
               tick(clk);\n\
               assert!(st == State::Done, \"-> done\");\n\
               tick(clk);\n\
               assert!(st == State::Idle, \"-> idle\");\n\
             }\n",
        );
    }

    #[test]
    fn simulates_concatenation() {
        // `{hi, lo}` packs two nibbles into a byte (hi is the high nibble).
        assert_test_passes(
            "module m;\n\
             entity Join { in hi: uint[4]; in lo: uint[4]; out y: uint[8]; }\n\
             impl Join { y = {hi, lo}; }\n\
             #[test] entity T {}\n\
             impl T {\n\
               let hi: uint[4] = 0; let lo: uint[4] = 0; let y: uint[8];\n\
               let dut = Join { .hi, .lo, .y };\n\
               hi = 10; lo = 11;\n\
               assert!(y == 171, \"0xA:0xB == 0xAB\");\n\
             }\n",
        );
    }

    #[test]
    fn simulates_nested_concatenation() {
        // `{a, {b, c}}`: a occupies bits 7..4, then b bits 3..2, c bits 1..0.
        // a=1, b=2, c=3 -> (1<<4) | (2<<2) | 3 = 16 | 8 | 3 = 27.
        assert_test_passes(
            "module m;\n\
             entity J { in a: uint[4]; in b: uint[2]; in c: uint[2]; out y: uint[8]; }\n\
             impl J { y = {a, {b, c}}; }\n\
             #[test] entity T {}\n\
             impl T {\n\
               let a: uint[4] = 0; let b: uint[2] = 0; let c: uint[2] = 0; let y: uint[8];\n\
               let dut = J { .a, .b, .c, .y };\n\
               a = 1; b = 2; c = 3;\n\
               assert!(y == 27, \"nested concat packs correctly\");\n\
             }\n",
        );
    }

    #[test]
    fn simulates_bit_slices() {
        // `data[7..4]` is the high nibble, `data[3..0]` the low nibble.
        assert_test_passes(
            "module m;\n\
             entity Split { in data: uint[8]; out hi: uint[4]; out lo: uint[4]; }\n\
             impl Split { hi = data[7..4]; lo = data[3..0]; }\n\
             #[test] entity T {}\n\
             impl T {\n\
               let data: uint[8] = 0; let hi: uint[4]; let lo: uint[4];\n\
               let dut = Split { .data, .hi, .lo };\n\
               data = 171;\n\
               assert!(hi == 10, \"high nibble of 0xAB\");\n\
               assert!(lo == 11, \"low nibble of 0xAB\");\n\
             }\n",
        );
    }

    #[test]
    fn simulates_array_element_signals() {
        // An array-typed port flattens to per-element signals; the DUT reads a
        // constant index, and the testbench drives elements individually.
        assert_test_passes(
            "module m;\n\
             entity Bank { in a: Bit[4]; out y: Bit; }\n\
             impl Bank { y = a[2]; }\n\
             #[test] entity T {}\n\
             impl T {\n\
               let a: Bit[4]; let y: Bit;\n\
               let dut = Bank { .a, .y };\n\
               a[2] = '1';\n\
               assert!(y == '1', \"reads a[2]\");\n\
               a[2] = '0';\n\
               assert!(y == '0', \"tracks a[2]\");\n\
             }\n",
        );
    }

    #[test]
    fn nameless_struct_literal_initialises_fields() {
        // `let p: Packet = { .valid = '1', .data = 5 }` — type from the target,
        // fields set individually.
        assert_test_passes(
            "module m;\n\
             struct Packet { valid: Bit, data: uint[8] }\n\
             entity Sink { in p: Packet; out got: uint[8]; }\n\
             impl Sink { got = p.data; }\n\
             #[test] entity T {}\n\
             impl T {\n\
               let p: Packet = { .valid = '1', .data = 5 };\n\
               let got: uint[8];\n\
               let dut = Sink { .p, .got };\n\
               assert!(got == 5, \"initialised p.data\");\n\
             }\n",
        );
    }

    #[test]
    fn simulates_struct_field_signals() {
        // A struct-typed port flattens to per-field signals; the DUT reads one
        // field, and the testbench drives fields individually.
        assert_test_passes(
            "module m;\n\
             struct Packet { valid: Bit, data: uint[8] }\n\
             entity Sink { in p: Packet; out got: uint[8]; }\n\
             impl Sink { got = p.data; }\n\
             #[test] entity T {}\n\
             impl T {\n\
               let p: Packet; let got: uint[8];\n\
               let dut = Sink { .p, .got };\n\
               p.data = 55;\n\
               p.valid = '1';\n\
               assert!(got == 55, \"reads p.data\");\n\
               p.data = 7;\n\
               assert!(got == 7, \"tracks p.data\");\n\
             }\n",
        );
    }

    #[test]
    fn simulates_a_ready_valid_handshake() {
        // A 1-deep buffer captures `d` only when `valid and ready` on a rising
        // edge (exercises a compound condition in an event block).
        assert_test_passes(
            "module m;\n\
             entity Fifo1 { in clk: Clock; in valid: Bit; in ready: Bit; in d: uint[8]; out q: uint[8]; }\n\
             impl Fifo1 {\n\
               let buf: uint[8] = 0;\n\
               if clk::rising { if valid and ready { buf = d; } }\n\
               q = buf;\n\
             }\n\
             #[test] entity T {}\n\
             impl T {\n\
               let clk: Logic = '0'; let valid: Bit = '0'; let ready: Bit = '0';\n\
               let d: uint[8] = 0; let q: uint[8];\n\
               let dut = Fifo1 { .clk, .valid, .ready, .d, .q };\n\
               d = 99; valid = '1';\n\
               tick(clk);\n\
               assert!(q == 0, \"no capture without ready\");\n\
               ready = '1';\n\
               tick(clk);\n\
               assert!(q == 99, \"captured on valid and ready\");\n\
             }\n",
        );
    }

    #[test]
    fn simulates_an_enum_old_monitor() {
        // `started` pulses for one step on the Idle -> Run transition, detected
        // combinationally via `state::old`.
        assert_test_passes(
            "module m;\n\
             enum State: uint[2] { Idle = 0, Run = 1, Done = 2 }\n\
             entity Mon { in state: State; out started: Bit; }\n\
             impl Mon {\n\
               started = '0';\n\
               if state::old == State::Idle and state == State::Run { started = '1'; }\n\
             }\n\
             #[test] entity T {}\n\
             impl T {\n\
               let state: State = State::Idle; let started: Bit;\n\
               let dut = Mon { .state, .started };\n\
               assert!(started == '0', \"no transition yet\");\n\
               state = State::Run;\n\
               assert!(started == '1', \"idle -> run detected\");\n\
               state = State::Done;\n\
               assert!(started == '0', \"not an idle -> run\");\n\
             }\n",
        );
    }

    #[test]
    fn simulates_a_register() {
        // q captures d on the rising edge and holds between edges.
        assert_test_passes(
            "module m;\n\
             entity Reg<W: integer> { in clk: Clock; in d: uint[W]; out q: uint[W]; }\n\
             impl Reg<W: integer> { let s: uint[W] = 0; if clk::rising { s = d; } q = s; }\n\
             #[test] entity T {}\n\
             impl T {\n\
               let clk: Logic = '0'; let d: uint[8] = 0; let q: uint[8];\n\
               let dut = Reg<W = 8> { .clk, .d, .q };\n\
               assert!(q == 0, \"starts at 0\");\n\
               d = 42;\n\
               tick(clk);\n\
               assert!(q == 42, \"captures d\");\n\
               d = 7;\n\
               assert!(q == 42, \"holds between edges\");\n\
               tick(clk);\n\
               assert!(q == 7, \"next edge\");\n\
             }\n",
        );
    }

    #[test]
    fn failing_assertion_reports_message_and_span() {
        let src = COUNTER_TEST.replace("PLACEHOLDER", "assert!(count == 99, \"wrong count\");");
        let results = run(&src);
        assert_eq!(results.len(), 1);
        assert!(!results[0].passed);
        assert_eq!(results[0].failure.as_deref(), Some("wrong count"));
        assert!(results[0].span.is_some());
    }

    #[test]
    fn name_filter_selects_a_test() {
        let src = COUNTER_TEST.replace("PLACEHOLDER", "assert!(count == 10, \"ok\");");
        let mut sink = DiagnosticSink::new();
        let module = siox_syntax::parse_module(FileId(0), &src, &mut sink);
        let modules = std::slice::from_ref(&module);
        let resolved = siox_resolve::resolve(modules, &mut sink);
        let typed = siox_types::check(modules, &resolved, &mut sink);
        let hier = siox_elab::elaborate(modules, &typed, &mut sink);
        let design = siox_ir::lower(modules, &hier, &mut sink);

        assert_eq!(run_tests(modules, &hier, &design, Some("Counter")).len(), 1);
        assert_eq!(run_tests(modules, &hier, &design, Some("Nope")).len(), 0);
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
        #[top]\n\
        entity H {}\n\
        impl H {\n\
          let clk: Logic = '0';\n\
          let dut = Counter<W = 8> { .clk, .rst, .en, .count };\n\
        }\n";

    /// Toggle the clock through a full cycle (rising then falling).
    fn tick(sim: &mut Simulator, clk: SignalId) {
        sim.set(clk, 1);
        sim.settle();
        sim.set(clk, 0);
        sim.settle();
    }

    #[test]
    fn counter_increments_on_rising_edges() {
        let design = lower(COUNTER);
        let mut sim = Simulator::new(&design);
        let clk = sim.signal("Counter.clk").unwrap();
        let rst = sim.signal("Counter.rst").unwrap();
        let en = sim.signal("Counter.en").unwrap();
        let count = sim.signal("Counter.count").unwrap();

        sim.set(rst, 0);
        sim.set(en, 1);
        sim.settle();
        assert_eq!(sim.read(count), 0);

        for _ in 0..10 {
            tick(&mut sim, clk);
        }
        assert_eq!(sim.read(count), 10);
    }

    #[test]
    fn arithmetic_wraps_at_the_signal_width() {
        // A 2-bit counter wraps at 4: after 5 ticks, count == 5 mod 4 == 1.
        let src = COUNTER.replace("W = 8", "W = 2");
        let design = lower(&src);
        let mut sim = Simulator::new(&design);
        let clk = sim.signal("Counter.clk").unwrap();
        let rst = sim.signal("Counter.rst").unwrap();
        let en = sim.signal("Counter.en").unwrap();
        let count = sim.signal("Counter.count").unwrap();
        sim.set(rst, 0);
        sim.set(en, 1);
        sim.settle();
        for _ in 0..5 {
            tick(&mut sim, clk);
        }
        assert_eq!(sim.read(count), 1);
    }

    #[test]
    fn mixed_operand_operator_impl_selects_by_rhs_type() {
        // `10 + 5i`: integer lhs finds `impl "+" for integer` with a Complex
        // rhs; Complex lhs overloads select between Complex and integer rhs.
        let results = run(
            "module m;\n\
             struct Complex { re: uint[8], im: uint[8] }\n\
             impl Suffix for Complex {\n\
               fn i(v: integer) -> Complex {\n\
                 return Complex { .re = 0, .im = v };\n\
               }\n\
             }\n\
             impl \"+\" for Complex {\n\
               fn apply(self, rhs: Complex) -> Complex {\n\
                 return Complex { .re = self.re + rhs.re, .im = self.im + rhs.im };\n\
               }\n\
               fn apply_int(self, rhs: integer) -> Complex {\n\
                 return Complex { .re = self.re + rhs, .im = self.im };\n\
               }\n\
             }\n\
             impl \"+\" for integer {\n\
               fn apply(self, rhs: Complex) -> Complex {\n\
                 return Complex { .re = self + rhs.re, .im = rhs.im };\n\
               }\n\
             }\n\
             entity Src { in a: Complex; out lit: Complex; out bumped: Complex; }\n\
             impl Src {\n\
               lit = 10 + 5i;\n\
               bumped = a + 3;\n\
             }\n\
             #[test]\n\
             entity MixTest {}\n\
             impl MixTest {\n\
               let a: Complex = { .re = 1, .im = 2 };\n\
               let lit: Complex;\n\
               let bumped: Complex;\n\
               let dut = Src { .a, .lit, .bumped };\n\
               wait 1ns;\n\
               assert!(lit.re == 10, \"10 + 5i re\");\n\
               assert!(lit.im == 5, \"10 + 5i im\");\n\
               assert!(bumped.re == 4, \"(1+2i) + 3 re\");\n\
               assert!(bumped.im == 2, \"(1+2i) + 3 im\");\n\
             }\n",
        );
        assert_eq!(results.len(), 1);
        assert!(results[0].passed, "{:?}", results[0].failure);
    }

    #[test]
    fn spaceship_impl_derives_all_comparisons() {
        // One `<=>` impl (major, then minor) gives <, <=, >, >=, ==, != on a
        // struct — including struct equality, which has no built-in form.
        let results = run(
            "module m;\n\
             enum Ordering { Less, Equal, Greater }\n\
             struct Version { major: uint[8], minor: uint[8] }\n\
             impl \"<=>\" for Version {\n\
               fn apply(self, rhs: Version) -> Ordering {\n\
                 if self.major < rhs.major {\n\
                   return Ordering::Less;\n\
                 }\n\
                 if self.major > rhs.major {\n\
                   return Ordering::Greater;\n\
                 }\n\
                 if self.minor < rhs.minor {\n\
                   return Ordering::Less;\n\
                 }\n\
                 if self.minor > rhs.minor {\n\
                   return Ordering::Greater;\n\
                 }\n\
                 return Ordering::Equal;\n\
               }\n\
             }\n\
             entity Cmp {\n\
               in a: Version;\n\
               in b: Version;\n\
               out lt: Bool; out le: Bool; out gt: Bool;\n\
               out ge: Bool; out eq: Bool; out ne: Bool;\n\
             }\n\
             impl Cmp {\n\
               lt = a < b;\n\
               le = a <= b;\n\
               gt = a > b;\n\
               ge = a >= b;\n\
               eq = a == b;\n\
               ne = a != b;\n\
             }\n\
             #[test]\n\
             entity SpaceshipTest {}\n\
             impl SpaceshipTest {\n\
               let a: Version = { .major = 1, .minor = 9 };\n\
               let b: Version = { .major = 2, .minor = 0 };\n\
               let lt: Bool; let le: Bool; let gt: Bool;\n\
               let ge: Bool; let eq: Bool; let ne: Bool;\n\
               let dut = Cmp { .a, .b, .lt, .le, .gt, .ge, .eq, .ne };\n\
               wait 1ns;\n\
               assert!(lt, \"1.9 < 2.0\");\n\
               assert!(le, \"1.9 <= 2.0\");\n\
               assert!(ne, \"1.9 != 2.0\");\n\
               a = { .major = 2, .minor = 0 };\n\
               wait 1ns;\n\
               assert!(eq, \"2.0 == 2.0\");\n\
               assert!(ge, \"2.0 >= 2.0\");\n\
               assert!(le, \"2.0 <= 2.0\");\n\
               a = { .major = 2, .minor = 1 };\n\
               wait 1ns;\n\
               assert!(gt, \"2.1 > 2.0 (minor breaks the tie)\");\n\
               assert!(ne, \"2.1 != 2.0\");\n\
             }\n",
        );
        assert_eq!(results.len(), 1);
        assert!(results[0].passed, "{:?}", results[0].failure);
    }

    #[test]
    fn wide_slots_carry_128_bit_signals() {
        // A value shifted above bit 63 survives only in 128-bit slots; the
        // dispatcher picks them automatically (`needs_wide`).
        let results = run(
            "module m;\n\
             entity Shifter { in a: uint[8]; out y: uint[128]; }\n\
             impl Shifter { y = a << 100; }\n\
             #[test]\n\
             entity WideTest {}\n\
             impl WideTest {\n\
               let a: uint[8] = 5;\n\
               let y: uint[128];\n\
               let dut = Shifter { .a, .y };\n\
               wait 1ns;\n\
               assert!(y != 0, \"bits above 63 survive in wide slots\");\n\
               assert!(y >> 100 == 5, \"shifted value round-trips\");\n\
               assert!(y >> 50 != 5, \"value is genuinely above bit 63\");\n\
             }\n",
        );
        assert_eq!(results.len(), 1);
        assert!(results[0].passed, "{:?}", results[0].failure);
    }

    #[test]
    fn suffix_trait_impl_types_and_inlines_literals() {
        // `impl Suffix for T`: the fn name is the suffix, the literal inlines
        // its body — `10ns` drives Time.fs with 10_000_000, `5i` a Complex.
        let results = run(
            "module m;\n\
             struct Time { fs: uint[48] }\n\
             impl Suffix for Time {\n\
               fn ns(v: integer) -> Time {\n\
                 return Time { .fs = v * 1000000 };\n\
               }\n\
             }\n\
             struct Complex { re: uint[8], im: uint[8] }\n\
             impl Suffix for Complex {\n\
               fn i(v: integer) -> Complex {\n\
                 return Complex { .re = 0, .im = v };\n\
               }\n\
             }\n\
             entity Src { out t: Time; out z: Complex; }\n\
             impl Src {\n\
               t = 10ns;\n\
               z = 5i;\n\
             }\n\
             #[test]\n\
             entity SuffixTest {}\n\
             impl SuffixTest {\n\
               let t: Time;\n\
               let z: Complex;\n\
               let dut = Src { .t, .z };\n\
               wait 1ns;\n\
               assert!(t.fs == 10000000, \"10ns is 10^7 fs\");\n\
               assert!(z.re == 0, \"5i has no real part\");\n\
               assert!(z.im == 5, \"5i has im 5\");\n\
             }\n",
        );
        assert_eq!(results.len(), 1);
        assert!(results[0].passed, "{:?}", results[0].failure);
    }

    #[test]
    fn struct_operator_impl_evaluates_per_field() {
        // `+` on a struct type: the impl body's struct literal lowers to one
        // driver per field, so complex addition works component-wise.
        let results = run(
            "module m;\n\
             struct Complex { re: uint[8], im: uint[8] }\n\
             impl \"+\" for Complex {\n\
               fn apply(self, rhs: Complex) -> Complex {\n\
                 return Complex { .re = self.re + rhs.re, .im = self.im + rhs.im };\n\
               }\n\
             }\n\
             entity Adder { in a: Complex; in b: Complex; out z: Complex; }\n\
             impl Adder { z = a + b; }\n\
             #[test]\n\
             entity ComplexTest {}\n\
             impl ComplexTest {\n\
               let a: Complex = { .re = 10, .im = 0 };\n\
               let b: Complex = { .re = 0, .im = 5 };\n\
               let z: Complex;\n\
               let dut = Adder { .a, .b, .z };\n\
               wait 1ns;\n\
               assert!(z.re == 10, \"10 + 5i has re 10\");\n\
               assert!(z.im == 5, \"10 + 5i has im 5\");\n\
               a = { .re = 3, .im = 4 };\n\
               b = { .re = 1, .im = 2 };\n\
               wait 1ns;\n\
               assert!(z.re == 4, \"(3+4i)+(1+2i) re\");\n\
               assert!(z.im == 6, \"(3+4i)+(1+2i) im\");\n\
             }\n",
        );
        assert_eq!(results.len(), 1);
        assert!(results[0].passed, "{:?}", results[0].failure);
    }

    #[test]
    fn operator_trait_impl_evaluates_in_sim() {
        // `+` on a user enum resolves to its impl body, inlined into the IR:
        // High wins, otherwise the right operand passes through.
        let results = run(
            "module m;\n\
             enum Volt { Low, High }\n\
             impl \"+\" for Volt {\n\
               fn apply(self, rhs: Volt) -> Volt {\n\
                 if self == Volt::High {\n\
                   return Volt::High;\n\
                 } else {\n\
                   return rhs;\n\
                 }\n\
               }\n\
             }\n\
             entity Mix { in a: Volt; in b: Volt; out y: Volt; }\n\
             impl Mix { y = a + b; }\n\
             #[test]\n\
             entity OpTest {}\n\
             impl OpTest {\n\
               let a: Volt = Volt::Low;\n\
               let b: Volt = Volt::Low;\n\
               let y: Volt;\n\
               let dut = Mix { .a, .b, .y };\n\
               wait 1ns;\n\
               assert!(y == Volt::Low, \"Low + Low = Low\");\n\
               b = Volt::High;\n\
               wait 1ns;\n\
               assert!(y == Volt::High, \"Low + High = High\");\n\
               a = Volt::High;\n\
               b = Volt::Low;\n\
               wait 1ns;\n\
               assert!(y == Volt::High, \"High + Low = High\");\n\
             }\n",
        );
        assert_eq!(results.len(), 1);
        assert!(results[0].passed, "{:?}", results[0].failure);
    }

    #[test]
    fn suffix_and_bitstring_literals_run() {
        let results = run(
            "module m;\n\
             entity Buf { in a: uint[8]; out y: uint[8]; }\n\
             impl Buf { y = a; }\n\
             #[test]\n\
             entity LitTest {}\n\
             impl LitTest {\n\
               let a: uint[8] = x\"AB\";\n\
               let y: uint[8];\n\
               let dut = Buf { .a, .y };\n\
               wait 1ns;\n\
               assert!(y == x\"AB\", \"hex literal drives through\");\n\
               assert!(y == 171, \"and equals its decimal value\");\n\
               a = b\"01010101\";\n\
               wait 1ns;\n\
               assert!(y == 85, \"binary literal too\");\n\
             }\n",
        );
        assert_eq!(results.len(), 1);
        assert!(results[0].passed, "{:?}", results[0].failure);
    }

    #[test]
    fn reset_clears_and_enable_gates() {
        let design = lower(COUNTER);
        let mut sim = Simulator::new(&design);
        let clk = sim.signal("Counter.clk").unwrap();
        let rst = sim.signal("Counter.rst").unwrap();
        let en = sim.signal("Counter.en").unwrap();
        let count = sim.signal("Counter.count").unwrap();

        // Count up to 3.
        sim.set(rst, 0);
        sim.set(en, 1);
        sim.settle();
        for _ in 0..3 {
            tick(&mut sim, clk);
        }
        assert_eq!(sim.read(count), 3);

        // Synchronous reset on the next rising edge.
        sim.set(rst, 1);
        sim.settle();
        tick(&mut sim, clk);
        assert_eq!(sim.read(count), 0);

        // With enable low, the count holds across an edge.
        sim.set(rst, 0);
        sim.set(en, 0);
        sim.settle();
        tick(&mut sim, clk);
        assert_eq!(sim.read(count), 0);
    }
}
