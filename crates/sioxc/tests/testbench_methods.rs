//! `recv.method(args)` in testbench stimulus (spec 3.20): the runner inlines
//! the impl method's body, so a struct-typed testbench local can drive a DUT
//! through a method result. Runs the fixture on the JIT via the CLI.
#![cfg(feature = "llvm")]

use std::process::Command;

#[test]
fn testbench_method_call_runs_on_jit() {
    let siox = env!("CARGO_BIN_EXE_sioxc");
    // Run from the repo root so `./std` resolves.
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let fixture = "crates/sioxc/tests/fixtures/method_test.siox";
    let out = Command::new(siox)
        .current_dir(root)
        .args(["test", fixture, "--std", "std"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "sioxc test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("test result: ok"), "testbench did not pass:\n{stdout}");
}
