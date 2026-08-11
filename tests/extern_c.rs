//! Executable coverage for the deliberately small foreign-C ABI.

use std::process::Command;

#[test]
fn scalar_extern_c_calls_run_in_hardware_and_testbench_code() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }

    let root = env!("CARGO_MANIFEST_DIR");
    let output = std::env::temp_dir().join(format!("siox_extern_c_{}", std::process::id()));
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .args(["--test", "tests/fixtures/extern_c_test.siox", "-o"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "extern C fixture failed to build:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let run = Command::new(&output).output().unwrap();
    let report =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "extern C fixture failed:\n{report}");
    assert!(
        report.contains("ffi_contract::ForeignCallsTest ... ok"),
        "the expected native test did not run:\n{report}"
    );

    let _ = std::fs::remove_file(output);
}

#[test]
fn unsupported_extern_c_shapes_stop_before_lowering() {
    let root = env!("CARGO_MANIFEST_DIR");
    let source =
        std::env::temp_dir().join(format!("siox_bad_extern_c_{}.siox", std::process::id()));
    std::fs::write(
        &source,
        r#"module bad_extern_c;
           struct Pair { left: integer, right: integer }
           extern "C" {
               fn too_wide(value: unsigned[65]) -> integer;
               fn aggregate() -> Pair;
               fn side_effect(value: integer);
           }"#,
    )
    .unwrap();

    let compile = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .arg(&source)
        .args(["--emit", "metadata"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&compile.stderr);
    assert!(!compile.status.success(), "unsafe ABI shapes were accepted");
    assert!(stderr.contains("packed value is 65 bits"), "{stderr}");
    assert!(
        stderr.contains("aggregate and nominal values have no C layout mapping"),
        "{stderr}"
    );
    assert!(
        stderr.contains("void extern C function `side_effect` is not supported yet"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("stage 5"),
        "unsafe signature reached elaboration:\n{stderr}"
    );

    let _ = std::fs::remove_file(source);
}
