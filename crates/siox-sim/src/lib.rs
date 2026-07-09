//! The siox delta-cycle interpreter — the reference engine.
//!
//! [`Simulator`] evaluates a `siox-ir` `Design` directly and implements
//! [`siox_run::Engine`], so the shared test runner can drive it. It is kept as
//! the **differential oracle** verifying the compiled (LLVM) backend, and as the
//! >64-bit fallback. See `siox-run` for the runner and `siox-llvm` for the
//! default engine.
//!
//! `settle` runs the delta cycle: mark `::event` for stimulus changes, evaluate
//! combinational drivers to a fixpoint, fire event blocks (next-state from
//! pre-commit values, spec 3.13), commit + re-settle, then roll `old<-current`.

use std::collections::HashMap;

use siox_elab::Hierarchy;
use siox_ir::{Design, Expr, ProcessKind, SignalId, UnOp};
use siox_syntax::Module;

use siox_run::{
    apply_binop, logic_value, run_test_traced_with_engine, run_tests_with_engine, Engine, Sample,
    Slot, TestResult,
};

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
    /// Combinational processes: `(target, source-ordered driver indices)`.
    /// Built once from `Design::processes` for event-driven dispatch.
    comb: Vec<(SignalId, Vec<usize>)>,
    /// Read signal -> combinational processes sensitive to it.
    sens: HashMap<SignalId, Vec<usize>>,
}

/// A combinational fixpoint that fails to converge after this many iterations is
/// treated as stable (oscillation guard).
const MAX_DELTAS: usize = 10_000;

