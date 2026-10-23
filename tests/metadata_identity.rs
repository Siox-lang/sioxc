#![cfg(feature = "cli")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use std::process::Command;

const LEFT: &str = r#"module metadata_left;
pub attr label: integer for entity, let = 3;
pub entity Data { value: integer out }
impl Data { attr label = 7; value = Data'label; }
"#;
const RIGHT: &str = r#"module metadata_right;
pub attr label: Bool for entity, let = false;
pub entity Data { value: integer out }
impl Data { attr label = true; value = if Data'label { 11 } else { 0 }; }
"#;
const SOURCE: &str = r#"module identity;
use L = metadata_left::Data;
use R = metadata_right::Data;
use left_label = metadata_left::label;
use right_label = metadata_right::label;
attr keep: integer for let = 1;
attr test: integer for entity = 1;
entity NotATest {}
attr test for NotATest = 99;
#[test] entity IdentityTest {}
attr test for IdentityTest = 21;
impl IdentityTest {
    let left: L = {};
    let right: R = {};
    attr left_label for both = 13;
    attr right_label for both = false;
    attr keep for both = 19;
    let both: L = {};
    await 1ns;
    assert!(left'left_label == 7 and left.value == 7);
    assert!(right'right_label and right.value == 11);
    assert!(both'left_label == 13 and not both'right_label and both'keep == 19);
    assert!(both.value == 7);
    assert!(metadata_left::Data'left_label == 7);
    assert!(metadata_right::Data'right_label);
    assert!(IdentityTest'test == 21);
}
"#;

fn compiler() -> Compiler {
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
}

#[test]
fn metadata_identity_survives_aliases_tree_output_and_native_execution() {
    let dir = std::env::temp_dir().join(format!("siox_metadata_identity_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("metadata_left.siox"), LEFT).unwrap();
    std::fs::write(dir.join("metadata_right.siox"), RIGHT).unwrap();
    let file = dir.join("identity.siox");
    std::fs::write(&file, SOURCE).unwrap();
    let binary = dir.join("native");
    let compiled = compiler().compile(
        CompileRequest::new(SourceInput::path(&file), Emit::TestExecutable).with_output(&binary),
    );
    assert!(
        compiled.succeeded(),
        "{}{:?}",
        compiled.render_diagnostics(),
        compiled.failure
    );
    assert_eq!(compiled.test_plan.as_ref().unwrap().len(), 1);
    assert!(
        !compiled
            .diagnostics
            .diagnostics()
            .iter()
            .any(|d| d.code == Some(siox::diag::codes::UNIMPLEMENTED_ATTR)),
        "{}",
        compiled.render_diagnostics()
    );
    let hierarchy = compiled.hierarchy.as_ref().unwrap();
    let both = hierarchy
        .instances
        .iter()
        .find(|instance| instance.name == "both")
        .unwrap();
    assert!(
        both.attrs
            .contains(&("metadata_left::label".into(), Some("13".into()))),
        "{:?}",
        both.attrs
    );
    assert!(
        both.attrs
            .contains(&("metadata_right::label".into(), Some("false".into()))),
        "{:?}",
        both.attrs
    );
    assert!(
        both.attrs
            .contains(&("identity::keep".into(), Some("19".into()))),
        "{:?}",
        both.attrs
    );
    let ran = Command::new(&binary)
        .arg("-o")
        .arg(dir.join("out.vcd"))
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn metadata_aliases_keep_type_errors_and_duplicate_binding_checks() {
    let dir =
        std::env::temp_dir().join(format!("siox_metadata_identity_bad_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("metadata_left.siox"), LEFT).unwrap();
    std::fs::write(dir.join("metadata_right.siox"), RIGHT).unwrap();
    let source = r#"module invalid;
use R = metadata_right::Data;
use tag = metadata_right::label;
entity E {}
impl E { attr tag for child = 13; let child: R = {}; }
"#;
    let bad = compiler().compile(CompileRequest::new(
        SourceInput::memory(dir.join("invalid.siox"), source),
        Emit::Metadata,
    ));
    assert!(!bad.succeeded());
    assert_eq!(
        bad.diagnostics
            .diagnostics()
            .iter()
            .filter(|d| d.code == Some(siox::diag::codes::INVALID_ATTR_VALUE_TYPE))
            .count(),
        1,
        "{}",
        bad.render_diagnostics()
    );
    let source = r#"module duplicate;
use a = metadata_left::label;
use b = metadata_left::label;
entity E {}
attr a for E = 1;
attr b for E = 2;
"#;
    let bad = compiler().compile(CompileRequest::new(
        SourceInput::memory(dir.join("duplicate.siox"), source),
        Emit::Metadata,
    ));
    assert!(!bad.succeeded());
    assert_eq!(
        bad.diagnostics
            .diagnostics()
            .iter()
            .filter(|d| d.code == Some(siox::diag::codes::DUPLICATE_ATTR_BINDING))
            .count(),
        1,
        "{}",
        bad.render_diagnostics()
    );
    std::fs::remove_dir_all(dir).unwrap();
}
