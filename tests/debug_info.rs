//! `-g` makes a test executable a debugger can follow in `.siox` terms.
//!
//! The generated C is attributed back to the statement that produced it with
//! `#line`, and compiled `-g -O0`. Clang turns that into DWARF naming the
//! `.siox` file, so a debugger sets breakpoints, steps, and shows source
//! without knowing anything about siox.
//!
//! The default build is asserted unchanged: no debug sections, still
//! optimized, because simulation throughput matters for long runs.
//!
//! The gdb-backed cases are skipped when gdb is absent rather than failing, so
//! the suite still runs on a machine without it.

use std::path::{Path, PathBuf};
use std::process::Command;

const SRC: &str = "module m;\n\
                   using std::bits::{unsigned};\n\
                   using std::logic::{Bit};\n\
                   entity Counter { clk: Bit in, y: unsigned[8] out }\n\
                   impl Counter {\n\
                   \x20   let n: unsigned[8] = 0;\n\
                   \x20   if clk.rising() { n = n + 1; }\n\
                   \x20   y = n;\n\
                   }\n\
                   #[test] entity T {}\n\
                   impl T {\n\
                   \x20   let clk: Bit = '0';\n\
                   \x20   let y: unsigned[8];\n\
                   \x20   let c: Counter = { .clk = clk, .y = y };\n\
                   \x20   clk = not clk after 5ns;\n\
                   \x20   await clk.rising(); await 1ns;\n\
                   \x20   await clk.rising(); await 1ns;\n\
                   \x20   assert!(y == 2, \"two edges give two\");\n\
                   }\n";

/// Build `SRC` as a test executable, with or without `-g`.
fn build(name: &str, debug: bool) -> PathBuf {
    build_src(name, debug, SRC)
}

fn build_src(name: &str, debug: bool, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("siox_dwarf_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, src).unwrap();
    let bin = dir.join(format!("{name}.bin"));
    let _ = std::fs::remove_file(&bin);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    cmd.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .arg("--test");
    if debug {
        cmd.arg("-g");
    }
    let out = cmd.arg(&file).arg("-o").arg(&bin).output().unwrap();
    assert!(
        bin.exists(),
        "build failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    bin
}

fn has_debug_sections(bin: &Path) -> bool {
    let out = Command::new("readelf").arg("-S").arg(bin).output();
    match out {
        Ok(out) => String::from_utf8_lossy(&out.stdout).contains("debug"),
        // No readelf: fall back to the presence of the string in the binary.
        Err(_) => std::fs::read(bin)
            .map(|bytes| bytes.windows(13).any(|w| w == b".debug_info\0\0"))
            .unwrap_or(false),
    }
}

fn gdb() -> Option<&'static str> {
    Command::new("gdb")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| "gdb")
}

#[test]
fn a_debug_build_carries_debug_info() {
    assert!(
        has_debug_sections(&build("dbg", true)),
        "`-g` should produce debug sections"
    );
}

#[test]
fn the_default_build_does_not() {
    assert!(
        !has_debug_sections(&build("plain", false)),
        "the ordinary build must stay free of debug info"
    );
}

#[test]
fn a_debug_build_is_unoptimized() {
    // Optimization reorders and merges the code the `#line` directives point
    // at, so stepping degrades exactly where it is most wanted. The flags are
    // recorded in DWARF, so this is asked of the binary rather than assumed.
    let bin = build("opt", true);
    let recorded = String::from_utf8_lossy(&std::fs::read(&bin).unwrap()).to_string();
    assert!(
        recorded.contains("-O0"),
        "a debug build should be compiled unoptimized"
    );
    assert!(
        !recorded.contains("-O2 -lm"),
        "and not also carry the optimized flags"
    );
}

#[test]
fn a_debugger_breaks_on_a_siox_line() {
    let Some(gdb) = gdb() else {
        eprintln!("gdb not installed; skipping");
        return;
    };
    let bin = build("brk", true);
    // Line 18 is the `assert!`, which is reached after two edges.
    let out = Command::new(gdb)
        .args([
            "-batch",
            "-ex",
            "break brk.siox:18",
            "-ex",
            "run",
            "-ex",
            "list",
        ])
        .arg(&bin)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("brk.siox:18") || text.contains("brk.siox, line 18"),
        "the breakpoint should resolve to the siox line, got:\n{text}"
    );
    // The debugger shows the siox source, not the generated C.
    assert!(
        text.contains("two edges give two"),
        "stopping there should display the siox source, got:\n{text}"
    );
}

