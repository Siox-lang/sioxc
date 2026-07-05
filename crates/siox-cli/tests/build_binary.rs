//! `siox build` produces a runnable native simulator binary (stage B5.1).
//! Only meaningful with the `llvm` feature + a clang toolchain.
#![cfg(feature = "llvm")]

use std::process::Command;

#[test]
fn test_no_run_builds_a_runnable_binary() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let siox = env!("CARGO_BIN_EXE_siox");
    let out = std::env::temp_dir().join(format!("siox_counter_{}", std::process::id()));

    // Build from the repo root so `./std` resolves.
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let status = Command::new(siox)
        .current_dir(root)
        .args(["test", "examples/counter_test.siox", "--no-run", "-o", out.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success(), "siox test --no-run failed");
    assert!(out.exists(), "no binary produced");

    // The binary runs the testbench and exits 0 on PASS.
    let run = Command::new(&out).status().unwrap();
    assert!(run.success(), "native simulator returned {:?}", run.code());
    let _ = std::fs::remove_file(&out);
}
