#![cfg(feature = "cli")]

use std::process::Command;

fn sioxc() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sioxc"))
}

#[test]
fn every_code_is_explainable_without_source_or_std() {
    for code in siox::diag::codes::ALL {
        let output = sioxc()
            .args(["--explain", code, "--std", "__missing_explanation_std__"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{code}: {:?}", output.stderr);
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            siox::diag::codes::explanation(code).unwrap()
        );
        assert!(
            output.stderr.is_empty(),
            "explanations do not run compiler stages"
        );
    }
}

#[test]
fn unknown_code_fails_without_running_compiler_stages() {
    let output = sioxc().args(["--explain", "E-P000"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("no explanation") && stderr.contains("E-P000"));
    assert!(!stderr.contains("stage"));
}

#[test]
fn source_and_explanation_are_exclusive_and_one_is_required() {
    for args in [
        vec![],
        vec!["--explain"],
        vec!["--explain", "E-P003", "missing.siox"],
    ] {
        let output = sioxc().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}
