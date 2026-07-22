//! Positional name-less struct locals (`let p: Pkt = { 3, 4 }`) bind to fields
//! by declaration order, on the JIT and the native test harness.
#![cfg(feature = "llvm")]

use std::process::Command;

const FIXTURE: &str = "tests/fixtures/struct_spread_test.siox";

#[test]
fn struct_spread_locals_run_on_jit() {
    let siox = env!("CARGO_BIN_EXE_sioxc");
    let root = env!("CARGO_MANIFEST_DIR");
    let out = Command::new(siox)
        .current_dir(root)
        .args(["test", FIXTURE, "--std", "std"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "struct spread test failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn struct_spread_locals_run_in_native_harness() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let siox = env!("CARGO_BIN_EXE_sioxc");
    let root = env!("CARGO_MANIFEST_DIR");
    let bin = std::env::temp_dir().join(format!("siox_spread_{}", std::process::id()));
    let build = Command::new(siox)
        .current_dir(root)
        .args(["test", FIXTURE, "--std", "std", "--no-run", "-o"])
        .arg(&bin)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "struct spread native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&bin).status().unwrap();
    let _ = std::fs::remove_file(&bin);
    assert!(run.success(), "struct spread native test failed");
}
