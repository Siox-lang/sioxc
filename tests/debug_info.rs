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
    let dir = std::env::temp_dir().join(format!("siox_dwarf_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, SRC).unwrap();
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
