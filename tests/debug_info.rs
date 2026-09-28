//! Native source-level stepping is not implemented for Process IR yet.
//!
//! Reject `-g` explicitly rather than silently rebuilding the removed
//! per-design C harness or producing a binary without the promised metadata.

use std::process::Command;

#[test]
fn debug_test_builds_fail_explicitly_until_process_dwarf_exists() {
    let directory = std::env::temp_dir().join(format!("siox_process_debug_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("debug.siox");
    let binary = directory.join("debug.bin");
    std::fs::write(
        &source,
        "module debug; #[test] entity T {} impl T { assert!(true); }",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(["--test", "--debug"])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "unsupported debug build succeeded"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("native Process IR debug metadata is not implemented"),
        "unexpected diagnostic:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!binary.exists(), "a rejected build left an executable");
    let _ = std::fs::remove_dir_all(directory);
}
