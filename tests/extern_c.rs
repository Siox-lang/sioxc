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
fn std_ffi_types_cross_at_their_c_widths() {
    let root = env!("CARGO_MANIFEST_DIR");
    let output = std::env::temp_dir().join(format!("siox_c_scalars_{}", std::process::id()));
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .args(["--test", "tests/fixtures/extern_c_scalars_test.siox", "-o"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "C scalar fixture failed to build:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&output).output().unwrap();
    let report =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success() && report.contains("CScalarsTest ... ok"),
        "C scalar fixture failed:\n{report}"
    );
    let _ = std::fs::remove_file(output);
}

#[test]
fn structs_and_arrays_cross_by_pointer() {
    let root = env!("CARGO_MANIFEST_DIR");
    let output = std::env::temp_dir().join(format!("siox_c_aggregates_{}", std::process::id()));
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .args([
            "--test",
            "tests/fixtures/extern_c_aggregates_test.siox",
            "-o",
        ])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "C aggregate fixture failed to build:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&output).output().unwrap();
    let report =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success() && report.contains("AggregatesTest ... ok"),
        "C aggregate fixture failed:\n{report}"
    );
    let _ = std::fs::remove_file(output);
}

#[test]
fn aggregates_are_testbench_parameters_only() {
    let root = env!("CARGO_MANIFEST_DIR");
    let source = std::env::temp_dir().join(format!(
        "siox_c_aggregate_errors_{}.siox",
        std::process::id()
    ));
    std::fs::write(
        &source,
        "module m;\nstruct P { pub a: integer, }\n\
         extern \"C\" { fn take(p: P) -> integer; fn give() -> P; fn bad(c: Char[2]) -> integer; }\n\
         entity E { y: integer out, }\nimpl E { process { let p: P = P { .a = 1 }; y = take(p); } }\n",
    )
    .unwrap();
    let compile = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .arg(&source)
        .args(["--emit", "metadata"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&compile.stderr);
    for message in [
        "extern C function `give` cannot return the aggregate `P`",
        "Char has no declared C character ABI",
        "`take` takes an aggregate by pointer, so it can only be called in a testbench",
    ] {
        assert!(stderr.contains(message), "missing `{message}`:\n{stderr}");
    }
    let _ = std::fs::remove_file(source);
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
        stderr.contains("extern C function `aggregate` cannot return the aggregate `Pair`"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("stage 5"),
        "unsafe signature reached elaboration:\n{stderr}"
    );

    let _ = std::fs::remove_file(source);
}
