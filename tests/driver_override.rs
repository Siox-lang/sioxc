//! Spec 3.14 override, in both engines' terms.
//!
//! Within one driver context a later assignment overrides an earlier one, so a
//! driver that a later *unconditional* one replaces never reaches the signal.
//! The emitter skips those, which is what stops a value the design never held
//! from being range-checked (see `runtime_failure_location`).
//!
//! The risk in that skip is pruning one driver too many. A live driver that
//! stops being emitted is close to invisible: the signal quietly keeps a stale
//! value, and a range check that never fires looks exactly like a design that
//! never misbehaves. So the cases asserted here are the ones where the
//! surviving driver has to *do* something — the default that holds when a later
//! conditional override does not fire.

use std::path::PathBuf;
use std::process::Command;

/// Build `src` as a test executable, run it, and return everything it printed.
fn run(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_override_{}", std::process::id()));
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
fn a_combinational_default_holds_when_its_override_does_not_fire() {
    // `t = 2` is the default and `if a > 3 { t = 9 }` overrides it. Both
    // branches are exercised in one run, because only the untaken branch
    // proves the default driver is still emitted.
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               entity E { a: unsigned[8] in, y: unsigned[8] out }\n\
               impl E {\n\
               \x20   let t: unsigned[8] = 0;\n\
               \x20   t = 2;\n\
               \x20   if a > 3 { t = 9; }\n\
               \x20   y = t;\n\
               }\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let a: unsigned[8] = 1;\n\
               \x20   let y: unsigned[8];\n\
               \x20   let e: E = { .a = a, .y = y };\n\
               \x20   await 1ns;\n\
               \x20   assert!(y == 2, \"the default holds while the override is false\");\n\
               \x20   a = 8;\n\
               \x20   await 1ns;\n\
               \x20   assert!(y == 9, \"the override wins once its condition is true\");\n\
               }\n";
    let out = run("combdefault", src);
    assert!(
        out.contains("test result: ok"),
        "both branches should hold, got:\n{out}"
    );
}

#[test]
fn a_clocked_default_holds_when_its_override_does_not_fire() {
    // The same shape in a clocked block. Updates stage from pre-commit state
    // and commit in order, so `t = 2` must still be staged even though a later
    // update in the block writes `t` -- that one is conditional, and subsumes
    // nothing.
    let src = "module m;\n\
               using std::logic::{Bit};\n\
               using std::bits::{unsigned};\n\
               entity E { clk: Bit in, a: unsigned[8] in, y: unsigned[8] out }\n\
               impl E {\n\
               \x20   let t: unsigned[8] = 7;\n\
               \x20   if clk.rising() {\n\
               \x20       t = 2;\n\
               \x20       if a > 3 { t = 9; }\n\
               \x20   }\n\
               \x20   y = t;\n\
               }\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let clk: Bit = '0';\n\
               \x20   let a: unsigned[8] = 1;\n\
               \x20   let y: unsigned[8];\n\
               \x20   let e: E = { .clk = clk, .a = a, .y = y };\n\
               \x20   clk = not clk after 5ns;\n\
               \x20   await clk.rising(); await 1ns;\n\
               \x20   assert!(y == 2, \"the default is committed, not the initial 7\");\n\
               \x20   a = 8;\n\
               \x20   await clk.rising(); await 1ns;\n\
               \x20   assert!(y == 9, \"the override wins once its condition is true\");\n\
               }\n";
    let out = run("seqdefault", src);
    assert!(
        out.contains("test result: ok"),
        "both branches should hold, got:\n{out}"
    );
}

#[test]
fn a_replaced_driver_leaves_no_trace_in_the_value() {
    // The pruned case, checked on the value rather than on a diagnostic: the
    // earlier driver is not merely unreported, it is gone.
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               entity E { a: unsigned[8] in, y: unsigned[8] out }\n\
               impl E {\n\
               \x20   let t: unsigned[8] = 0;\n\
               \x20   t = a + 5;\n\
               \x20   t = 2;\n\
               \x20   y = t;\n\
               }\n\
               #[test] entity T {}\n\
               impl T {\n\
               \x20   let a: unsigned[8] = 200;\n\
               \x20   let y: unsigned[8];\n\
               \x20   let e: E = { .a = a, .y = y };\n\
               \x20   await 1ns;\n\
               \x20   assert!(y == 2, \"the replaced driver contributes nothing\");\n\
               }\n";
    let out = run("replaced", src);
    assert!(
        out.contains("test result: ok"),
        "the later unconditional driver wins outright, got:\n{out}"
    );
}
