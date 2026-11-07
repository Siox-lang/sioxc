//! Regression for sioxc #454 and #408 (in part): a generic struct value
//! constructed with its arguments written (`Box<integer> { .. }`) takes its
//! layout from the written type, and an array of generic structs
//! (`Box<integer>[2]`) is laid out from its element type; both had no width
//! and failed native lowering.

use std::process::Command;

fn run(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!(
        "siox_generic_layouts_{name}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
        ])
        .arg(&file)
        .arg("-o")
        .arg(dir.join("t.bin"))
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    assert!(
        run.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_typed_generic_construct_lowers() {
    run("field", FIELD);
    run("argument", ARGUMENT);
}

#[cfg(feature = "llvm")]
#[test]
fn an_array_of_generic_structs_lowers() {
    run("array", ARRAY);
}

const FIELD: &str = r#"module test_box_construct_field;

struct Box<T> {
    pub val: T,
}

#[test]
entity TestBoxConstructField {}
impl TestBoxConstructField {
    let x: integer = Box<integer> { .val = 42 }.val;
    assert!(x == 42, "x is 42");
}
"#;

const ARGUMENT: &str = r#"module test_unbox_arg;

struct Box<T> {
    pub val: T,
}

fn unbox<T>(b: Box<T>) -> T {
    return b.val;
}

#[test]
entity TestUnboxArg {}
impl TestUnboxArg {
    let x: integer = unbox(Box<integer> { .val = 42 });
    assert!(x == 42, "x is 42");
}
"#;

const ARRAY: &str = r#"module test_array_generic_struct;
use std::sim::*;

struct Node<N: integer> {
    pub val: unsigned[N],
}

#[test]
entity Tb {}

impl Tb {
    process {
        let arr: Node<8>[2] = [
            Node { .val = 10 },
            Node { .val = 20 }
        ];
        assert!(arr[0].val == 10 and arr[1].val == 20);
    }
}
"#;