#[test]
fn a_debug_build_carries_the_signal_name_table() {
    // A hardware signal is not a variable -- it lives behind `sx_read`, indexed
    // by `SignalId` -- so printing one by its siox path needs this mapping.
    let bin = build("names", true);
    let bytes = std::fs::read(&bin).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("T.c.n"),
        "the table should carry hierarchical signal paths"
    );
}

#[test]
fn the_default_build_carries_no_signal_table() {
    // It is debug-only: an ordinary build should not grow the table.
    let bin = build("notable", false);
    let out = Command::new("nm").arg(&bin).output();
    if let Ok(out) = out {
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("sx_signal_names"),
            "the ordinary build must not carry the signal table"
        );
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("sx_dbg_print"),
            "nor the debug accessors that read it"
        );
    }
}

#[test]
fn the_binary_reads_a_signal_by_siox_name() {
    let Some(gdb) = gdb() else {
        eprintln!("gdb not installed; skipping");
        return;
    };
    // The lookup is compiled into the binary, so there is nothing to source.
    // `-g` is the only prerequisite.
    let bin = build("sig", true);
    let out = Command::new(gdb)
        .args([
            "-batch",
            "-ex",
            "break sig.siox:18",
            "-ex",
            "run",
            // A trailing path resolves without naming the root.
            "-ex",
            r#"call sx_dbg_print("c.n")"#,
            // And the value is usable in an expression, not just printed.
            "-ex",
            r#"print sx_dbg_get("c.n") + 1"#,
        ])
        .arg(&bin)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    // Two edges have run, so the counter holds 2.
    assert!(
        text.contains("T.c.n = 2"),
        "the binary should read the signal by its siox path, got:\n{text}"
    );
    assert!(
        text.contains("= 3"),
        "`sx_dbg_get` should yield a value an expression can use, got:\n{text}"
    );
}

#[test]
fn listing_matches_every_signal_under_a_path() {
    let Some(gdb) = gdb() else {
        eprintln!("gdb not installed; skipping");
        return;
    };
    let bin = build("list", true);
    let out = Command::new(gdb)
        .args([
            "-batch",
            "-ex",
            "break list.siox:18",
            "-ex",
            "run",
            "-ex",
            r#"call sx_dbg_list("c.")"#,
        ])
        .arg(&bin)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    for signal in ["T.c.n", "T.c.y", "T.c.clk"] {
        assert!(
            text.contains(signal),
            "`{signal}` should be listed, got:\n{text}"
        );
    }
}

#[test]
fn an_unknown_or_ambiguous_path_says_so() {
    let Some(gdb) = gdb() else {
        eprintln!("gdb not installed; skipping");
        return;
    };
    // Silence would be the worst answer here: a reader who mistypes a path
    // would read the absence of output as "the signal is zero".
    //
    // Ambiguity needs two instances of one entity. A testbench's own locals
    // are not hardware signals and never reach the table, so in a design with
    // a single DUT every leaf name is unique and the shared fixture cannot
    // produce the case.
    let two = "module m;\n\
               using std::bits::{unsigned};\n\
               using std::logic::{Bit};\n\
               entity Counter { clk: Bit in, y: unsigned[8] out }\n\
               impl Counter {\n\
               \x20   let n: unsigned[8] = 0;\n\
               \x20   if clk.rising() { n = n + 1; }\n\
               \x20   y = n;\n\
               }\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let clk: Bit = '0';\n\
               \x20   let y1: unsigned[8];\n\
               \x20   let y2: unsigned[8];\n\
               \x20   let a: Counter = { .clk = clk, .y = y1 };\n\
               \x20   let b: Counter = { .clk = clk, .y = y2 };\n\
               \x20   clk = not clk after 5ns;\n\
               \x20   await clk.rising(); await 1ns;\n\
               \x20   assert!(y1 == 1, \"one edge gives one\");\n\
               }\n";
    let bin = build_src("miss", true, two);
    let out = Command::new(gdb)
        .args([
            "-batch",
            "-ex",
            "break miss.siox:19",
            "-ex",
            "run",
            "-ex",
            r#"call sx_dbg_print("definitely_absent")"#,
            // `y` is both `T.a.y` and `T.b.y`, so a bare leaf is ambiguous.
            "-ex",
            r#"call sx_dbg_print("y")"#,
            // Naming the instance disambiguates it.
            "-ex",
            r#"call sx_dbg_print("a.y")"#,
        ])
        .arg(&bin)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("no signal matching `definitely_absent`"),
        "an unknown path should be reported, got:\n{text}"
    );
    assert!(
        text.contains("matches more than one signal"),
        "an ambiguous path should be reported, got:\n{text}"
    );
    assert!(
        text.contains("T.a.y = 1"),
        "naming the instance should resolve it, got:\n{text}"
    );
}

