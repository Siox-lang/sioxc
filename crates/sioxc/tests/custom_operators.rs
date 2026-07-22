//! End-to-end coverage for attributed custom operators in the JIT and native
//! test harness paths.

use std::process::Command;

const FIXTURE: &str = "crates/sioxc/tests/fixtures/custom_operator_test.siox";

#[test]
fn custom_operators_run_on_jit() {
    let siox = env!("CARGO_BIN_EXE_sioxc");
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let out = Command::new(siox)
        .current_dir(root)
        .args(["test", FIXTURE, "--std", "std"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "custom operator test failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn custom_operators_run_in_native_harness() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let siox = env!("CARGO_BIN_EXE_sioxc");
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let bin = std::env::temp_dir().join(format!("siox_custom_ops_{}", std::process::id()));
    let build = Command::new(siox)
        .current_dir(root)
        .args(["test", FIXTURE, "--std", "std", "--no-run", "-o"])
        .arg(&bin)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "custom operator native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&bin).status().unwrap();
    let _ = std::fs::remove_file(&bin);
    assert!(run.success(), "custom operator native test failed");
}
