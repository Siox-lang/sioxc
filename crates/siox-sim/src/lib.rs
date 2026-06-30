//! Event-driven simulator core for siox Phase 1 (spec Stage 7) plus the test
//! runner / assertions (spec Stage 8).
//!
//! Simulator concepts: current value, old value, event flag, delta cycle,
//! driver evaluation, next-state queue, commit phase, wakeup scheduling, and
//! stable-state detection.
//!
//! Delta-cycle loop (spec Stage 7):
//! 1. apply initial values
//! 2. evaluate combinational drivers
//! 3. commit signal changes
//! 4. mark `::event` for changed values
//! 5. wake event-controlled blocks whose conditions may now be true
//! 6. evaluate event-controlled blocks
//! 7. queue next-state updates
//! 8. commit next-state updates
//! 9. repeat delta cycles until stable
//! 10. advance time when requested by stimulus

use siox_diag::DiagnosticSink;
use siox_ir::{Design, SignalId};

/// Per-signal runtime state: current value, previous value, and event flag.
pub struct SignalState {
    pub current: u64,
    pub old: u64,
    pub event: bool,
}

/// Simulation kernel.
pub struct Simulator<'a> {
    design: &'a Design,
    state: Vec<SignalState>,
    /// Simulation time in femtoseconds (resolution TBD in Stage 8).
    time_fs: u64,
}

impl<'a> Simulator<'a> {
    pub fn new(design: &'a Design) -> Self {
        Simulator { design, state: Vec::new(), time_fs: 0 }
    }

    /// Run delta cycles until the design is stable (no pending events).
    pub fn settle(&mut self) {
        // TODO(stage-7): the delta-cycle loop above.
        let _ = (&self.design, &self.state, self.time_fs);
        todo!("Stage 7: delta-cycle settle")
    }

    /// Read a signal's current value.
    pub fn read(&self, _sig: SignalId) -> u64 {
        todo!("Stage 7: signal read")
    }

    /// Advance simulation time, settling at each scheduled wakeup.
    pub fn advance(&mut self, _fs: u64) {
        todo!("Stage 7: time advance")
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