#[test]
fn a_trailing_match_stops_at_a_path_separator() {
    let Some(gdb) = gdb() else {
        eprintln!("gdb not installed; skipping");
        return;
    };
    // Leaving the root off is a substring match anchored at a `.`, not a bare
    // suffix test. `en` ends with `n`, so an unanchored match would make the
    // path `n` ambiguous and refuse to print the signal the reader asked for.
    let near = "module m;\n\
                using std::bits::{unsigned};\n\
                using std::logic::{Bit};\n\
                entity Counter { clk: Bit in, y: unsigned[8] out }\n\
                impl Counter {\n\
                \x20   let n: unsigned[8] = 0;\n\
                \x20   let en: unsigned[8] = 1;\n\
                \x20   if clk.rising() { n = n + en; }\n\
                \x20   y = n;\n\
                }\n\
                #[test] entity T {}\n\
                impl T {\n\
                \x20   let clk: Bit = '0';\n\
                \x20   let y: unsigned[8];\n\
                \x20   let c: Counter = { .clk = clk, .y = y };\n\
                \x20   clk = not clk after 5ns;\n\
                \x20   await clk.rising(); await 1ns;\n\
                \x20   assert!(y == 1, \"one edge adds one\");\n\
                }\n";
    let bin = build_src("anchor", true, near);
    let out = Command::new(gdb)
        .args([
            "-batch",
            "-ex",
            "break anchor.siox:19",
            "-ex",
            "run",
            "-ex",
            r#"call sx_dbg_print("n")"#,
        ])
        .arg(&bin)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("T.c.n = 1"),
        "`n` should resolve to `T.c.n`, not collide with `T.c.en`, got:\n{text}"
    );
    assert!(
        !text.contains("matches more than one"),
        "`T.c.en` is not a match for `n`, got:\n{text}"
    );
}

#[test]
fn a_value_wider_than_a_word_is_shown_whole() {
    let Some(gdb) = gdb() else {
        eprintln!("gdb not installed; skipping");
        return;
    };
    // `sx_read` hands back one machine word. Without the width table a 128-bit
    // signal printed its low word and looked like a small number rather than a
    // truncation -- here `0x1_0000_0000_0000_0007` read as plain `7`.
    let wide = "module m;\n\
                using std::bits::{unsigned};\n\
                using std::logic::{Bit};\n\
                entity Big { clk: Bit in, seed: unsigned[128] in, y: unsigned[128] out }\n\
                impl Big {\n\
                \x20   let acc: unsigned[128] = 0;\n\
                \x20   if clk.rising() { acc = acc + seed; }\n\
                \x20   y = acc;\n\
                }\n\
                #[test] entity T {}\n\
                impl T {\n\
                \x20   let clk: Bit = '0';\n\
                \x20   let seed: unsigned[128] = 0x1_0000_0000_0000_0007;\n\
                \x20   let y: unsigned[128];\n\
                \x20   let b: Big = { .clk = clk, .seed = seed, .y = y };\n\
                \x20   clk = not clk after 5ns;\n\
                \x20   await clk.rising(); await 1ns;\n\
                \x20   assert!(b.y == seed, \"one edge adds the seed once\");\n\
                }\n";
    let bin = build_src("bits", true, wide);
    let out = Command::new(gdb)
        .args([
            "-batch",
            "-ex",
            "break bits.siox:18",
            "-ex",
            "run",
            "-ex",
            r#"call sx_dbg_print("b.acc")"#,
            "-ex",
            r#"print sx_dbg_get("b.acc")"#,
        ])
        .arg(&bin)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("T.b.acc = 0x10000000000000007"),
        "a wide value should print whole, in hex, got:\n{text}"
    );
    // And the one-word accessor must admit what it returned rather than hand
    // back `7` as though that were the value.
    assert!(
        text.contains("is 128 bits; this is its low word"),
        "`sx_dbg_get` should say it truncated, got:\n{text}"
    );
}

#[test]
fn a_backtrace_names_the_siox_source() {
    let Some(gdb) = gdb() else {
        eprintln!("gdb not installed; skipping");
        return;
    };
    let bin = build("bt", true);
    let out = Command::new(gdb)
        .args([
            "-batch",
            "-ex",
            "break bt.siox:18",
            "-ex",
            "run",
            "-ex",
            "bt 1",
        ])
        .arg(&bin)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("bt.siox:18"),
        "a backtrace should name the siox file and line, got:\n{text}"
    );
}
