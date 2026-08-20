//! A runtime failure names the source that caused it.
//!
//! A failing `assert!` used to print its message and nothing else, so finding
//! the statement in a large testbench meant reading it. A range violation named
//! the signal, its domain and the offending value, but not where that signal
//! was declared. Neither needs a debugger to fix: the spans exist at emit time
//! and only had to reach the generated C.
//!
//! The location is asserted as `file:line:col` against a source written by the
//! test, so the numbers are checked rather than merely present.

use std::path::PathBuf;
use std::process::Command;

/// Build `src` as a test executable, run it, and return everything it printed.
fn run(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_failloc_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, src).unwrap();
    let bin: PathBuf = dir.join(format!("{name}.bin"));
    let _ = std::fs::remove_file(&bin);
    let built = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .arg("--test")
        .arg(&file)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap();
    assert!(
        bin.exists(),
        "the test executable did not build:\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let ran = Command::new(&bin).output().unwrap();
    String::from_utf8_lossy(&ran.stdout).to_string() + &String::from_utf8_lossy(&ran.stderr)
}

#[test]
fn a_failing_assertion_names_its_line() {
    // The `assert!` is on line 8, indented four spaces, so it starts at
    // column 5.
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let v: unsigned[8] = 1;\n\
               \x20   await 1ns;\n\
               \x20   print!(\"before\");\n\
               \x20   assert!(v == 99, \"v should have been 99\");\n\
               }\n";
    let out = run("assertline", src);
    assert!(
        out.contains("v should have been 99"),
        "the message is still reported, got:\n{out}"
    );
    // The path is however the file was named to the compiler, so only the
    // tail is asserted -- the line and column are the part under test.
    assert!(
        out.contains("--> ") && out.contains("assertline.siox:8:5"),
        "the failing assertion should name its own line, got:\n{out}"
    );
}

#[test]
fn a_range_violation_names_the_declaration() {
    // `c` is declared on line 6 at column 5; it leaves 0..10 on the third edge.
    let src = "module m;\n\
               using std::logic::{Bit};\n\
               entity E { clk: Bit in, y: integer<0..10> out }\n\
               impl E {\n\
               \x20   let c: integer<0..10> = 0;\n\
               \x20   if clk.rising() { c = c + 5; }\n\
               \x20   y = c;\n\
               }\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let clk: Bit = '0';\n\
               \x20   let y: integer<0..10>;\n\
               \x20   let e: E = { .clk = clk, .y = y };\n\
               \x20   clk = not clk after 5ns;\n\
               \x20   await clk.rising(); await 1ns;\n\
               \x20   await clk.rising(); await 1ns;\n\
               \x20   await clk.rising(); await 1ns;\n\
               }\n";
    let out = run("rangeline", src);
    assert!(
        out.contains("left its range 0..10"),
        "the domain and value are still reported, got:\n{out}"
    );
    assert!(
        out.contains("--> ") && out.contains("rangeline.siox:5:5"),
        "a range violation should name the signal's declaration, got:\n{out}"
    );
}

#[test]
fn a_failing_file_read_names_the_declaration() {
    // A `read<T>` that cannot open its file names the `let` that asked for it,
    // the way an assertion names its own statement. The declaration is on
    // line 5 at column 5.
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let text: string = read<string>(\"definitely_absent_fixture.txt\");\n\
               \x20   await 1ns;\n\
               }\n";
    let out = run("ioline", src);
    assert!(
        out.contains("No such file") || out.contains("cannot"),
        "the failure should still say what went wrong, got:\n{out}"
    );
    assert!(
        out.contains("--> ") && out.contains("ioline.siox:5:5"),
        "a failing read should name its declaration, got:\n{out}"
    );
}

#[test]
fn a_failure_shows_the_source_line_with_a_caret() {
    // The location is followed by the line itself and a caret under the
    // column, which is what the compiler's own diagnostics render. The snippet
    // is embedded at compile time, so the executable never reads the source
    // and stays right even if the tree moves on.
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let v: unsigned[8] = 1;\n\
               \x20   await 1ns;\n\
               \x20   assert!(v == 99, \"v should have been 99\");\n\
               }\n";
    let out = run("caret", src);
    // The line number is right-aligned to the gutter, so the row reads " 7 | ".
    let text = out
        .lines()
        .find(|line| line.trim_start().starts_with("7 | "))
        .unwrap_or_else(|| panic!("the failing line should be shown, got:\n{out}"));
    let row = out
        .lines()
        .find(|line| line.contains('^'))
        .unwrap_or_else(|| panic!("a caret row should follow it, got:\n{out}"));
    // The caret sits under the statement's first column, not at the margin.
    let column = row.find('^').unwrap();
    assert_eq!(
        text.as_bytes().get(column).copied(),
        Some(b'a'),
        "the caret should point at `assert`, got:\n{out}"
    );
}

#[test]
fn a_passing_test_reports_no_location() {
    // The location is per-failure, not a banner: a run with nothing wrong must
    // not mention a source position at all.
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let v: unsigned[8] = 1;\n\
               \x20   await 1ns;\n\
               \x20   assert!(v == 1, \"v is one\");\n\
               }\n";
    let out = run("passing", src);
    assert!(
        !out.contains("-->"),
        "a passing run should report no location, got:\n{out}"
    );
}
