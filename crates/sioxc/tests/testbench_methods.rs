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

#[test]
fn testbench_method_call_runs_native() {
    if std::process::Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let siox = env!("CARGO_BIN_EXE_sioxc");
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let fixture = "crates/sioxc/tests/fixtures/method_test.siox";
    let bin = std::env::temp_dir().join(format!("siox_method_{}", std::process::id()));
    // Build the standalone native test binary (struct-local + method inline).
    let build = Command::new(siox)
        .current_dir(root)
        .args(["test", fixture, "--std", "std", "--no-run", "-o", bin.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "native build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    // The binary runs the testbench and exits 0 on PASS.
    let run = Command::new(&bin).status().unwrap();
    assert!(run.success(), "native simulator returned {:?}", run.code());
    let _ = std::fs::remove_file(&bin);
}
