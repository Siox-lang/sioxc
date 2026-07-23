//! Waveform / tracing output for siox Phase 1 (spec Stage 9).
//!
//! Takes the per-time [`Sample`]s recorded by a traced simulation run
//! ([`crate::run::run_test_traced`]) and writes a VCD file: a `$timescale`, one
//! `$scope`/`$var` per signal (grouped by the `Entity.signal` path prefix), then
//! `#time` value-change records. Enum symbolic names and FST are follow-ups.

use std::collections::HashMap;
use std::io::{self, Write};

use crate::ir::Design;
use crate::run::Sample;

/// Write the recorded samples for `design` as a VCD stream.
pub fn write_vcd<W: Write>(out: &mut W, design: &Design, samples: &[Sample]) -> io::Result<()> {
    let ids: Vec<String> = (0..design.signals.len()).map(|i| format!("v{i}")).collect();

    // X/Z metavalue companions (`v$meta`): a `Logic`-vector's per-element
    // discriminant. They aren't dumped as their own vars — instead the vector
    // renders each bit as `x`/`z` where its companion says so.
    let companion_of: Vec<Option<usize>> =
        (0..design.signals.len()).map(|i| design.meta_of.get(&(i as u32)).map(|&c| c as usize)).collect();
    let owner_of: HashMap<usize, usize> =
        design.meta_of.iter().map(|(&o, &c)| (c as usize, o as usize)).collect();
    let is_companion: Vec<bool> = (0..design.signals.len()).map(|i| owner_of.contains_key(&i)).collect();

    // A logic-scalar enum (Bit/ULogic/Logic: every variant a quoted
    // logic character) dumps as a 1-bit VCD scalar with native 0/1/z/x states
    // instead of its raw discriminant — what waveform viewers expect.
    let logic_tables: Vec<Option<Vec<char>>> = design
        .signals
        .iter()
        .map(|s| logic_vcd_table(design.enum_syms.get(s.enum_type.as_deref()?)?))
        .collect();

    // The disc -> VCD-symbol table for X/Z metavalue vectors, from the logic
    // enum's own declaration (not a baked-in `disc 2 = z`) — so the waveform
    // reads its meaning from the same enum table everything else does.
    let meta_table: Vec<char> = ["ULogic", "Logic"]
        .iter()
        .find_map(|n| design.enum_syms.get(*n))
        .and_then(logic_vcd_table)
        .unwrap_or_default();

    // A non-logic enum (an FSM `State`, `Bool`) dumps as a VCD `string` var —
    // the de-facto extension that waveform viewers (GTKWave, Surfer) render as
    // text — so states show as `Idle`/`Run` rather than a bare discriminant.
    // Logic scalars, handled above, keep their native 0/1/z/x.
    let name_tables: Vec<Option<&HashMap<u64, String>>> = design
        .signals
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if logic_tables[i].is_some() {
                return None;
            }
            design.enum_syms.get(s.enum_type.as_deref()?)
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
            if is_companion[i] {
                continue; // merged into its vector's rendering
            }
            let (_, name) = split_path(&design.signals[i].path);
            if name_tables[i].is_some() {
                writeln!(out, "$var string 1 {} {name} $end", ids[i])?;
            } else {
                let w = if logic_tables[i].is_some() { 1 } else { vcd_width(design.signals[i].width) };
                writeln!(out, "$var wire {w} {} {name} $end", ids[i])?;
            }
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
        let mut rendered_meta = std::collections::HashSet::new();
        for (i, v) in changes {
            last[i] = Some(v);
            // A metavalue vector (or its companion changing) re-renders the
            // vector with per-bit `x`/`z`, once per time.
            let owner = if is_companion[i] { owner_of[&i] } else { i };
            if let Some(cid) = companion_of[owner] {
                if rendered_meta.insert(owner) {
                    write_metavalue(
                        out,
                        sample.values[owner],
                        sample.values[cid],
                        design.signals[owner].width,
                        &ids[owner],
                        &meta_table,
                    )?;
                }
                continue;
            }
            if let Some(table) = &logic_tables[i] {
                let ch = table.get(v as usize).copied().unwrap_or('x');
                writeln!(out, "{ch}{}", ids[i])?;
            } else if let Some(names) = name_tables[i] {
                // A `string` value change: `s<symbol> <id>` (unknown
                // discriminants — never expected — fall back to the number).
                match names.get(&(v as u64)) {
                    Some(sym) => writeln!(out, "s{sym} {}", ids[i])?,
                    None => writeln!(out, "s{v} {}", ids[i])?,
                }
            } else {
                write_value(out, v, design.signals[i].width, &ids[i])?;
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

/// Build a discriminant -> VCD-symbol table from a logic enum's own
/// declaration (`disc -> 'X'` / `'Z'` / …), reducing the 9-value `std_ulogic`
/// alphabet to VCD's `0/1/x/z`. The disc->char meaning comes from the enum
/// table; only the `x`/`z`/`0`/`1` targets are the VCD format's fixed alphabet.
fn logic_vcd_table(syms: &HashMap<u64, String>) -> Option<Vec<char>> {
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
}

/// A `Logic`-vector with metavalues: each element's discriminant maps through
/// `table` (from the enum declaration) — an `x`/`z` symbol means a metavalue,
/// otherwise the element takes its plain value bit. MSB-first, VCD binary.
fn write_metavalue<W: Write>(
    out: &mut W,
    value: u128,
    meta: u128,
    width: u32,
    id: &str,
    table: &[char],
) -> io::Result<()> {
    let mut bits = String::with_capacity(width as usize);
    for j in (0..width).rev() {
        let disc = ((meta >> (4 * j)) & 0xF) as usize;
        let sym = table.get(disc).copied().unwrap_or('x');
        bits.push(if sym == 'x' || sym == 'z' {
            sym
        } else {
            char::from(b'0' + ((value >> j) & 1) as u8)
        });
    }
    writeln!(out, "b{bits} {id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Signal;

    fn design() -> Design {
        Design {
            signals: vec![
                Signal { path: "Counter.clk".into(), width: 1, real: false, char: false, range: None, init: 0, enum_type: None },
                Signal { path: "Counter.count".into(), width: 8, real: false, char: false, range: None, init: 0, enum_type: None },
            ],
            drivers: vec![],
            event_blocks: vec![],
            enum_syms: Default::default(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
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
    fn enum_signals_dump_symbolically() {
        // A logic scalar (Bit: '0'/'1') dumps native 0/1; a named enum (State)
        // dumps as a VCD `string` var with its variant names.
        let mut enum_syms: HashMap<String, HashMap<u64, String>> = HashMap::new();
        enum_syms.insert(
            "Bit".into(),
            HashMap::from([(0, "'0'".into()), (1, "'1'".into())]),
        );
        enum_syms.insert(
            "State".into(),
            HashMap::from([(0, "Idle".into()), (1, "Run".into()), (2, "Done".into())]),
        );
        let design = Design {
            signals: vec![
                Signal { path: "M.b".into(), width: 1, real: false, char: false, range: None, init: 0, enum_type: Some("Bit".into()) },
                Signal { path: "M.st".into(), width: 2, real: false, char: false, range: None, init: 0, enum_type: Some("State".into()) },
            ],
            drivers: vec![],
            event_blocks: vec![],
            enum_syms,
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
        };
        let samples = vec![
            Sample { time_fs: 0, values: vec![0, 0] },
            Sample { time_fs: 5, values: vec![1, 1] }, // b -> '1', st -> Run
            Sample { time_fs: 10, values: vec![1, 2] }, // st -> Done
        ];
        let mut buf = Vec::new();
        write_vcd(&mut buf, &design, &samples).unwrap();
        let vcd = String::from_utf8(buf).unwrap();
        assert!(vcd.contains("$var wire 1 v0 b $end"), "Bit is a 1-bit wire");
        assert!(vcd.contains("$var string 1 v1 st $end"), "State is a string var");
        assert!(vcd.contains("0v0"), "Bit dumps native 0/1");
        assert!(vcd.contains("sIdle v1"), "state 0 -> Idle");
        assert!(vcd.contains("sRun v1"), "state 1 -> Run");
        assert!(vcd.contains("sDone v1"), "state 2 -> Done");
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
