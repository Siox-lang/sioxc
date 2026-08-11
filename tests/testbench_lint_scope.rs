//! W-P014 is a hardware driver-context lint. Sequential native stimulus
//! settles after every connected-signal write, so adjacent writes may be
//! deliberately observable and must not be diagnosed as dead assignments.

use std::process::Command;

#[test]
fn adjacent_testbench_writes_are_observable_and_not_linted() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let binary =
        std::env::temp_dir().join(format!("siox_testbench_lint_scope_{}", std::process::id()));
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .args([
            "--test",
            "tests/fixtures/testbench_lint_scope_test.siox",
            "-o",
            binary.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let diagnostics = format!(
        "{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(
        build.status.success(),
        "native build failed:\n{diagnostics}"
    );
    assert!(
        !diagnostics.contains("W-P014"),
        "testbench stimulus was treated as a hardware driver:\n{diagnostics}"
    );
    let run = Command::new(&binary).output().unwrap();
    assert!(
        run.status.success(),
        "native stimulus test failed:\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_file(binary);
}
