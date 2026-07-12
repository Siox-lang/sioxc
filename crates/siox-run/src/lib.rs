//! The siox simulation kernel / test runner (spec Stages 7-8), engine-agnostic.
//!
//! This crate owns the `#[test]` runner, the stimulus interpreter, the
//! `await`/`clock` scheduler and event wheel, waveform sample recording, and
//! simulation time — everything that *drives* a design, independent of how the
//! design is evaluated. A backend supplies an [`Engine`]: the JIT (`siox-llvm`)
//! does, and the `siox-sim` interpreter does too (for differential verification).

use std::collections::HashMap;

use siox_diag::Span;
use siox_elab::Hierarchy;
use siox_ir::{BinOp, Design, SignalId};
use siox_syntax::ast;
use siox_syntax::Module;

pub trait Slot: Copy + PartialEq + PartialOrd + core::fmt::Debug + Default {
    const BITS: u32;
    fn from_u64(v: u64) -> Self;
    fn from_u128(v: u128) -> Self;
    fn to_u64(self) -> u64;
    fn to_u128(self) -> u128;
    fn wrapping_add(self, o: Self) -> Self;
    fn wrapping_sub(self, o: Self) -> Self;
    fn wrapping_mul(self, o: Self) -> Self;
    fn checked_div(self, o: Self) -> Option<Self>;
    fn wrapping_shl(self, n: u32) -> Self;
    fn wrapping_shr(self, n: u32) -> Self;
    fn bitand(self, o: Self) -> Self;
    fn bitor(self, o: Self) -> Self;
    fn bitxor(self, o: Self) -> Self;
    fn bitnot(self) -> Self;
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
            fn from_u128(v: u128) -> Self {
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
            fn bitor(self, o: Self) -> Self {
                self | o
            }
            fn bitxor(self, o: Self) -> Self {
                self ^ o
            }
            fn bitnot(self) -> Self {
                !self
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

/// A backend that evaluates a `Design`: drive signals, read them, and settle
/// the combinational + sequential logic. The JIT and the interpreter both
/// implement this; the runner drives whichever it's given.
pub trait Engine {
    fn set(&mut self, sig: SignalId, value: u128);
    fn read(&self, sig: SignalId) -> u128;
    fn settle(&mut self);
    fn design(&self) -> &Design;
}

pub fn logic_value(c: char) -> u64 {
    match c {
        '1' | 'H' => 1,
        'Z' => 2,
        'X' | 'U' | 'W' => 3,
        _ => 0,
    }
}

/// `and`/`or`/`xor` are "boolean, per bit": on a bit array they apply per bit
/// (VHDL logic-vector style), on a one-bit Boolean they are plain boolean —
/// one operation, the width decides. Matches the LLVM backend.
pub fn apply_binop<S: Slot>(op: BinOp, a: S, b: S) -> S {
    let bool_s = |v: bool| S::from_u64(v as u64);
    match op {
        BinOp::Add => a.wrapping_add(b),
        BinOp::Sub => a.wrapping_sub(b),
        BinOp::Mul => a.wrapping_mul(b),
        BinOp::Div => a.checked_div(b).unwrap_or_else(|| S::from_u64(0)),
        BinOp::Shl => a.wrapping_shl(b.to_u64() as u32),
        BinOp::Shr => a.wrapping_shr(b.to_u64() as u32),
        BinOp::And => a.bitand(b),
        BinOp::Nand => a.bitand(b).bitnot(),
        BinOp::Or => a.bitor(b),
        BinOp::Nor => a.bitor(b).bitnot(),
        BinOp::Xor => a.bitxor(b),
        BinOp::Xnor => a.bitxor(b).bitnot(),
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
    /// Non-fatal `warn!` reports (message + span): the test still passes, but
    /// the summary surfaces them.
    pub warnings: Vec<(String, Span)>,
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
pub fn run_tests_with_engine<'e>(
    modules: &[Module],
    hier: &Hierarchy,
    design: &Design,
    filter: Option<&str>,
    mut make_engine: impl FnMut() -> Box<dyn Engine + 'e>,
) -> Vec<TestResult> {
    let (entities, impls) = collect_defs(modules);
    let enums = enum_discriminants(modules);
    let fns = collect_fns(modules);
    let families = siox_ir::vector_families(modules);
    let mut results = Vec::new();
    for &root in &hier.roots {
        let inst = hier.instance(root);
        let is_test = entities.get(inst.entity.as_str()).is_some_and(|e| has_attr(e, "test"));
        let selected = filter.map_or(true, |f| inst.entity.contains(f));
        if is_test && selected {
            let body = impls.get(inst.entity.as_str()).cloned().unwrap_or_default();
            let engine = make_engine();
            results.push(run_one(engine, &inst.entity, root, hier, design, &body, &enums, &fns, &families, false).0);
        }
    }
    results
}

/// Like [`run_test_traced`] but against a caller-provided engine (e.g. the
/// JIT), so waveform tracing works on the compiled backend. Records a sample at
/// every step of the first matching `#[test]`.
pub fn run_test_traced_with_engine<'e>(
    modules: &[Module],
    hier: &Hierarchy,
    design: &Design,
    filter: Option<&str>,
    mut make_engine: impl FnMut() -> Box<dyn Engine + 'e>,
) -> Option<(TestResult, Vec<Sample>)> {
    let (entities, impls) = collect_defs(modules);
    let enums = enum_discriminants(modules);
    let fns = collect_fns(modules);
    let families = siox_ir::vector_families(modules);
    for &root in &hier.roots {
        let inst = hier.instance(root);
        let is_test = entities.get(inst.entity.as_str()).is_some_and(|e| has_attr(e, "test"));
        let selected = filter.map_or(true, |f| inst.entity.contains(f));
        if is_test && selected {
            let body = impls.get(inst.entity.as_str()).cloned().unwrap_or_default();
            let engine = make_engine();
            return Some(run_one(engine, &inst.entity, root, hier, design, &body, &enums, &fns, &families, true));
        }
    }
    None
}

/// Run the first `#[test]` entity (optionally name-filtered), recording a signal
/// sample at every simulation step for waveform export (spec Stage 9).
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

/// The literal path of a `read`/`read_to_string` call, when `e` is one.
fn fs_read_path(e: &ast::Expr, which: &str) -> Option<String> {
    let ast::Expr::Call { callee, args, .. } = e else { return None };
    let ast::Expr::Path(p) = callee.as_ref() else { return None };
    if p.segments.len() != 1 || p.segments[0].text != which {
        return None;
    }
    match args.first() {
        Some(ast::Expr::StrLit { text, .. }) => Some(text.clone()),
        _ => None,
    }
}

/// Module-level functions by name (testbench-callable).
fn collect_fns(modules: &[Module]) -> HashMap<String, &ast::FnDecl> {
    let mut out = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Fn(f) = item {
                out.insert(f.name.text.clone(), f);
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
fn run_one<'a>(
    engine: Box<dyn Engine + 'a>,
    name: &str,
    root: siox_elab::InstanceId,
    hier: &Hierarchy,
    design: &Design,
    body: &[&ast::ImplDecl],
    enums: &'a HashMap<String, HashMap<String, u64>>,
    fns: &'a HashMap<String, &'a ast::FnDecl>,
    families: &'a std::collections::HashSet<String>,
    record: bool,
) -> (TestResult, Vec<Sample>) {
    // Map this test's local signal names to design signals via the connections
    // of the DUTs it instantiates. Each DUT is lowered per-instance under the
    // testbench path (`<test>.<inst>.<port>`), so `.clk = clk` aliases `clk` to
    // that instance's `clk` port — two instances of one entity stay distinct.
    // A struct port flattens to per-field entries (`.p = p` -> `p.valid`, ...).
    let mut map: HashMap<String, SignalId> = HashMap::new();
    for &child_id in &hier.instance(root).children {
        let child = hier.instance(child_id);
        for c in &child.connections {
            let prefix = format!("{}.{}.{}", name, child.name, c.port);
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

    let mut tb = Testbench {
        engine,
        map,
        enums,
        failure: None,
        record,
        samples: Vec::new(),
        clocks: Vec::new(),
        oneshots: Vec::new(),
        time_fs: 0,
        locals: HashMap::new(),
        local_widths: HashMap::new(),
        fns,
        families,
        halted: false,
        rand_state: std::cell::Cell::new(0x9E3779B97F4A7C15),
        warnings: Vec::new(),
    };

    // One pass in source order: `let`s apply as they appear (sequential
    // semantics — a mid-body let sees the signals as they are then), with a
    // settle + baseline sample before the first stimulus statement.
    let mut started = false;
    'run: for im in body {
        for item in &im.items {
            match item {
                ast::ImplItem::Let(l) => tb.apply_let(l),
                ast::ImplItem::Stmt(s) => {
                    if !started {
                        tb.engine.settle();
                        tb.sample();
                        started = true;
                    }
                    tb.exec(s);
                    if tb.failure.is_some() || tb.halted {
                        break 'run;
                    }
                }
                _ => {}
            }
        }
    }
    if !started {
        tb.engine.settle();
        tb.sample();
    }

    let warnings = std::mem::take(&mut tb.warnings);
    let result = match tb.failure {
        Some((msg, span)) => TestResult {
            name: name.to_string(),
            passed: false,
            failure: Some(msg),
            span: Some(span),
            warnings,
        },
        None => TestResult {
            name: name.to_string(),
            passed: true,
            failure: None,
            span: None,
            warnings,
        },
    };
    (result, tb.samples)
}

/// Interprets a testbench's stimulus statements against a running simulator.
/// A free-running background clock started by `clock(clk, period)`; the
/// scheduler toggles it every half period so `await clk::rising` has an edge
/// to wait for.
struct ClockGen {
    id: SignalId,
    half_period: u64,
    /// Absolute femtosecond time of the next toggle.
    next_edge: u64,
}

struct Testbench<'a> {
    engine: Box<dyn Engine + 'a>,
    /// Test-local signal name -> design signal id.
    map: HashMap<String, SignalId>,
    /// Enum name -> variant -> discriminant, for evaluating `Enum::Variant`.
    enums: &'a HashMap<String, HashMap<String, u64>>,
    failure: Option<(String, Span)>,
    /// Non-fatal `warn!(cond, msg)` reports collected during the run.
    warnings: Vec<(String, Span)>,
    /// When set, a sample is recorded after each simulation step.
    record: bool,
    samples: Vec<Sample>,
    /// Background clocks driving `await` edges/conditions.
    clocks: Vec<ClockGen>,
    /// One-shot delayed writes from `x = v after d;` — the value is evaluated
    /// at schedule time (VHDL waveform semantics): (fire time fs, signal, value).
    oneshots: Vec<(u64, SignalId, u128)>,
    /// Simulation time in femtoseconds. The runner owns the clock (the engine
    /// is purely combinational), so time is correct on any backend — including
    /// the JIT, whose settle-only engine has no notion of time.
    time_fs: u64,
    /// Testbench-local scalar values: unconnected `let`s and `for` loop
    /// variables. Consulted before the signal map.
    locals: HashMap<String, u128>,
    /// Declared bit width of a testbench local (`let c: uint[8]` -> 8), keyed
    /// by base name; writes to the local (or its `[i]` elements) mask to it so
    /// arithmetic wraps exactly like the equivalent hardware signal.
    local_widths: HashMap<String, u32>,
    /// Module-level functions callable from testbench expressions.
    fns: &'a HashMap<String, &'a ast::FnDecl>,
    /// Bit-vector families (name -> signed), for testbench conversions.
    families: &'a std::collections::HashSet<String>,
    /// `stop!()` / `finish!()` was executed: end the test cleanly (passing,
    /// unless a failure was already recorded).
    halted: bool,
    /// xorshift64* state for `rand!`/`randint!`/`uniform!` — deterministic
    /// default seed so runs reproduce; `seed!(n)` reseeds. The native harness
    /// uses the same algorithm, so all three engines agree.
    rand_state: std::cell::Cell<u64>,
}

impl Testbench<'_> {
    /// The declared bit width of a vector-family type: `uint[8]` -> 8, and the
    /// element width of an array of one (`uint[8][4]` -> 8). Anything else
    /// (enums, integer, real, structs) has no maskable width here.
    fn declared_width(&self, ty: &ast::Type) -> Option<u32> {
        if let ast::Type::Indexed { base, index: Some(i), .. } = ty {
            // `F[w][n]`: an array of vectors — the element width governs.
            if matches!(base.as_ref(), ast::Type::Indexed { .. }) {
                return self.declared_width(base);
            }
            let head = match base.as_ref() {
                ast::Type::Path(p) => p.segments.last().map(|s| s.text.as_str())?,
                _ => return None,
            };
            if !self.families.contains(head) {
                return None;
            }
            if let ast::Expr::Int { text, .. } = i.as_ref() {
                return text.parse().ok();
            }
        }
        None
    }

    /// Drive a DUT signal, masking the value to the signal's declared width so
    /// a testbench expression that overflowed (e.g. `0 - 7` evaluated in wide
    /// arithmetic) lands as the same bit pattern a hardware driver would
    /// produce. Reals pass through (f64 bits are not a bit vector).
    fn set_signal(&mut self, id: siox_ir::SignalId, v: u128) {
        let sig = &self.engine.design().signals[id.0 as usize];
        let w = sig.width;
        let v = if !sig.real && w > 0 && (w as usize) < 128 { v & ((1u128 << w) - 1) } else { v };
        self.engine.set(id, v);
    }

    /// Mask `v` to `name`'s declared local width (wrap at 2^w), matching what
    /// the engines do for hardware signals. Names without a recorded width
    /// (loop vars, integers, reals) pass through.
    fn mask_local(&self, name: &str, v: u128) -> u128 {
        // `xs[3]` masks by its base array's element width.
        let base = name.split('[').next().unwrap_or(name);
        match self.local_widths.get(base) {
            Some(&w) if w > 0 && (w as usize) < 128 => v & ((1u128 << w) - 1),
            _ => v,
        }
    }

    /// Apply a `let` in statement order: DUT-connected names write signals;
    /// an unconnected scalar becomes a testbench local.
    fn apply_let(&mut self, l: &ast::LetDecl) {
        // Record the declared width first, so this let's own initializer and
        // every later assignment mask consistently.
        if !self.map.contains_key(&l.name.text) {
            if let Some(w) = l.ty.as_ref().and_then(|t| self.declared_width(t)) {
                self.local_widths.insert(l.name.text.clone(), w);
            }
        }
        match &l.value {
            // A named construct is an instance; elaboration handled it.
            Some(ast::Expr::Construct { ty: Some(_), .. }) => {}
            // A name-less struct literal initialises each field signal.
            Some(ast::Expr::Construct { args, .. }) => {
                for c in args {
                    if let Some(v) = &c.value {
                        let field = format!("{}.{}", l.name.text, c.field.text);
                        let val = self.eval_for(&field, v);
                        self.set_name(&field, val);
                    }
                }
            }
            // `let s: string = "hello";` writes each Char element.
            Some(ast::Expr::StrLit { text, .. }) => {
                self.set_string(&l.name.text, text);
            }
            // Runtime file reads (std::fs): fill the array element-wise.
            Some(v) if fs_read_path(v, "read_to_string").is_some() => {
                let fpath = fs_read_path(v, "read_to_string").unwrap();
                match std::fs::read_to_string(self.engine.design().base_dir.join(&fpath)) {
                    Ok(text) => {
                        for (i, c) in text.chars().enumerate() {
                            self.set_elem(&l.name.text, i, c as u32 as u128);
                        }
                    }
                    Err(e) => {
                        self.failure = Some((
                            format!("read_to_string(\"{fpath}\"): {e}"),
                            l.name.span,
                        ));
                    }
                }
            }
            Some(v) if fs_read_path(v, "read").is_some() => {
                let fpath = fs_read_path(v, "read").unwrap();
                match std::fs::read(self.engine.design().base_dir.join(&fpath)) {
                    Ok(bytes) => {
                        for (i, b) in bytes.iter().enumerate() {
                            self.set_elem(&l.name.text, i, *b as u128);
                        }
                    }
                    Err(e) => {
                        self.failure =
                            Some((format!("read(\"{fpath}\"): {e}"), l.name.span));
                    }
                }
            }
            Some(value) => {
                let v = self.eval_for(&l.name.text, value);
                if self.map.contains_key(&l.name.text) {
                    self.set_name(&l.name.text, v);
                } else {
                    let v = self.mask_local(&l.name.text, v);
                    self.locals.insert(l.name.text.clone(), v);
                }
            }
            None => {
                if !self.map.contains_key(&l.name.text) {
                    self.locals.insert(l.name.text.clone(), 0);
                }
            }
        }
    }

    /// Write one array element: to the DUT signal when connected, else to a
    /// testbench-local element slot.
    fn set_elem(&mut self, name: &str, i: usize, v: u128) {
        let key = format!("{name}[{i}]");
        if let Some(&id) = self.map.get(&key) {
            self.set_signal(id, v);
        } else {
            let v = self.mask_local(&key, v);
            self.locals.insert(key, v);
        }
    }

    /// Element count of an array — DUT-connected (`xs[0]` in the signal map)
    /// or a testbench-local one.
    fn array_len(&self, path: &str) -> Option<u64> {
        let mut n = 0;
        while self.map.contains_key(&format!("{path}[{n}]"))
            || self.locals.contains_key(&format!("{path}[{n}]"))
        {
            n += 1;
        }
        (n > 0).then_some(n)
    }

    fn set_name(&mut self, name: &str, value: u128) {
        let masked = self.mask_local(name, value);
        if let std::collections::hash_map::Entry::Occupied(mut e) =
            self.locals.entry(name.to_string())
        {
            e.insert(masked);
            return;
        }
        if let Some(&id) = self.map.get(name) {
            self.set_signal(id, value);
        }
    }

    /// Write a string literal to a Char-array local, one code point per
    /// element (`s = "hi"` sets `s[0]='h'`, `s[1]='i'`).
    fn set_string(&mut self, path: &str, text: &str) {
        let prefix = format!("{path}[");
        let mut ids: Vec<(usize, SignalId)> = self
            .map
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, &id)| (id.0 as usize, id))
            .collect();
        ids.sort_by_key(|(i, _)| *i);
        for ((_, id), c) in ids.into_iter().zip(text.chars()) {
            self.engine.set(id, c as u32 as u128);
        }
    }

    /// Evaluate a stimulus value for a named target: `real` targets take the
    /// value's f64 bits (`a.re = 3` stores 3.0).
    fn eval_for(&self, name: &str, e: &ast::Expr) -> u128 {
        let real = self
            .map
            .get(name)
            .map(|&id| self.engine.design().signals[id.0 as usize].real)
            .unwrap_or(false);
        if real {
            return u128::from_u64(self.eval_real(e).to_bits());
        }
        let is_char = self
            .map
            .get(name)
            .map(|&id| self.engine.design().signals[id.0 as usize].char)
            .unwrap_or(false);
        if is_char {
            if let ast::Expr::LogicLit { ch, .. } = e {
                return u128::from_u64(*ch as u32 as u64);
            }
        }
        self.eval(e)
    }

    /// Record the full signal vector at the current simulation time.
    fn sample(&mut self) {
        if self.record {
            let n = self.engine.design().signals.len() as u32;
            let values = (0..n).map(|i| self.engine.read(SignalId(i))).collect();
            self.samples.push(Sample { time_fs: self.time_fs, values });
        }
        self.check_ranges();
    }

    /// The dynamic range assert (spec 3.26): after every settle, a ranged
    /// numeric's settled value must lie in its declared domain — leaving it
    /// fails the test, like an assertion. Plain uint/int wrap instead.
    fn check_ranges(&mut self) {
        if self.failure.is_some() {
            return;
        }
        let design = self.engine.design();
        for (i, sig) in design.signals.iter().enumerate() {
            let Some((lo, hi)) = sig.range else { continue };
            let raw = self.engine.read(SignalId(i as u32)).to_u64();
            // Decode two's complement when the domain dips below zero.
            let v = if lo < 0 && sig.width > 0 && sig.width < 64 {
                let sign = 1u64 << (sig.width - 1);
                if raw & sign != 0 {
                    (raw as i64) - (1i64 << sig.width)
                } else {
                    raw as i64
                }
            } else {
                raw as i64
            };
            if v < lo || v > hi {
                self.failure = Some((
                    format!(
                        "`{}` = {v} left its range {lo}..{hi} at {} fs",
                        sig.path, self.time_fs
                    ),
                    Span::new(siox_diag::FileId(0), 0..0),
                ));
                return;
            }
        }
    }

    fn exec(&mut self, s: &ast::Stmt) {
        match s {
            ast::Stmt::Assign { target, value, after, .. } => {
                if let Some(delay) = after {
                    self.exec_after(target, value, delay);
                    return;
                }
                if let Some(path) = expr_path(target) {
                    // A string literal assigns each Char element (`s = "hi";`).
                    if let ast::Expr::StrLit { text, .. } = value {
                        self.set_string(&path, text);
                        self.engine.settle();
                        self.sample();
                        return;
                    }
                    // A struct literal assigns each field of a flattened
                    // struct local (`a = { .re = 3, .im = 4 };`).
                    if let ast::Expr::Construct { args, .. } = value {
                        for arg in args {
                            let field = format!("{path}.{}", arg.field.text);
                            let v = arg
                                .value
                                .as_ref()
                                .map(|v| self.eval_for(&field, v))
                                .unwrap_or_else(|| u128::from_u64(0));
                            self.set_name(&field, v);
                        }
                    } else {
                        let v = self.eval_for(&path, value);
                        self.set_name(&path, v);
                    }
                }
                self.engine.settle();
                self.sample();
            }
            ast::Stmt::Expr(ast::Expr::Call { callee, args, bang, span }) => {
                self.exec_call(callee, args, *bang, *span);
            }
            ast::Stmt::For { var, range, body, .. } => {
                // `for x in xs` iterates an array's elements (Python-style);
                // `for i in lo..hi` binds the index. The loop variable is a
                // testbench local for the body's duration.
                let elems: Vec<u128> = if let Some(path) = expr_path(range) {
                    let n = self.array_len(&path).unwrap_or(0);
                    (0..n)
                        .map(|i| {
                            let key = format!("{path}[{i}]");
                            if let Some(&v) = self.locals.get(&key) {
                                v
                            } else {
                                self.map
                                    .get(&key)
                                    .map(|&id| self.engine.read(id))
                                    .unwrap_or_else(|| u128::from_u64(0))
                            }
                        })
                        .collect()
                } else if let ast::Expr::Range { lo, hi, .. } = range {
                    // Inclusive, directional (`0..2` -> 0,1,2; `2..0` -> 2,1,0),
                    // matching bit slices and array ranges.
                    let (a, b) = (self.eval(lo).to_u64(), self.eval(hi).to_u64());
                    if a <= b {
                        (a..=b).map(u128::from_u64).collect()
                    } else {
                        (b..=a).rev().map(u128::from_u64).collect()
                    }
                } else {
                    Vec::new()
                };
                let saved = self.locals.get(&var.text).copied();
                for v in elems {
                    self.locals.insert(var.text.clone(), v);
                    for s in &body.stmts {
                        self.exec(s);
                        if self.failure.is_some() || self.halted {
                            return;
                        }
                    }
                }
                match saved {
                    Some(v) => self.locals.insert(var.text.clone(), v),
                    None => self.locals.remove(&var.text),
                };
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
                        if self.failure.is_some() || self.halted {
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
            // `tick`/`wait` were removed in favour of the one timing
            // primitive: start a generator (`clk = not clk after 5ns;`) and
            // `await` on it. `wait` already errors at parse; `tick` is plain
            // call syntax, so fail the test with the pointer instead.
            "tick" => {
                self.failure = Some((
                    "`tick()` was removed (it returns as a std function later); write \
                     `clk = '1'; await 5ns; clk = '0';` or start a clock generator \
                     (`clk = not clk after 5ns;`)"
                        .to_string(),
                    span,
                ));
            }
            // clock() was sugar; the canonical generator is the after-form.
            "clock" => {
                self.failure = Some((
                    "`clock()` was removed; write `clk = not clk after <half-period>;`"
                        .to_string(),
                    span,
                ));
            }
            // await <duration> | <edge> | <condition>.
            "await" => self.do_await(args),
            // print!("n={} at {}", n, t): simulation output. `{}` renders per
            // the argument's kind (real -> float, Char -> the character,
            // else decimal); auto-newline, like $display.
            "print" if bang => {
                let Some(ast::Expr::StrLit { text, .. }) = args.first() else { return };
                let mut out = String::new();
                let mut vals = args[1..].iter();
                let mut rest = text.as_str();
                while let Some(i) = rest.find("{}") {
                    out.push_str(&rest[..i]);
                    if let Some(a) = vals.next() {
                        out.push_str(&self.render_arg(a));
                    }
                    rest = &rest[i + 2..];
                }
                out.push_str(rest);
                println!("{out}");
            }
            // seed!(n): reseed the deterministic RNG.
            "seed" => {
                let n = args.first().map(|a| self.eval(a).to_u64()).unwrap_or(1);
                self.rand_state.set(if n == 0 { 1 } else { n });
            }
            // stop!() / finish!(): end the test cleanly at this point.
            "stop" | "finish" => {
                println!("{} at {} fs", name, self.time_fs);
                self.halted = true;
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
            // warn!(cond, msg): a non-fatal assertion — records a warning but
            // lets the test continue (the recoverable tier; SV `$warning`).
            "warn" if bang => {
                let ok = args.first().map(|c| !self.eval(c).is_zero()).unwrap_or(true);
                if !ok {
                    let msg = args
                        .get(1)
                        .and_then(str_lit)
                        .unwrap_or_else(|| "warning".to_string());
                    self.warnings.push((msg, span));
                }
            }
            _ => {}
        }
    }

    /// `await <expr>`: dispatch on the argument shape — a duration advances
    /// time; `clk::rising` waits for an edge; anything else is a condition.
    fn do_await(&mut self, args: &[ast::Expr]) {
        match args.first() {
            Some(ast::Expr::SuffixLit { .. }) | Some(ast::Expr::Field { .. }) => {
                let target = self.time_fs + duration_fs(args);
                self.run_clocks_until(target);
                let now = self.time_fs;
                if target > now {
                    self.time_fs = target;
                }
                self.engine.settle();
                self.sample();
            }
            Some(ast::Expr::SysAttr { base, attr, .. }) => {
                let id = self.signal_of(base);
                self.await_edge(id, attr.text.as_str());
            }
            Some(cond) => {
                let cond = cond.clone();
                self.await_cond(&cond);
            }
            None => {}
        }
    }

    /// `x = v after d;` — a VHDL-style delayed assignment. The self-toggle
    /// idiom (`clk = !clk after 5ns;`) registers a free-running background
    /// clock with `d` as its half period (the canonical clock generator); any
    /// other RHS is evaluated now and applied at `now + d`.
    fn exec_after(&mut self, target: &ast::Expr, value: &ast::Expr, delay: &ast::Expr) {
        let Some(path) = expr_path(target) else { return };
        let Some(&id) = self.map.get(&path) else { return };
        let d = duration_fs(std::slice::from_ref(delay)).max(1);
        if let ast::Expr::Unary { op: ast::UnOp::Not, rhs, .. } = value {
            if expr_path(rhs).as_deref() == Some(path.as_str()) {
                // The signal keeps its initial value; first toggle at `now + d`.
                self.clocks.push(ClockGen { id, half_period: d, next_edge: self.time_fs + d });
                return;
            }
        }
        let v = self.eval_for(&path, value);
        self.oneshots.push((self.time_fs + d, id, v));
    }

    /// The earliest pending scheduler event: a clock edge or a one-shot write.
    fn next_event(&self) -> Option<u64> {
        let c = self.clocks.iter().map(|c| c.next_edge).min();
        let o = self.oneshots.iter().map(|&(t, _, _)| t).min();
        match (c, o) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, None) => a,
            (None, b) => b,
        }
    }

    /// Advance to the earliest pending event and fire everything due at that
    /// instant (clock toggles + one-shot writes); false if nothing is pending.
    fn step_one_clock(&mut self) -> bool {
        let Some(t) = self.next_event() else {
            return false;
        };
        if t > self.time_fs {
            self.time_fs = t;
        }
        for i in 0..self.clocks.len() {
            if self.clocks[i].next_edge == t {
                let id = self.clocks[i].id;
                let v = self.engine.read(id);
                self.engine.set(id, if v == 0 { 1 } else { 0 });
                self.clocks[i].next_edge = t + self.clocks[i].half_period;
            }
        }
        let mut fired = Vec::new();
        self.oneshots.retain(|&(ft, id, v)| {
            if ft == t {
                fired.push((id, v));
                false
            } else {
                true
            }
        });
        for (id, v) in fired {
            self.set_signal(id, v);
        }
        self.engine.settle();
        self.sample();
        true
    }

    /// Run pending events up to (and including) `target` femtoseconds.
    fn run_clocks_until(&mut self, target: u64) {
        while self.next_event().is_some_and(|t| t <= target) {
            self.step_one_clock();
        }
    }

    /// Wait for a `rising`/`falling`/`event` edge on `id`, driven by the
    /// background clocks. Bounded so a missing clock can't hang the run.
    fn await_edge(&mut self, id: Option<SignalId>, kind: &str) {
        let Some(id) = id else { return };
        let mut prev = self.engine.read(id);
        for _ in 0..1_000_000 {
            if !self.step_one_clock() {
                break;
            }
            let cur = self.engine.read(id);
            let hit = match kind {
                "rising" => prev == 0 && cur != 0,
                "falling" => prev != 0 && cur == 0,
                _ => prev != cur, // ::event
            };
            prev = cur;
            if hit {
                break;
            }
        }
    }

    /// Wait until `cond` holds, stepping the background clocks. Proceeds
    /// immediately if already true; bounded against a missing clock.
    fn await_cond(&mut self, cond: &ast::Expr) {
        self.engine.settle();
        let mut guard = 0;
        while self.eval(cond).is_zero() && guard < 1_000_000 {
            if !self.step_one_clock() {
                break;
            }
            guard += 1;
        }
    }

    /// One xorshift64* step (Marsaglia; * variant's output multiply).
    fn next_rand(&self) -> u64 {
        let mut x = self.rand_state.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rand_state.set(x);
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    /// Render one `print!` argument by its kind.
    fn render_arg(&self, a: &ast::Expr) -> String {
        let v = self.eval(a);
        // A signal argument renders per its declared kind.
        if let Some(p) = expr_path(a) {
            if let Some(&id) = self.map.get(&p) {
                let sig = &self.engine.design().signals[id.0 as usize];
                if sig.real {
                    return format!("{}", f64::from_bits(v.to_u64()));
                }
                if sig.char {
                    return char::from_u32(v.to_u64() as u32).map(String::from).unwrap_or_default();
                }
                // An enum-typed signal renders its stored discriminant as the
                // variant symbol (`'X'` for Logic, `Idle` for an FSM state).
                // `enum_syms` spans every module, `std` included.
                if let Some(ety) = &sig.enum_type {
                    if let Some(sym) =
                        self.engine.design().enum_syms.get(ety).and_then(|m| m.get(&v.to_u64()))
                    {
                        return sym.clone();
                    }
                }
            }
        }
        // A real literal keeps its float face.
        if let ast::Expr::Int { text, .. } = a {
            if text.contains('.') {
                return text.clone();
            }
        }
        format!("{}", v.to_u64())
    }

    fn signal_of(&self, e: &ast::Expr) -> Option<SignalId> {
        if let ast::Expr::Path(p) = e {
            if p.segments.len() == 1 {
                return self.map.get(&p.segments[0].text).copied();
            }
        }
        None
    }

    /// Evaluate an AST expression against the simulator via the signal map.
    /// Evaluate a module-level `fn` call in a testbench expression: bind the
    /// arguments positionally and execute the `return`/`if` body.
    fn eval_free_call(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        fenv: &HashMap<String, u128>,
    ) -> u128 {
        let name = match callee {
            ast::Expr::Path(p) if p.segments.len() == 1 => p.segments[0].text.as_str(),
            _ => return 0,
        };
        let Some(f) = self.fns.get(name) else {
            // Runtime-provided functions (std::rand): ordinary calls, no
            // special syntax — the runtime supplies the implementation.
            return match name {
                "exists" => {
                    let p = match args.first() {
                        Some(ast::Expr::StrLit { text, .. }) => text.clone(),
                        _ => return u128::from_u64(0),
                    };
                    u128::from_u64(self.engine.design().base_dir.join(&p).exists() as u64)
                }
                "rand" => u128::from_u64(self.next_rand()),
                "uniform" => {
                    let x = (self.next_rand() >> 11) as f64 / (1u64 << 53) as f64;
                    u128::from_u64(x.to_bits())
                }
                "randint" => {
                    let lo = args.first().map(|a| self.eval_env(a, fenv).to_u64()).unwrap_or(0);
                    let hi = args.get(1).map(|a| self.eval_env(a, fenv).to_u64()).unwrap_or(lo);
                    let span = hi.saturating_sub(lo).saturating_add(1).max(1);
                    u128::from_u64(lo + self.next_rand() % span)
                }
                _ => u128::from_u64(0),
            };
        };
        let Some(body) = &f.body else { return 0 };
        // Constant arguments: use the signed static evaluator (the dynamic
        // path below is unsigned words, which breaks e.g. `abs(0 - 5)`).
        let consts: Option<Vec<i64>> = args
            .iter()
            .map(|a| siox_ir::eval_const_fns(a, &HashMap::new(), self.fns, 0))
            .collect();
        if let Some(cs) = consts {
            let mut cenv = HashMap::new();
            for (p, v) in f.params.iter().filter(|p| !p.is_self).zip(cs) {
                if let Some(n) = &p.name {
                    cenv.insert(n.text.clone(), v);
                }
            }
            if let Some(v) = siox_ir::eval_const_stmts(&body.stmts, &cenv, self.fns, 0) {
                return v as u128;
            }
        }
        let mut env = HashMap::new();
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                env.insert(n.text.clone(), self.eval_env(a, fenv));
            }
        }
        self.eval_fn_stmts(&body.stmts, &env).unwrap_or(0)
    }

    fn eval_fn_stmts(&self, stmts: &[ast::Stmt], env: &HashMap<String, u128>) -> Option<u128> {
        for st in stmts {
            match st {
                ast::Stmt::Return { value, .. } => {
                    return Some(self.eval_env(value.as_ref()?, env));
                }
                ast::Stmt::If(iff) => {
                    if !self.eval_env(&iff.cond, env).is_zero() {
                        if let Some(v) = self.eval_fn_stmts(&iff.then.stmts, env) {
                            return Some(v);
                        }
                    } else if let Some(ast::ElseBranch::Block(b)) = iff.else_.as_deref() {
                        if let Some(v) = self.eval_fn_stmts(&b.stmts, env) {
                            return Some(v);
                        }
                    }
                }
                _ => return None,
            }
        }
        None
    }

    fn eval(&self, e: &ast::Expr) -> u128 {
        self.eval_env(e, &HashMap::new())
    }

    /// [`Self::eval`] with a function-parameter overlay (module-fn calls in
    /// testbench expressions bind their arguments here).
    fn eval_env(&self, e: &ast::Expr, fenv: &HashMap<String, u128>) -> u128 {
        match e {
            ast::Expr::IfExpr { cond, then, els, .. } => {
                if !self.eval_env(cond, fenv).is_zero() {
                    self.eval_env(then, fenv)
                } else {
                    self.eval_env(els, fenv)
                }
            }
            ast::Expr::Int { text, .. } => u128::from_u64(parse_u64(text)),
            ast::Expr::SuffixLit { text, suffix, .. } => u128::from_u64(
                parse_u64(text).saturating_mul(ast::suffix_scale(&suffix.text).unwrap_or(1) as u64),
            ),
            ast::Expr::BitStrLit { base, digits, .. } => {
                let radix = if *base == 'x' { 16 } else { 2 };
                u128::from_u64(u64::from_str_radix(digits, radix).unwrap_or(0))
            }
            ast::Expr::Bool { value, .. } => u128::from_u64(*value as u64),
            ast::Expr::LogicLit { ch, .. } => u128::from_u64(logic_value(*ch)),
            // Conversions (spec 3.17): testbench evaluation masks to the
            // target width (`integer(x)` passes through); source
            // sign-extension is a hardware-lowering concern.
            ast::Expr::Call { callee, args, .. } => {
                let Some(arg) = args.first() else { return 0 };
                let v = self.eval_env(arg, fenv);
                let w = match callee.as_ref() {
                    ast::Expr::Index { base, index, .. }
                        if expr_path(base)
                            .as_deref()
                            .is_some_and(|h| self.families.contains(h)) =>
                    {
                        self.eval_env(index, fenv).to_u64() as u32
                    }
                    ast::Expr::Path(p)
                        if p.segments.len() == 1 && p.segments[0].text == "resize" =>
                    {
                        args.get(1).map(|n| self.eval_env(n, fenv).to_u64() as u32).unwrap_or(0)
                    }
                    ast::Expr::Path(p)
                        if p.segments.len() == 1
                            && matches!(p.segments[0].text.as_str(), "integer" | "Char") =>
                    {
                        return v;
                    }
                    _ => return self.eval_free_call(callee, args, fenv),
                };
                if w == 0 || w >= 128 {
                    v
                } else {
                    v & ((1u128 << w) - 1)
                }
            }
            // `xs::len`: an array's element count (spec: `::` metadata).
            ast::Expr::SysAttr { base, attr, .. } if attr.text == "len" => expr_path(base)
                .and_then(|p| self.array_len(&p))
                .map(|n| u128::from_u64(n))
                .unwrap_or_else(|| u128::from_u64(0)),
            ast::Expr::Path(p) if p.segments.len() == 1 => {
                let name = &p.segments[0].text;
                if let Some(&v) = fenv.get(name) {
                    return v;
                }
                if let Some(&v) = self.locals.get(name) {
                    return v;
                }
                self.map
                    .get(name)
                    .map(|&id| self.engine.read(id))
                    .unwrap_or_else(|| u128::from_u64(0))
            }
            // `Enum::Variant` evaluates to its discriminant.
            ast::Expr::Path(p) if p.segments.len() >= 2 => u128::from_u64(
                self.enums
                    .get(&p.segments[0].text)
                    .and_then(|m| m.get(&p.segments[1].text))
                    .copied()
                    .unwrap_or(0),
            ),
            // A struct-field (`p.data`) or array-element (`a[2]`) read resolves
            // through the flattened map; a dynamic index (`xs[i]`) evaluates
            // the index first.
            ast::Expr::Field { .. } | ast::Expr::Index { .. } => {
                let key = expr_path(e).or_else(|| {
                    if let ast::Expr::Index { base, index, .. } = e {
                        let b = expr_path(base)?;
                        let i = self.eval_env(index, fenv).to_u64();
                        return Some(format!("{b}[{i}]"));
                    }
                    None
                });
                match key {
                    Some(p) => {
                        if let Some(&v) = self.locals.get(&p) {
                            v
                        } else {
                            self.map
                                .get(&p)
                                .map(|&id| self.engine.read(id))
                                .unwrap_or_else(|| u128::from_u64(0))
                        }
                    }
                    None => u128::from_u64(0),
                }
            }
            ast::Expr::Unary { op, rhs, .. } => {
                let a = self.eval_env(rhs, fenv);
                match op {
                    ast::UnOp::Not => u128::from_u64(a.is_zero() as u64),
                    ast::UnOp::Neg => a.wrapping_neg(),
                }
            }
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                // Whole-string equality: `s == "hello"` compares element-wise
                // (a string is a Char array).
                if matches!(lower_ast_binop(*op), BinOp::Eq | BinOp::Ne) {
                    if let Some(eq) = self.string_eq(lhs, rhs) {
                        let v = if matches!(lower_ast_binop(*op), BinOp::Eq) { eq } else { !eq };
                        return u128::from_u64(v as u64);
                    }
                }
                // A character literal reads through its counterpart's type:
                // a Char signal reads it as Unicode (code point).
                if self.is_char_operand(lhs) || self.is_char_operand(rhs) {
                    let a = self.eval_char(lhs);
                    let b = self.eval_char(rhs);
                    return apply_binop(lower_ast_binop(*op), a, b);
                }
                // A real operand switches to float semantics: integer literal
                // counterparts coerce, so `z.re == 10` compares 10.0.
                if self.is_real_operand(lhs) || self.is_real_operand(rhs) {
                    let a = self.eval_real(lhs);
                    let b = self.eval_real(rhs);
                    return match lower_ast_binop(*op) {
                        BinOp::Add => u128::from_u64((a + b).to_bits()),
                        BinOp::Sub => u128::from_u64((a - b).to_bits()),
                        BinOp::Mul => u128::from_u64((a * b).to_bits()),
                        BinOp::Div => u128::from_u64((a / b).to_bits()),
                        BinOp::Eq => u128::from_u64((a == b) as u64),
                        BinOp::Ne => u128::from_u64((a != b) as u64),
                        BinOp::Lt => u128::from_u64((a < b) as u64),
                        BinOp::Le => u128::from_u64((a <= b) as u64),
                        BinOp::Gt => u128::from_u64((a > b) as u64),
                        BinOp::Ge => u128::from_u64((a >= b) as u64),
                        other => apply_binop(other, u128::from_u64(a.to_bits()), u128::from_u64(b.to_bits())),
                    };
                }
                apply_binop(lower_ast_binop(*op), self.eval_env(lhs, fenv), self.eval_env(rhs, fenv))
            }
            _ => u128::from_u64(0),
        }
    }

