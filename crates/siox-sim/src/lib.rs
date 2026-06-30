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

/// Per-signal runtime state: current value, previous value, and event flag.
#[derive(Clone, Copy, Debug, Default)]
pub struct SignalState {
    pub current: u64,
    pub old: u64,
    pub event: bool,
}

/// Simulation kernel.
pub struct Simulator<'a> {
    design: &'a Design,
    state: Vec<SignalState>,
    /// Simulation time in femtoseconds.
    time_fs: u64,
}

/// A combinational fixpoint that fails to converge after this many iterations is
/// treated as stable (oscillation guard).
const MAX_DELTAS: usize = 10_000;

impl<'a> Simulator<'a> {
    pub fn new(design: &'a Design) -> Self {
        let state = vec![SignalState::default(); design.signals.len()];
        Simulator { design, state, time_fs: 0 }
    }

    /// The id of a signal by its hierarchical path, e.g. `Counter.count`.
    pub fn signal(&self, path: &str) -> Option<SignalId> {
        self.design.signals.iter().position(|s| s.path == path).map(|i| SignalId(i as u32))
    }

    /// Drive a signal (stimulus). Call `settle` afterwards to propagate.
    pub fn set(&mut self, sig: SignalId, value: u64) {
        self.state[sig.0 as usize].current = value;
    }

