#![cfg(feature = "cli")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use siox::diag::codes;
use std::path::PathBuf;
use std::process::Command;

const PROVIDER: &str = r#"module entity_alias_provider;
const BASE: integer = 6;
pub attr mark: integer for entity, let = 1;
pub entity Cell<BASE: integer> { input: integer in, value: integer out }
impl<BASE: integer> Cell<BASE> { attr mark = 9; value = BASE + input; }
pub type One = Cell<(BASE + 1)>;
pub type Link = One;
pub type Group = Link[2];
pub attr specific: integer for One = 2;
"#;

fn setup(name: &str) -> (Compiler, PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_entity_alias_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("entity_alias_provider.siox"), PROVIDER).unwrap();
    (
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")),
        dir,
    )
}

#[test]
fn transparent_entity_aliases_keep_parameters_arrays_metadata_and_native_behavior() {
    let (compiler, dir) = setup("native");
    let source = r#"module entity_alias_test;
use entity_alias_provider::{Link, Group, mark, specific};
const BASE: integer = 99;
#[test] entity AliasTest {}
impl AliasTest {
    let direct: Link = { .input = 0 };
    attr specific for direct = 4;
    let group: Group;
    group[0] = Link { .input = 0 };
    group[1] = Link { .input = 0 };
    await 1ns;
    assert!(direct.value == 7);
    assert!(direct'mark == 9);
    assert!(Link'mark == 9 and direct'specific == 4);
    assert!(group[0].value == 7 and group[1].value == 7);
}
"#;
    let binary = dir.join("native");
    let compiled = compiler.compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("entity_alias_test.siox"), source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compiled.succeeded(),
        "{}{:?}",
        compiled.render_diagnostics(),
        compiled.failure
    );
    let hierarchy = compiled.hierarchy.as_ref().unwrap();
    assert_eq!(hierarchy.roots.len(), 1);
    let root = hierarchy.instance(hierarchy.roots[0]);
    assert_eq!(root.children.len(), 3);
    for &id in &root.children {
        let child = hierarchy.instance(id);
        assert_eq!(
            hierarchy.qualified_entity_name(child.entity_id),
            Some("entity_alias_provider::Cell")
        );
        assert!(
            child
                .params
                .iter()
                .any(|(name, value)| name == "BASE" && *value == siox::elab::ParamValue::Int(7)),
            "{:?}",
            child.params
        );
    }
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
fn an_entity_instantiated_through_an_alias_is_not_an_extra_structural_root() {
    let (compiler, dir) = setup("roots");
    let source = "module alias_roots; use entity_alias_provider::Link;
        entity Top {} impl Top { let child: Link = {}; }";
    let compiled = compiler.compile(CompileRequest::new(
        SourceInput::memory(dir.join("roots.siox"), source),
        Emit::Metadata,
    ));
    assert!(
        compiled.succeeded(),
        "{}{:?}",
        compiled.render_diagnostics(),
        compiled.failure
    );
    let hierarchy = compiled.hierarchy.as_ref().unwrap();
    assert_eq!(hierarchy.roots.len(), 1);
    assert_eq!(hierarchy.instance(hierarchy.roots[0]).entity, "Top");
    assert_eq!(hierarchy.instance(hierarchy.roots[0]).children.len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn runtime_connections_to_aliased_instances_and_array_elements_keep_source_order() {
    let (compiler, dir) = setup("ordered");
    let source = r#"module alias_order;
use entity_alias_provider::{Link, Group};
fn take(value: integer) -> integer { return value * 2; }
#[test] entity AliasOrder {}
impl AliasOrder {
    let value: integer = 3;
    let direct: Link = { .input = take(value) };
    let group: Group;
    group[0] = Link { .input = take(value) };
    value = 5;
    group[1] = Link { .input = take(value) };
    await 1ns;
    assert!(direct.value == 13);
    assert!(group[0].value == 13);
    assert!(group[1].value == 17);
}
"#;
    let binary = dir.join("ordered");
    let compiled = compiler.compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("ordered.siox"), source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compiled.succeeded(),
        "{}{:?}",
        compiled.render_diagnostics(),
        compiled.failure
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
fn unresolved_array_constructor_connections_are_rejected_not_dropped() {
    let (compiler, dir) = setup("dynamic");
    let source = r#"module alias_dynamic;
use entity_alias_provider::{Link, Group};
fn take(value: integer) -> integer { return value * 2; }
#[test] entity Dynamic {}
impl Dynamic {
    let value: integer = 3;
    let group: Group;
    for i in 0..1 { group[i] = Link { .input = take(value) }; }
}
"#;
    let compiled = compiler.compile(CompileRequest::new(
        SourceInput::memory(dir.join("dynamic.siox"), source),
        Emit::TestExecutable,
    ));
    assert!(!compiled.succeeded());
    assert!(
        compiled
            .diagnostics
            .diagnostics()
            .iter()
            .any(|d| d.code == Some(codes::UNSUPPORTED_EXPR)),
        "{}{:?}",
        compiled.render_diagnostics(),
        compiled.failure
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn aliases_do_not_bypass_entity_constant_or_placement_checks() {
    let (compiler, dir) = setup("invalid");
    for (name, source, code, expected) in [
        (
            "constant",
            "module bad_const; use entity_alias_provider::Link;
            const BAD: Link = {}; entity Top {}",
            codes::CONST_ENTITY_INSTANCE,
            1,
        ),
        (
            "placement",
            "module bad_place; use entity_alias_provider::Link;
            fn build() { let bad: Link = {}; }
            entity Top {} impl Top { worker: process { let bad: Link = {}; } }",
            codes::INSTANCE_PLACEMENT,
            2,
        ),
    ] {
        let compiled = compiler.compile(CompileRequest::new(
            SourceInput::memory(dir.join(format!("{name}.siox")), source),
            Emit::Metadata,
        ));
        assert!(!compiled.succeeded(), "{name} unexpectedly compiled");
        assert_eq!(
            compiled
                .diagnostics
                .diagnostics()
                .iter()
                .filter(|d| d.code == Some(code))
                .count(),
            expected,
            "{name}: {}{:?}",
            compiled.render_diagnostics(),
            compiled.failure
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_named_alias_target_does_not_match_an_unrelated_entity() {
    let (compiler, dir) = setup("target");
    let source = r#"module alias_target;
use entity_alias_provider::specific;
entity Other {}
entity Top {}
impl Top { attr specific for bad = 4; let bad: Other = {}; }
"#;
    let compiled = compiler.compile(CompileRequest::new(
        SourceInput::memory(dir.join("target.siox"), source),
        Emit::Metadata,
    ));
    assert!(!compiled.succeeded());
    assert_eq!(
        compiled
            .diagnostics
            .diagnostics()
            .iter()
            .filter(|d| d.code == Some(codes::INVALID_ATTR_TARGET))
            .count(),
        1,
        "{}{:?}",
        compiled.render_diagnostics(),
        compiled.failure
    );
    std::fs::remove_dir_all(dir).unwrap();
}
