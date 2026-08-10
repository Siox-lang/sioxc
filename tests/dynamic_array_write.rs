//! Runtime-indexed native aggregate writes must follow declared array labels
//! and work through fields, composite values, and packed elements.

use std::process::Command;

#[test]
fn native_runtime_array_writes_compile_and_run() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let binary =
        std::env::temp_dir().join(format!("siox_dynamic_array_write_{}", std::process::id()));
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .args([
            "--test",
            "tests/fixtures/dynamic_array_write_test.siox",
            "-o",
            binary.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "native build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&binary).output().unwrap();
    assert!(
        run.status.success(),
        "native runtime-array test failed:\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_file(binary);
}
