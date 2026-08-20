//! A runtime failure names the source that caused it.
//!
//! A failing `assert!` used to print its message and nothing else, so finding
//! the statement in a large testbench meant reading it. A range violation named
//! the signal, its domain and the offending value, but not where that signal
//! was declared. Neither needs a debugger to fix: the spans exist at emit time
//! and only had to reach the generated C.
//!
//! A range violation then moved on from the declaration to the *assignment*
//! that broke the domain, which is the line the reader has to change. The
//! declaration remains the fallback for a signal written by a driver the
//! lowering synthesized rather than read from source.
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
fn a_range_violation_names_the_assignment() {
    // `c` leaves 0..10 on the third edge. The line under test is the
    // assignment on line 6 -- inside the `if`, so at column 23 -- and not the
    // declaration on line 5, which says which domain was left but not what
    // left it.
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
        out.contains("--> ") && out.contains("rangeline.siox:6:23"),
        "a range violation should name the assignment that broke it, got:\n{out}"
    );
    assert!(
        !out.contains("rangeline.siox:5:5"),
        "it should no longer stop at the declaration, got:\n{out}"
    );
}

#[test]
fn a_range_violation_picks_the_assignment_that_broke_it() {
    // Two assignments write the same ranged signal from different lines, and
    // only the second can leave the domain. Naming the signal is not enough to
    // tell them apart -- this is the case the site index exists for.
    //
    // `step` stays inside 0..100 throughout; `level` is written on line 7 by a
    // statement that stays small and on line 8 by one that does not. Event
    // updates all read pre-commit state, so on the fifth edge line 8 stores
    // `74 + 30`.
    let src = "module m;\n\
               using std::logic::{Bit};\n\
               entity E { clk: Bit in }\n\
               impl E {\n\
               \x20   let level: integer<0..100> = 0;\n\
               \x20   let step: integer<0..100> = 0;\n\
               \x20   if clk.rising() { step = level + 7;\n\
               \x20       level = step + 30; }\n\
               }\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let clk: Bit = '0';\n\
               \x20   let e: E = { .clk = clk };\n\
               \x20   clk = not clk after 5ns;\n\
               \x20   for i in 0..6 { await clk.rising(); }\n\
               }\n";
    let out = run("rangepick", src);
    assert!(
        out.contains("`T.e.level` left its range 0..100 (it was 104)"),
        "the offending signal and value should be reported, got:\n{out}"
    );
    assert!(
        out.contains("rangepick.siox:8:9"),
        "the second assignment is the one that broke the domain, got:\n{out}"
    );
    assert!(
        !out.contains("rangepick.siox:7:"),
        "the assignment that stayed in range should not be blamed, got:\n{out}"
    );
    assert!(
        !out.contains("rangepick.siox:5:"),
        "nor the declaration, got:\n{out}"
    );
}

#[test]
fn a_combinational_range_violation_names_its_assignment() {
    // The clocked path and the combinational path pin the statement being
    // lowered in different places, so a driver needs its own case: a next-state
    // update reaching its line says nothing about whether a driver does.
    //
    // `t` leaves 0..10 in the driver on line 6, at column 5.
    let src = "module m;\n\
               using std::logic::{Bit};\n\
               entity E { a: integer<0..10> in, y: integer<0..10> out }\n\
               impl E {\n\
               \x20   let t: integer<0..10> = 0;\n\
               \x20   t = a + 5;\n\
               \x20   y = t;\n\
               }\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let a: integer<0..10> = 0;\n\
               \x20   let y: integer<0..10>;\n\
               \x20   let e: E = { .a = a, .y = y };\n\
               \x20   a = 8;\n\
               \x20   await 1ns;\n\
               }\n";
    let out = run("rangecomb", src);
    assert!(
        out.contains("left its range 0..10 (it was 13)"),
        "the offending value should be reported, got:\n{out}"
    );
    assert!(
        out.contains("rangecomb.siox:6:5"),
        "a combinational range violation should name its driver, got:\n{out}"
    );
}

#[test]
fn one_statement_shared_by_instances_names_the_instance_that_broke_it() {
    // A generic entity's body lowers once per instance, so the same statement
    // produces a driver for each. The table folds them to one site -- the line
    // is a source fact, not a per-instance one -- and it is the *signal path*
    // that says which instance went wrong. This is the dedup case the unit test
    // asserts on the table, seen from the outside.
    //
    // `K = 1` stays inside 0..100; `K = 3` does not.
    let src = "module m;\n\
               using std::logic::{Bit};\n\
               entity Cell<K: integer> { clk: Bit in, y: integer<0..100> out }\n\
               impl<K: integer> Cell<K> {\n\
               \x20   let v: integer<0..100> = 0;\n\
               \x20   if clk.rising() {\n\
               \x20       v = v + K * 30;\n\
               \x20   }\n\
               \x20   y = v;\n\
               }\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let clk: Bit = '0';\n\
               \x20   let y0: integer<0..100>;\n\
               \x20   let y1: integer<0..100>;\n\
               \x20   let c0: Cell<K = 1> = { .clk = clk, .y = y0 };\n\
               \x20   let c1: Cell<K = 3> = { .clk = clk, .y = y1 };\n\
               \x20   clk = not clk after 5ns;\n\
               \x20   for i in 0..4 { await clk.rising(); }\n\
               }\n";
    let out = run("shared", src);
    assert!(
        out.contains("`T.c1.v` left its range 0..100"),
        "the instance that broke it should be named by its path, got:\n{out}"
    );
    assert!(
        out.contains("shared.siox:7:9"),
        "and the shared statement by its one line, got:\n{out}"
    );
}

