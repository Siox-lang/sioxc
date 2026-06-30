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

use siox_diag::DiagnosticSink;
use siox_ir::{BinOp, Design, Expr, SignalId, UnOp};

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
    /// Failure message with span info when an assertion fails.
    pub failure: Option<String>,
}

/// Discover and run all `#[test]` entities in the design (spec Stage 8).
pub fn run_tests(_design: &Design, _sink: &mut DiagnosticSink) -> Vec<TestResult> {
    // TODO(stage-8): drive stimulus (`wait`, `tick`), evaluate `assert!`.
    todo!("Stage 8: test runner")
}

#[cfg(test)]
mod tests {
    use super::*;
    use siox_diag::FileId;

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
