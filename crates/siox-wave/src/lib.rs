//! Waveform / tracing output for siox Phase 1 (spec Stage 9).
//!
//! Takes the per-time [`Sample`]s recorded by a traced simulation run
//! ([`siox_run::run_test_traced`]) and writes a VCD file: a `$timescale`, one
//! `$scope`/`$var` per signal (grouped by the `Entity.signal` path prefix), then
//! `#time` value-change records. Enum symbolic names and FST are follow-ups.

use std::io::{self, Write};

use siox_ir::Design;
use siox_run::Sample;

/// Write the recorded samples for `design` as a VCD stream.
pub fn write_vcd<W: Write>(out: &mut W, design: &Design, samples: &[Sample]) -> io::Result<()> {
    let ids: Vec<String> = (0..design.signals.len()).map(|i| format!("v{i}")).collect();

    // A logic-scalar enum (Bit/ULogic/Logic/Clock: every variant a quoted
    // logic character) dumps as a 1-bit VCD scalar with native 0/1/z/x states
    // instead of its raw discriminant — what waveform viewers expect.
    let logic_tables: Vec<Option<Vec<char>>> = design
        .signals
        .iter()
        .map(|s| {
            let syms = design.enum_syms.get(s.enum_type.as_deref()?)?;
            let mut table = vec!['x'; syms.keys().max().map(|&m| m as usize + 1)?];
            for (&d, sym) in syms {
                let ch = sym.strip_prefix('\'')?.strip_suffix('\'')?.chars().next()?;
                table[d as usize] = match ch {
                    '0' | 'L' => '0',
                    '1' | 'H' => '1',
                    'Z' => 'z',
                    'X' | 'U' | 'W' | '-' => 'x',
                    _ => return None,
                };
            }
            Some(table)
        })
        .collect();

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
            let w = if logic_tables[i].is_some() { 1 } else { vcd_width(design.signals[i].width) };
            writeln!(out, "$var wire {w} {} {name} $end", ids[i])?;
        }
        writeln!(out, "$upscope $end")?;
    }
    writeln!(out, "$enddefinitions $end")?;

    // Value changes over time. Only emit signals whose value actually changed,
    // and one `#time` marker per distinct time.
    let mut last: Vec<Option<u128>> = vec![None; design.signals.len()];
    let mut cur_time: Option<u64> = None;
    for sample in samples {
        let changes: Vec<(usize, u128)> = sample
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
            match &logic_tables[i] {
                Some(table) => {
                    let ch = table.get(v as usize).copied().unwrap_or('x');
                    writeln!(out, "{ch}{}", ids[i])?;
                }
                None => write_value(out, v, design.signals[i].width, &ids[i])?,
            }
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

fn write_value<W: Write>(out: &mut W, value: u128, width: u32, id: &str) -> io::Result<()> {
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
                Signal { path: "Counter.clk".into(), width: 1, real: false, char: false, range: None, init: 0, enum_type: None },
                Signal { path: "Counter.count".into(), width: 8, real: false, char: false, range: None, init: 0, enum_type: None },
            ],
            drivers: vec![],
            event_blocks: vec![],
            enum_syms: Default::default(),
            base_dir: Default::default(),
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
