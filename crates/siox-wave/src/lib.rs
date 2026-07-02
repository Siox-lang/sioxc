//! Waveform / tracing output for siox Phase 1 (spec Stage 9).
//!
//! Takes the per-time [`Sample`]s recorded by a traced simulation run
//! ([`siox_sim::run_test_traced`]) and writes a VCD file: a `$timescale`, one
//! `$scope`/`$var` per signal (grouped by the `Entity.signal` path prefix), then
//! `#time` value-change records. Enum symbolic names and FST are follow-ups.

use std::io::{self, Write};

use siox_ir::Design;
use siox_sim::Sample;

/// Write the recorded samples for `design` as a VCD stream.
pub fn write_vcd<W: Write>(out: &mut W, design: &Design, samples: &[Sample]) -> io::Result<()> {
    let ids: Vec<String> = (0..design.signals.len()).map(|i| format!("v{i}")).collect();

    writeln!(out, "$timescale 1fs $end")?;

    // Group signals into scopes by the part of the path before the first `.`.
    let mut scopes: Vec<(&str, Vec<usize>)> = Vec::new();
    for (i, s) in design.signals.iter().enumerate() {
        let (scope, _) = split_path(&s.path);
        match scopes.iter_mut().find(|(sc, _)| *sc == scope) {
            Some((_, idxs)) => idxs.push(i),
            None => scopes.push((scope, vec![i])),
        }
    }
    for (scope, idxs) in &scopes {
        writeln!(out, "$scope module {scope} $end")?;
        for &i in idxs {
            let (_, name) = split_path(&design.signals[i].path);
            writeln!(out, "$var wire {} {} {name} $end", vcd_width(design.signals[i].width), ids[i])?;
        }
        writeln!(out, "$upscope $end")?;
    }
    writeln!(out, "$enddefinitions $end")?;

    // Value changes over time. Only emit signals whose value actually changed,
    // and one `#time` marker per distinct time.
    let mut last: Vec<Option<u64>> = vec![None; design.signals.len()];
    let mut cur_time: Option<u64> = None;
    for sample in samples {
        let changes: Vec<(usize, u64)> = sample
            .values
            .iter()
            .enumerate()
            .filter(|(i, &v)| last[*i] != Some(v))
            .map(|(i, &v)| (i, v))
            .collect();
        if changes.is_empty() {
            continue;
        }
        if cur_time != Some(sample.time_fs) {
            writeln!(out, "#{}", sample.time_fs)?;
            cur_time = Some(sample.time_fs);
        }
        for (i, v) in changes {
            last[i] = Some(v);
            write_value(out, v, design.signals[i].width, &ids[i])?;
        }
    }
    Ok(())
}

/// Split `Entity.signal` into `(scope, name)`; a path with no `.` goes under
/// scope `top`.
fn split_path(path: &str) -> (&str, &str) {
    path.split_once('.').unwrap_or(("top", path))
}

/// VCD requires a concrete width; a parametric/unknown `0` is shown as 32 bits.
fn vcd_width(width: u32) -> u32 {
    match width {
        0 => 32,
        w => w,
    }
}

fn write_value<W: Write>(out: &mut W, value: u64, width: u32, id: &str) -> io::Result<()> {
    if width == 1 {
        writeln!(out, "{}{id}", value & 1)
    } else {
        writeln!(out, "b{value:b} {id}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use siox_ir::Signal;

    fn design() -> Design {
        Design {
            signals: vec![
                Signal { path: "Counter.clk".into(), width: 1, real: false },
                Signal { path: "Counter.count".into(), width: 8, real: false },
            ],
            drivers: vec![],
            event_blocks: vec![],
        }
    }

    #[test]
    fn writes_a_valid_vcd() {
        let samples = vec![
            Sample { time_fs: 0, values: vec![0, 0] },
            Sample { time_fs: 5, values: vec![1, 0] }, // clk rises
            Sample { time_fs: 10, values: vec![0, 3] }, // clk falls, count -> 3
        ];
        let mut buf = Vec::new();
        write_vcd(&mut buf, &design(), &samples).unwrap();
        let vcd = String::from_utf8(buf).unwrap();

        assert!(vcd.contains("$timescale 1fs $end"));
        assert!(vcd.contains("$scope module Counter $end"));
        assert!(vcd.contains("$var wire 1 v0 clk $end"));
        assert!(vcd.contains("$var wire 8 v1 count $end"));
        assert!(vcd.contains("$enddefinitions $end"));
        // initial values at #0, the rising edge at #5, count == 3 at #10.
        assert!(vcd.contains("#0\n0v0\nb0 v1"));
        assert!(vcd.contains("#5\n1v0"));
        assert!(vcd.contains("#10\n0v0\nb11 v1"));
    }

    #[test]
    fn unchanged_signals_are_not_re_emitted() {
        let samples = vec![
            Sample { time_fs: 0, values: vec![0, 0] },
            Sample { time_fs: 5, values: vec![0, 0] }, // nothing changed
        ];
        let mut buf = Vec::new();
        write_vcd(&mut buf, &design(), &samples).unwrap();
        let vcd = String::from_utf8(buf).unwrap();
        // Only the #0 sample produces a time marker.
        assert!(vcd.contains("#0"));
        assert!(!vcd.contains("#5"));
    }
}
