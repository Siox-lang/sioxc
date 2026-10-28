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
fn aggregate_results_wide_values_and_floats_follow_the_platform() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let scratch = std::env::temp_dir().join(format!("siox_c_returns_{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let helper = scratch.join("extern_c_returns.o");
    let compiled = Command::new("clang")
        .current_dir(root)
        .args(["-c", "-O2", "tests/fixtures/extern_c_returns.c", "-o"])
        .arg(&helper)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let output = scratch.join("returns");
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .args([
            "--test",
            "tests/fixtures/extern_c_returns_test.siox",
            "--link",
        ])
        .arg(&helper)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "C return fixture failed to build:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&output).output().unwrap();
    let report =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success() && report.contains("ReturnsTest ... ok"),
        "C return fixture failed:\n{report}"
    );
}

#[test]
fn handles_are_c_pointers_passed_by_value() {
    let root = env!("CARGO_MANIFEST_DIR");
    let scratch = std::env::temp_dir().join(format!("siox_c_handles_{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let source = scratch.join("handles.siox");
    std::fs::write(
        &source,
        r#"module handles;
use std::ffi::{c_int, Handle};
extern "C" {
    fn tmpfile() -> Handle;
    fn fputc(c: c_int, file: Handle) -> c_int;
    fn rewind(file: Handle);
    fn fgetc(file: Handle) -> c_int;
    fn fclose(file: Handle) -> c_int;
}
#[test] entity HandlesTest {}
impl HandlesTest { process {
    let file: Handle = tmpfile();
    assert!(not file.is_null(), "C made a FILE");
    assert!(fputc(65, file) == 65, "written through the handle");
    rewind(file);
    assert!(fgetc(file) == 65, "read back through it");
    assert!(fclose(file) == 0 and Handle::null().is_null() and file != Handle::null(), "closed");
} }
"#,
    )
    .unwrap();
    let output = scratch.join("handles");
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .arg("--test")
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "handle fixture failed to build:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&output).output().unwrap();
    let report =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success() && report.contains("HandlesTest ... ok"),
        "handle fixture failed:\n{report}"
    );
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
         entity E { y: integer out, z: integer out, }\n\
         impl E { process { let p: P = P { .a = 1 }; y = take(p); let q: P = give(); z = q.a; } }\n",
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
        "Char has no declared C character ABI",
        "`take` passes a struct or array to or from C, so it can only be called in a testbench",
        "`give` passes a struct or array to or from C, so it can only be called in a testbench",
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
           struct Flagged { count: integer, flag: Bit }
           extern "C" {
               fn character(value: Char) -> integer;
               fn flagged(value: Flagged) -> integer;
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
    assert!(
        stderr.contains("Char has no declared C character ABI"),
        "{stderr}"
    );
    assert!(stderr.contains("field `flag`"), "{stderr}");
    assert!(
        !stderr.contains("stage 5"),
        "unsafe signature reached elaboration:\n{stderr}"
    );

    let _ = std::fs::remove_file(source);
}