impl<'a, S: Slot> Simulator<'a, S> {
    pub fn new(design: &'a Design) -> Self {
        let state = vec![SignalState::default(); design.signals.len()];
        // Combinational processes and their sensitivity, for event-driven
        // dispatch (only recompute a target when a signal it reads changes).
        let mut comb = Vec::new();
        let mut sens: HashMap<SignalId, Vec<usize>> = HashMap::new();
        for p in design.processes() {
            if let ProcessKind::Comb { target, drivers } = p.kind {
                let pi = comb.len();
                for r in &p.reads {
                    sens.entry(*r).or_default().push(pi);
                }
                comb.push((target, drivers));
            }
        }
        Simulator { design, state, comb, sens }
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

    /// Evaluate combinational processes to a fixpoint by **event-driven
    /// dispatch**: recompute a target only when a signal it reads has
    /// changed. Each target resolves its source-ordered drivers (spec 3.14
    /// later-wins); a change wakes the processes sensitive to it. Equivalent
    /// to the old all-drivers-per-pass fixpoint, but O(affected) per delta.
    fn settle_combinational(&mut self) {
        let comb = std::mem::take(&mut self.comb);
        let sens = std::mem::take(&mut self.sens);
        // Seed every process dirty (a settle may follow an event-block commit).
        let mut queue: Vec<usize> = (0..comb.len()).collect();
        let mut queued = vec![true; comb.len()];
        // Oscillation guard, matching the old MAX_DELTAS-pass bound.
        let mut budget = (MAX_DELTAS * comb.len().max(1)) as u64;

        while let Some(pi) = queue.pop() {
            queued[pi] = false;
            if budget == 0 {
                break;
            }
            budget -= 1;

            let target = comb[pi].0;
            let ti = target.0 as usize;
            // Resolve the target: firing drivers apply in source order.
            let mut val = self.state[ti].current;
            for &di in &comb[pi].1 {
                let d = &self.design.drivers[di];
                if self.cond_true(&d.cond) {
                    val = self.eval(&d.expr);
                }
            }
            let val = mask(val, self.design.signals[ti].width);
            if self.state[ti].current != val {
                self.state[ti].current = val;
                self.state[ti].event = true;
                if let Some(dep) = sens.get(&target) {
                    for &qi in dep {
                        if !queued[qi] {
                            queued[qi] = true;
                            queue.push(qi);
                        }
                    }
                }
            }
        }
        self.comb = comb;
        self.sens = sens;
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

/// The DUT execution engine the test runner drives: the interpreter
/// ([`Simulator`]) or a compiled backend (the JIT, via an adapter). Values
/// cross the boundary as `u128` so wide designs keep full precision; a narrow
/// engine widens on read and truncates on set.
impl<S: Slot> Engine for Simulator<'_, S> {
    fn set(&mut self, sig: SignalId, value: u128) {
        self.set_slot(sig, S::from_u128(value));
    }
    fn read(&self, sig: SignalId) -> u128 {
        self.state[sig.0 as usize].current.to_u128()
    }
    fn settle(&mut self) {
        Simulator::settle(self);
    }
    fn design(&self) -> &Design {
        self.design
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
    run_tests_impl(modules, hier, design, filter, wide_run(slot, design))
}

/// Build an interpreter engine (`Simulator`) of the right slot width.
fn interp_engine(design: &Design, wide: bool) -> Box<dyn Engine + '_> {
    if wide {
        Box::new(Simulator::<u128>::new(design))
    } else {
        Box::new(Simulator::<u64>::new(design))
    }
}

fn wide_run(slot: SlotWidth, design: &Design) -> bool {
    match slot {
        SlotWidth::W64 => false,
        SlotWidth::W128 => true,
        SlotWidth::Auto => needs_wide(design),
    }
}

fn run_tests_impl(
    modules: &[Module],
    hier: &Hierarchy,
    design: &Design,
    filter: Option<&str>,
    wide: bool,
) -> Vec<TestResult> {
    run_tests_with_engine(modules, hier, design, filter, || interp_engine(design, wide))
}

/// Run the `#[test]` entities against a caller-provided execution engine. The
/// factory builds a fresh, reset engine per test (state must not leak between
/// tests). This is how a compiled backend (the JIT) plugs into the runner.
pub fn run_test_traced(
    modules: &[Module],
    hier: &Hierarchy,
    design: &Design,
    filter: Option<&str>,
) -> Option<(TestResult, Vec<Sample>)> {
    run_test_traced_impl(modules, hier, design, filter, needs_wide(design))
}

fn run_test_traced_impl(
    modules: &[Module],
    hier: &Hierarchy,
    design: &Design,
    filter: Option<&str>,
    wide: bool,
) -> Option<(TestResult, Vec<Sample>)> {
    run_test_traced_with_engine(modules, hier, design, filter, || interp_engine(design, wide))
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
    fn after_clock_and_oneshot_delayed_write() {
        // `clk = not clk after 5ns` is a 10ns-period clock; the one-shot
        // `rst = '0' after 12ns` releases reset mid-run (VHDL semantics).
        let results = run(
            "module m;\n\
             entity Ctr { in clk: Clock; in rst: Logic; out n: uint[8]; }\n\
             impl Ctr {\n\
               let v: uint[8] = 0;\n\
               if clk::rising { if rst == '1' { v = 0; } else { v = v + 1; } }\n\
               n = v;\n\
             }\n\
             #[test]\n\
             entity T {}\n\
             impl T {\n\
               let clk: Logic = '0';\n\
               let rst: Logic = '1';\n\
               let n: uint[8];\n\
               let dut = Ctr { .clk, .rst, .n };\n\
               clk = not clk after 5ns;\n\
               rst = '0' after 12ns;\n\
               await 52ns;\n\
               assert!(n == 4, \"rises at 15/25/35/45 count after reset drops at 12\");\n\
             }\n",
        );
        assert!(results[0].passed, "{:?}", results[0].failure);
    }

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
          let rst: Logic;\n\
          let en: Bit;\n\
          let count: uint[8];\n\
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
        let clk = sim.signal("H.clk").unwrap();
        let rst = sim.signal("H.rst").unwrap();
        let en = sim.signal("H.en").unwrap();
        let count = sim.signal("H.count").unwrap();

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
        let clk = sim.signal("H.clk").unwrap();
        let rst = sim.signal("H.rst").unwrap();
        let en = sim.signal("H.en").unwrap();
        let count = sim.signal("H.count").unwrap();
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
    fn combinational_chain_propagates_through_dispatch() {
        // a -> b -> c -> out: event-driven dispatch must wake each stage when
        // its input settles, propagating a change all the way through in one
        // settle. (Drivers are declared out of dependency order on purpose.)
        assert_test_passes(
            "module m;\n\
             entity Chain { in i: uint[8]; out o: uint[8]; }\n\
             impl Chain {\n\
               let a: uint[8];\n\
               let b: uint[8];\n\
               let c: uint[8];\n\
               o = c;\n\
               c = b + 1;\n\
               b = a + 1;\n\
               a = i + 1;\n\
             }\n\
             #[test]\n\
             entity ChainTest {}\n\
             impl ChainTest {\n\
               let i: uint[8] = 10;\n\
               let o: uint[8];\n\
               let dut = Chain { .i, .o };\n\
               wait 1ns;\n\
               assert!(o == 13, \"10 +1 +1 +1 propagates through the chain\");\n\
               i = 20;\n\
               wait 1ns;\n\
               assert!(o == 23, \"a new input re-propagates\");\n\
             }\n",
        );
    }

    #[test]
    fn string_literals_infer_char_arrays() {
        // `using string = Char[]` — an unconstrained array; a string literal
        // supplies the length (Char[5]), assigns per element, and whole-string
        // equality compares element-wise.
        assert_test_passes(
            "module m;\n\
             using string = Char[];\n\
             entity Echo { in s: string[5]; out o: string[5]; }\n\
             impl Echo { o = s; }\n\
             #[test]\n\
             entity StrTest {}\n\
             impl StrTest {\n\
               let s: string = \"hello\";\n\
               let o: string[5];\n\
               let dut = Echo { .s, .o };\n\
               wait 1ns;\n\
               assert!(o == \"hello\", \"echoed string matches\");\n\
               assert!(o != \"world\", \"and differs from another\");\n\
               s = \"world\";\n\
               wait 1ns;\n\
               assert!(o == \"world\", \"reassignment propagates\");\n\
             }\n",
        );
    }

    #[test]
    fn char_literals_read_through_the_context_type() {
        // The same literal syntax means different things by context: a Char
        // signal reads 'A' as its Unicode symbol; Logic keeps reading '1' as
        // a logic level. Symbol equality is intrinsic.
        let results = run(
            "module m;\n\
             entity P { in c: Char; out is_a: Bool; out echo: Char; }\n\
             impl P {\n\
               is_a = c == 'A';\n\
               echo = c;\n\
             }\n\
             #[test]\n\
             entity CharTest {}\n\
             impl CharTest {\n\
               let c: Char = 'A';\n\
               let is_a: Bool;\n\
               let echo: Char;\n\
               let dut = P { .c, .is_a, .echo };\n\
               wait 1ns;\n\
               assert!(is_a, \"'A' == 'A' (symbol equality)\");\n\
               assert!(echo == 'A', \"symbol round-trips\");\n\
               c = '\u{20AC}';\n\
               wait 1ns;\n\
               assert!(echo == '\u{20AC}', \"non-ASCII symbols work (euro sign)\");\n\
               assert!(is_a == false, \"and are distinct from 'A'\");\n\
             }\n",
        );
        assert_eq!(results.len(), 1);
        assert!(results[0].passed, "{:?}", results[0].failure);
    }

    #[test]
    fn ranged_integers_store_in_covering_width() {
        // integer<0..1114111> is 21 bits: a large code point round-trips;
        // integer<0..255> is 8 bits: arithmetic wraps at the range width.
        let results = run(
            "module m;\n\
             using Char = integer<0..1114111>;\n\
             using Byte = integer<0..255>;\n\
             entity P { in c: Char; in b: Byte; out oc: Char; out ob: Byte; }\n\
             impl P {\n\
               oc = c;\n\
               ob = b + 1;\n\
             }\n\
             #[test]\n\
             entity RangedTest {}\n\
             impl RangedTest {\n\
               let c: Char = 128512;\n\
               let b: Byte = 255;\n\
               let oc: Char;\n\
               let ob: Byte;\n\
               let dut = P { .c, .b, .oc, .ob };\n\
               wait 1ns;\n\
               assert!(oc == 128512, \"21-bit code point survives (width > 16)\");\n\
               assert!(ob == 0, \"byte arithmetic wraps at 8 bits\");\n\
             }\n",
        );
        assert_eq!(results.len(), 1);
        assert!(results[0].passed, "{:?}", results[0].failure);
    }

    #[test]
    fn slice_direction_follows_written_order() {
        // w = 0xA5 = 0b1010_0101. Descending `[7..4]` extracts MSB-first
        // (0b1010 = 10); ascending `[4..7]` reverses the bit order
        // (0b0101 = 5); a named range constant works in slice position.
        let results = run(
            "module m;\n\
             const HI: integer = 7;\n\
             const NIB: range = 3..0;\n\
             entity S {\n\
               in w: uint[8];\n\
               out hi_dn: uint[4]; out lo_up: uint[4]; out named: uint[4];\n\
             }\n\
             impl S {\n\
               hi_dn = w[HI..4];\n\
               lo_up = w[4..7];\n\
               named = w[NIB];\n\
             }\n\
             #[test]\n\
             entity SliceTest {}\n\
             impl SliceTest {\n\
               let w: uint[8] = 165;\n\
               let hi_dn: uint[4];\n\
               let lo_up: uint[4];\n\
               let named: uint[4];\n\
               let dut = S { .w, .hi_dn, .lo_up, .named };\n\
               wait 1ns;\n\
               assert!(hi_dn == 10, \"w[7..4] descending is MSB-first\");\n\
               assert!(lo_up == 5, \"w[4..7] ascending reverses bit order\");\n\
               assert!(named == 5, \"named range const slices (w[3..0])\");\n\
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
        let clk = sim.signal("H.clk").unwrap();
        let rst = sim.signal("H.rst").unwrap();
        let en = sim.signal("H.en").unwrap();
        let count = sim.signal("H.count").unwrap();

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
