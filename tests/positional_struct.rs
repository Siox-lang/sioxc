//! Positional name-less struct locals (`let p: Pkt = { 3, 4 }`) bind to fields
//! by declaration order, on the JIT and the native test harness.

use std::process::Command;

const FIXTURE: &str = "tests/fixtures/positional_struct_test.siox";

#[test]
fn positional_struct_locals_run_on_jit() {
    let siox = env!("CARGO_BIN_EXE_sioxc");
    let root = env!("CARGO_MANIFEST_DIR");
    let out = Command::new(siox)
        .current_dir(root)
        .args(["test", FIXTURE, "--std", "std"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "positional struct test failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn positional_struct_locals_run_in_native_harness() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let siox = env!("CARGO_BIN_EXE_sioxc");
    let root = env!("CARGO_MANIFEST_DIR");
    let bin = std::env::temp_dir().join(format!("siox_pos_struct_{}", std::process::id()));
    let build = Command::new(siox)
        .current_dir(root)
        .args(["test", FIXTURE, "--std", "std", "--no-run", "-o"])
        .arg(&bin)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "positional struct native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&bin).status().unwrap();
    let _ = std::fs::remove_file(&bin);
    assert!(run.success(), "positional struct native test failed");
}
