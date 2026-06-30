//! Waveform / tracing output for siox Phase 1 (spec Stage 9).
//!
//! Records signal changes with hierarchy paths; enum values as symbolic names;
//! struct fields recursively. VCD first, FST later.
//!
//! Acceptance (spec Stage 9): counter waveform shows `clk/rst/en/count`; FSM
//! shows symbolic states or encoded values; struct fields appear as separate
//! trace paths; `::old` is not dumped by default but can be enabled as debug.

use siox_ir::{Design, SignalId};
use std::io::{self, Write};

/// Accumulates value-change records during a simulation run.
#[derive(Default)]
pub struct Trace {
    // TODO(stage-9): scope tree, var ids, (time, signal, value) change list.
}

impl Trace {
    pub fn new(_design: &Design) -> Self {
        Trace::default()
    }

    /// Record a value change at a given time.
    pub fn record(&mut self, _time_fs: u64, _sig: SignalId, _value: u64) {
        // TODO(stage-9): append a change record.
    }

    /// Write the trace as a VCD file.
    pub fn write_vcd<W: Write>(&self, _out: &mut W) -> io::Result<()> {
        // TODO(stage-9): emit $scope/$var headers then $dumpvars + changes.
        todo!("Stage 9: VCD export")
    }
}
