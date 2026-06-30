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

use siox_diag::DiagnosticSink;
use siox_elab::Hierarchy;

/// A flattened design ready to simulate: signals, drivers, and event blocks.
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
    /// Hierarchical path, e.g. `top.dut.count`.
    pub path: String,
    pub width: u32,
}

/// A combinational driver: `signal = expr` under `cond` (spec 3.14 source
/// order override is resolved during lowering into a priority chain).
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
/// them directly.
#[derive(Clone, Debug)]
pub enum Expr {
    Const(u64),
    Logic(char),
    Current(SignalId),
    Old(SignalId),
    Event(SignalId),
    // TODO(stage-6): Unary, Binary, Index/Slice, Concat, Match, Mux chain.
}

/// Lower the elaborated hierarchy into simulation IR.
pub fn lower(_hier: &Hierarchy, _sink: &mut DiagnosticSink) -> Design {
    // TODO(stage-6): walk impl bodies, split combinational vs. event blocks,
    // lower system attributes, resolve method calls.
    todo!("Stage 6: IR lowering")
}

impl Design {
    /// Render normalized IR (backs `siox ir`).
    pub fn to_ir_string(&self) -> String {
        // TODO(stage-6): print drivers and event blocks.
        String::new()
    }
}