#[test]
fn an_external_write_falls_back_to_the_declaration() {
    // The fallback, end to end. A value pushed in from the testbench reaches
    // the port through no assignment in the design, so there is no line to
    // blame and site 0 is latched -- and the declaration, which the anchor
    // otherwise replaces, is what remains to say. Here that is the port itself,
    // on line 2 at column 12.
    //
    // Reaching this needs a *runtime* value: a constant out of range is
    // rejected by the frontend long before the design runs.
    let src = "module m;\n\
               entity E { a: integer<0..10> in, y: integer<0..10> out }\n\
               impl E {\n\
               \x20   y = a;\n\
               }\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let a: integer<0..10> = 0;\n\
               \x20   let y: integer<0..10>;\n\
               \x20   let e: E = { .a = a, .y = y };\n\
               \x20   for i in 0..3 {\n\
               \x20       a = a + 4;\n\
               \x20       await 1ns;\n\
               \x20   }\n\
               }\n";
    let out = run("extwrite", src);
    assert!(
        out.contains("left its range 0..10 (it was 12)"),
        "an external write out of range should still be caught, got:\n{out}"
    );
    assert!(
        out.contains("extwrite.siox:2:12"),
        "with no assignment to blame it should name the declaration, got:\n{out}"
    );
}

#[test]
fn an_overwritten_assignment_is_not_range_checked() {
    // A driver that a later unconditional one replaces never reaches the
    // signal, so its value cannot break the signal's domain. The range check
    // ran per driver regardless, and reported a value the design never held --
    // pointing, once assignments carried their line, at the very statement
    // W-P014 had just called dead.
    //
    // `t` is 2 throughout. 13 is computed and selected away.
    let src = "module m;\n\
               entity E { a: integer<0..10> in, y: integer<0..10> out }\n\
               impl E {\n\
               \x20   let t: integer<0..10> = 0;\n\
               \x20   t = a + 5;\n\
               \x20   t = 2;\n\
               \x20   y = t;\n\
               }\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let a: integer<0..10> = 8;\n\
               \x20   let y: integer<0..10>;\n\
               \x20   let e: E = { .a = a, .y = y };\n\
               \x20   await 1ns;\n\
               \x20   assert!(y == 2, \"the later assignment wins\");\n\
               }\n";
    let out = run("deadcomb", src);
    assert!(
        !out.contains("left its range"),
        "a value that never reached the signal should not fail it, got:\n{out}"
    );
    assert!(
        out.contains("test result: ok"),
        "the design is sound and the test should pass, got:\n{out}"
    );
}

#[test]
fn an_overwritten_clocked_update_is_not_range_checked() {
    // The same, in a clocked block: updates all stage from pre-commit state and
    // commit in order, so a later unconditional write to the same target under
    // the same block condition subsumes the earlier one.
    let src = "module m;\n\
               using std::logic::{Bit};\n\
               entity E { clk: Bit in, a: integer<0..10> in, y: integer<0..10> out }\n\
               impl E {\n\
               \x20   let t: integer<0..10> = 0;\n\
               \x20   if clk.rising() {\n\
               \x20       t = a + 5;\n\
               \x20       t = 2;\n\
               \x20   }\n\
               \x20   y = t;\n\
               }\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let clk: Bit = '0';\n\
               \x20   let a: integer<0..10> = 8;\n\
               \x20   let y: integer<0..10>;\n\
               \x20   let e: E = { .clk = clk, .a = a, .y = y };\n\
               \x20   clk = not clk after 5ns;\n\
               \x20   await clk.rising(); await 1ns;\n\
               \x20   assert!(y == 2, \"the later update wins\");\n\
               }\n";
    let out = run("deadseq", src);
    assert!(
        !out.contains("left its range"),
        "a staged value that is committed over should not fail, got:\n{out}"
    );
    assert!(
        out.contains("test result: ok"),
        "the design is sound and the test should pass, got:\n{out}"
    );
}

#[test]
fn a_conditional_assignment_before_a_default_is_still_checked() {
    // The counterpart the fix must not break. Dropping *every* driver before
    // the last unconditional one is right; dropping a live one would silently
    // stop checking real violations, which is the failure mode that matters
    // most here -- a range check that never fires looks exactly like a design
    // that never misbehaves.
    //
    // Here the overriding write comes first, so the later conditional one is
    // live and does break the domain on line 6.
    let src = "module m;\n\
               entity E { a: integer<0..10> in, y: integer<0..10> out }\n\
               impl E {\n\
               \x20   let t: integer<0..10> = 0;\n\
               \x20   t = 2;\n\
               \x20   if a > 3 { t = a + 5; }\n\
               \x20   y = t;\n\
               }\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let a: integer<0..10> = 8;\n\
               \x20   let y: integer<0..10>;\n\
               \x20   let e: E = { .a = a, .y = y };\n\
               \x20   await 1ns;\n\
               }\n";
    let out = run("livecond", src);
    assert!(
        out.contains("left its range 0..10 (it was 13)"),
        "a live conditional driver must still be checked, got:\n{out}"
    );
    assert!(
        out.contains("livecond.siox:6:16"),
        "and named at its own assignment, got:\n{out}"
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
