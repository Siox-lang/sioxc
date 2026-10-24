#![cfg(feature = "cli")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use std::process::Command;

const LEFT: &str = r#"module entity_left;
pub entity Cell<BASE: integer> { value: integer out }
impl<BASE: integer> Cell<BASE> { let hidden: integer = BASE; value = hidden; }
"#;
const RIGHT: &str = r#"module entity_right;
pub entity Cell { value: real out }
impl Cell { let hidden: real = 2.5; value = hidden; }
"#;
const SOURCE: &str = r#"module entity_identity;
use L = entity_left::Cell;
use R = entity_right::Cell;
#[test] entity EntityIdentityTest {}
impl EntityIdentityTest {
    let left: L<7> = {};
    let right: R = {};
    await 1ns;
    assert!(left.value == 7);
    assert!(right.value == 2.5);
}
"#;

#[test]
fn entity_ports_and_parameter_bindings_keep_identity_through_the_native_pipeline() {
    let dir = std::env::temp_dir().join(format!("siox_entity_identity_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("entity_left.siox"), LEFT).unwrap();
    std::fs::write(dir.join("entity_right.siox"), RIGHT).unwrap();
    let file = dir.join("entity_identity.siox");
    std::fs::write(&file, SOURCE).unwrap();
    let binary = dir.join("native");
    let compiler = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"));
    let compiled = compiler.compile(
        CompileRequest::new(SourceInput::path(&file), Emit::TestExecutable).with_output(&binary),
    );
    assert!(
        compiled.succeeded(),
        "{}{:?}",
        compiled.render_diagnostics(),
        compiled.failure
    );
    assert_eq!(compiled.test_plan.as_ref().unwrap().len(), 1);
    let hierarchy = compiled.hierarchy.as_ref().unwrap();
    assert_eq!(hierarchy.roots.len(), 1);
    let child = |name: &str| {
        hierarchy
            .instances
            .iter()
            .find(|instance| instance.name == name)
            .unwrap()
    };
    let left = child("left");
    let right = child("right");
    assert_ne!(left.entity_id, right.entity_id);
    assert_eq!(
        hierarchy.qualified_entity_name(left.entity_id),
        Some("entity_left::Cell")
    );
    assert_eq!(
        hierarchy.qualified_entity_name(right.entity_id),
        Some("entity_right::Cell")
    );
    assert!(left
        .params
        .iter()
        .any(|(name, value)| name == "BASE" && *value == siox::elab::ParamValue::Int(7)));
    let vcd = dir.join("out.vcd");
    let ran = Command::new(&binary).arg("-o").arg(&vcd).output().unwrap();
    assert!(
        ran.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(std::fs::metadata(vcd).unwrap().len() > 0);
    std::fs::remove_dir_all(dir).unwrap();
}