    /// The ordered element signal ids of a Char-array local, if `e` names
    /// one (`s` -> the ids of `s[0]`, `s[1]`, ...). Elements are kept in the
    /// order the design flattened them.
    fn char_array(&self, e: &ast::Expr) -> Option<Vec<SignalId>> {
        let path = expr_path(e)?;
        let prefix = format!("{path}[");
        if !self.map.keys().any(|k| k.starts_with(&prefix)) {
            return None;
        }
        // Preserve the design's element order via signal id.
        let mut elems: Vec<(usize, SignalId)> = self
            .map
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, &id)| (id.0 as usize, id))
            .collect();
        elems.sort_by_key(|(i, _)| *i);
        Some(elems.into_iter().map(|(_, id)| id).collect())
    }

    /// Element-wise string equality when one side is a string literal (or
    /// both sides are Char arrays). `None` if this is not a string compare.
    fn string_eq(&self, lhs: &ast::Expr, rhs: &ast::Expr) -> Option<bool> {
        let lit = |e: &ast::Expr| match e {
            ast::Expr::StrLit { text, .. } => Some(text.chars().collect::<Vec<_>>()),
            _ => None,
        };
        // literal vs array
        let (arr, chars) = match (lit(lhs), lit(rhs)) {
            (Some(_), Some(_)) => return None, // two literals: not our case
            (None, Some(c)) => (self.char_array(lhs)?, c),
            (Some(c), None) => (self.char_array(rhs)?, c),
            (None, None) => {
                // array vs array
                let a = self.char_array(lhs)?;
                let b = self.char_array(rhs)?;
                return Some(
                    a.len() == b.len()
                        && a.iter().zip(&b).all(|(&x, &y)| self.engine.read(x) == self.engine.read(y)),
                );
            }
        };
        Some(
            arr.len() == chars.len()
                && arr
                    .iter()
                    .zip(&chars)
                    .all(|(&id, &c)| self.engine.read(id) == c as u32 as u128),
        )
    }

    /// Whether a stimulus expression reads a `Char` signal.
    fn is_char_operand(&self, e: &ast::Expr) -> bool {
        expr_path(e)
            .and_then(|p| self.map.get(&p))
            .map(|&id| self.engine.design().signals[id.0 as usize].char)
            .unwrap_or(false)
    }

    /// A stimulus operand in a `Char` comparison: literals are Unicode code
    /// points, signals read their slots.
    fn eval_char(&self, e: &ast::Expr) -> u128 {
        match e {
            ast::Expr::LogicLit { ch, .. } => u128::from_u64(*ch as u32 as u64),
            _ => self.eval(e),
        }
    }

    /// Whether a stimulus expression reads a `real` signal.
    fn is_real_operand(&self, e: &ast::Expr) -> bool {
        expr_path(e)
            .and_then(|p| self.map.get(&p))
            .map(|&id| self.engine.design().signals[id.0 as usize].real)
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

// These exercises run designs on the interpreter, so they need its feature.