    /// Read a signal's current value.
    pub fn read(&self, sig: SignalId) -> u64 {
        self.state[sig.0 as usize].current
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
        let mut next: Vec<(usize, u64)> = Vec::new();
        for eb in &self.design.event_blocks {
            if self.eval(&eb.condition) != 0 {
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
            let mut next: Vec<u64> = self.state.iter().map(|s| s.current).collect();
            for d in &self.design.drivers {
                if self.cond_true(&d.cond) {
                    next[d.target.0 as usize] = self.eval(&d.expr);
                }
            }
            let mut changed = false;
            for (i, &v) in next.iter().enumerate() {
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
            Some(e) => self.eval(e) != 0,
        }
    }

    /// Evaluate an IR expression against the current state.
    fn eval(&self, e: &Expr) -> u64 {
        match e {
            Expr::Const(v) => *v,
            Expr::Logic(c) => logic_value(*c),
            Expr::Current(id) => self.state[id.0 as usize].current,
            Expr::Old(id) => self.state[id.0 as usize].old,
            Expr::Event(id) => self.state[id.0 as usize].event as u64,
            Expr::Unary { op, rhs } => {
                let a = self.eval(rhs);
                match op {
                    UnOp::Not => (a == 0) as u64,
                    UnOp::Neg => a.wrapping_neg(),
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let a = self.eval(lhs);
                let b = self.eval(rhs);
                apply_binop(*op, a, b)
            }
            Expr::Unknown => 0,
        }
    }
}

/// Numeric value of a logic literal. `'X'`/`'Z'` are treated as 0 in Phase 1.
fn logic_value(c: char) -> u64 {
    match c {
        '1' | 'H' => 1,
        _ => 0,
    }
}

/// `and`/`or`/... are evaluated as logical (boolean) operators in Phase 1, which
/// is correct for conditions; bitwise-on-vectors is a later, width-aware concern.
fn apply_binop(op: BinOp, a: u64, b: u64) -> u64 {
    let (la, lb) = (a != 0, b != 0);
    match op {
        BinOp::Add => a.wrapping_add(b),
        BinOp::Sub => a.wrapping_sub(b),
        BinOp::Mul => a.wrapping_mul(b),
        BinOp::Div => {
            if b != 0 {
                a / b
            } else {
                0
            }
        }
        BinOp::Shl => a.wrapping_shl(b as u32),
        BinOp::Shr => a.wrapping_shr(b as u32),
        BinOp::And => (la && lb) as u64,
        BinOp::Nand => (!(la && lb)) as u64,
        BinOp::Or => (la || lb) as u64,
        BinOp::Nor => (!(la || lb)) as u64,
        BinOp::Xor => (la ^ lb) as u64,
        BinOp::Xnor => (!(la ^ lb)) as u64,
        BinOp::Eq => (a == b) as u64,
        BinOp::Ne => (a != b) as u64,
        BinOp::Lt => (a < b) as u64,
        BinOp::Le => (a <= b) as u64,
        BinOp::Gt => (a > b) as u64,
        BinOp::Ge => (a >= b) as u64,
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

/// Discover and run every `#[test]` entity, driving its stimulus through the
/// simulator and evaluating its assertions (spec Stage 8).
///
/// Phase-1 scope: a test entity instantiates one or more DUTs and drives them
/// via `tick`/`wait`/assignments; its signals are aliased to the DUTs' signals
/// through the elaborated connections. The interpreted stimulus statements are
/// `let` initial values, assignments, `tick(clk)`, `wait`, `for` over a static
/// range, `if`, and `assert!(cond, "msg")`.
pub fn run_tests(modules: &[Module], hier: &Hierarchy, design: &Design) -> Vec<TestResult> {
    let mut entities: HashMap<&str, &ast::EntityDecl> = HashMap::new();
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

    let mut results = Vec::new();
    for &root in &hier.roots {
        let inst = hier.instance(root);
        let is_test = entities.get(inst.entity.as_str()).is_some_and(|e| has_attr(e, "test"));
        if is_test {
            let body = impls.get(inst.entity.as_str()).cloned().unwrap_or_default();
            results.push(run_one(&inst.entity, root, hier, design, &body));
        }
    }
    results
}

fn run_one(
    name: &str,
    root: siox_elab::InstanceId,
    hier: &Hierarchy,
    design: &Design,
    body: &[&ast::ImplDecl],
) -> TestResult {
    // Map this test's local signal names to design signals via the connections
    // of the DUTs it instantiates (`.clk = clk` aliases `clk` to `DUT.clk`).
    let mut map: HashMap<String, SignalId> = HashMap::new();
    for &child_id in &hier.instance(root).children {
        let child = hier.instance(child_id);
        for c in &child.connections {
            if let Some(id) = signal_id(design, &format!("{}.{}", child.entity, c.port)) {
                map.insert(c.signal.clone(), id);
            }
        }
    }

    let mut tb = Testbench { sim: Simulator::new(design), map, failure: None };

    // Apply initial `let` values, then settle.
    for im in body {
        for item in &im.items {
            if let ast::ImplItem::Let(l) = item {
                if let Some(value) = &l.value {
                    if !matches!(value, ast::Expr::Construct { .. }) {
                        let v = tb.eval(value);
                        tb.set_name(&l.name.text, v);
                    }
                }
            }
        }
    }
    tb.sim.settle();

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

    match tb.failure {
        Some((msg, span)) => {
            TestResult { name: name.to_string(), passed: false, failure: Some(msg), span: Some(span) }
        }
        None => TestResult { name: name.to_string(), passed: true, failure: None, span: None },
    }
}

/// Interprets a testbench's stimulus statements against a running simulator.
struct Testbench<'a> {
    sim: Simulator<'a>,
    /// Test-local signal name -> design signal id.
    map: HashMap<String, SignalId>,
    failure: Option<(String, Span)>,
}

impl Testbench<'_> {
    fn set_name(&mut self, name: &str, value: u64) {
        if let Some(&id) = self.map.get(name) {
            self.sim.set(id, value);
        }
    }

    fn exec(&mut self, s: &ast::Stmt) {
        match s {
            ast::Stmt::Assign { target, value, .. } => {
                let v = self.eval(value);
                if let ast::Expr::Path(p) = target {
                    if p.segments.len() == 1 {
                        self.set_name(&p.segments[0].text, v);
                    }
                }
                self.sim.settle();
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
                let branch = if self.eval(&iff.cond) != 0 {
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
            // tick(clk): one full clock cycle (rising then falling).
            "tick" => {
                if let Some(id) = args.first().and_then(|a| self.signal_of(a)) {
                    self.sim.set(id, 1);
                    self.sim.settle();
                    self.sim.set(id, 0);
                    self.sim.settle();
                }
            }
            // wait <duration>: advance time (no time-based behaviour in Phase 1).
            "wait" => self.sim.advance(0),
            // assert!(cond, "msg"): record the first failure.
            "assert" if bang => {
                let ok = args.first().map(|c| self.eval(c) != 0).unwrap_or(true);
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
            let (a, b) = (self.eval(lo), self.eval(hi));
            return b.saturating_sub(a);
        }
        0
    }

    /// Evaluate an AST expression against the simulator via the signal map.
    fn eval(&self, e: &ast::Expr) -> u64 {
        match e {
            ast::Expr::Int { text, .. } => parse_u64(text),
            ast::Expr::Bool { value, .. } => *value as u64,
            ast::Expr::LogicLit { ch, .. } => logic_value(*ch),
            ast::Expr::Path(p) if p.segments.len() == 1 => {
                self.map.get(&p.segments[0].text).map(|&id| self.sim.read(id)).unwrap_or(0)
            }
            ast::Expr::Unary { op, rhs, .. } => {
                let a = self.eval(rhs);
                match op {
                    ast::UnOp::Not => (a == 0) as u64,
                    ast::UnOp::Neg => a.wrapping_neg(),
                }
            }
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                apply_binop(lower_ast_binop(*op), self.eval(lhs), self.eval(rhs))
            }
            _ => 0,
        }
    }
}

fn signal_id(design: &Design, path: &str) -> Option<SignalId> {
    design.signals.iter().position(|s| s.path == path).map(|i| SignalId(i as u32))
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
        run_tests(modules, &hier, &design)
    }

    const COUNTER_TEST: &str = "module m;\n\
        entity Counter<W: usize> {\n\
          in clk: Clock; in rst: Logic; in en: Bit; out count: uint[W];\n\
        }\n\
        impl Counter<W: usize> {\n\
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

    #[test]
    fn failing_assertion_reports_message_and_span() {
        let src = COUNTER_TEST.replace("PLACEHOLDER", "assert!(count == 99, \"wrong count\");");
        let results = run(&src);
        assert_eq!(results.len(), 1);
        assert!(!results[0].passed);
        assert_eq!(results[0].failure.as_deref(), Some("wrong count"));
        assert!(results[0].span.is_some());
    }

    const COUNTER: &str = "module m;\n\
        entity Counter<W: usize> {\n\
          in clk: Clock;\n\
          in rst: Logic;\n\
          in en: Bit;\n\
          out count: uint[W];\n\
        }\n\
        impl Counter<W: usize> {\n\
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
