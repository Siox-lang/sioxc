//! End-to-end coverage for attributed custom operators in the native
//! test harness paths.

use std::process::Command;

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

const FIXTURE: &str = "tests/fixtures/custom_operator_test.siox";

fn imported_operator_fixture(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let directory = std::env::temp_dir().join(format!(
        "siox_imported_operator_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(directory.join("local")).unwrap();
    std::fs::write(
        directory.join("local/operators.siox"),
        r#"module local::operators;
           using std::ops::{Operator};
           pub enum Flag { Off, On }
           #[precedence = 45]
           impl Operator<"unless", Flag, Bool> for Flag {
               fn apply(self, rhs: Flag) -> Bool { return true; }
           }
           #[precedence = 44]
           impl Operator<"^^", Flag, Bool> for Flag {
               fn apply(self, rhs: Flag) -> Bool { return true; }
           }"#,
    )
    .unwrap();
    std::fs::write(
        directory.join("local/api.siox"),
        "module local::api; pub using local::operators::{Flag};",
    )
    .unwrap();
    // Deliberately not imported. A directory-wide operator scan would pick up
    // this conflicting precedence and group the entry expression incorrectly.
    std::fs::write(
        directory.join("unrelated.siox"),
        r#"module unrelated;
           using std::ops::{Operator};
           enum Other { A }
           #[precedence = 1]
           impl Operator<"unless", Other, Bool> for Other {
               fn apply(self, rhs: Other) -> Bool { return true; }
           }"#,
    )
    .unwrap();
    let entry = directory.join("entry.siox");
    std::fs::write(
        &entry,
        r#"module imported_operator;
           using local::api::{Flag};
           #[test] entity ImportedOperator {}
           impl ImportedOperator {
               let left: Flag = Flag::On;
               let right: Flag = Flag::Off;
               let result: Bool;
               result = left unless right and left ^^ left;
               assert!(result == true, "imported operators execute");
           }"#,
    )
    .unwrap();
    (directory, entry)
}

#[test]
fn imported_custom_operators_are_known_during_api_parse() {
    let (directory, entry) = imported_operator_fixture("api");
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(SourceInput::path(&entry), Emit::Metadata),
    );
    assert!(
        compilation.succeeded(),
        "imported operator failed through Compiler API:\n{}",
        compilation.render_diagnostics()
    );
    assert!(compilation.modules.iter().any(|module| module
        .path
        .segments
        .iter()
        .map(|part| part.text.as_str())
        .eq(["local", "operators"])));
    assert!(!compilation.modules.iter().any(|module| module
        .path
        .segments
        .iter()
        .map(|part| part.text.as_str())
        .eq(["unrelated"])));
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn imported_custom_operators_run_in_native_harness() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let (directory, entry) = imported_operator_fixture("native");
    let binary = directory.join("operator-test");
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .arg("--test")
        .arg(&entry)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "imported operator native build failed:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&binary).output().unwrap();
    assert!(
        run.status.success(),
        "imported operator native execution failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn custom_operators_run_via_native_cli() {
    let siox = env!("CARGO_BIN_EXE_sioxc");
    let root = env!("CARGO_MANIFEST_DIR");
    let out = Command::new(siox)
        .current_dir(root)
        .args(["--test", FIXTURE, "--std", "std", "-o"])
        .arg(std::env::temp_dir().join(format!("siox_custom_cli_{}", std::process::id())))
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
    let root = env!("CARGO_MANIFEST_DIR");
    let bin = std::env::temp_dir().join(format!("siox_custom_ops_{}", std::process::id()));
    let build = Command::new(siox)
        .current_dir(root)
        .args(["--test", FIXTURE, "--std", "std", "-o"])
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
