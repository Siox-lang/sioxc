//! Regression for sioxc #454: a generic struct constructed inline
//! (`Box<integer> { .val = 42 }.val`, `unbox(Box<integer> { .. })`,
//! `Buffer<3> { .data = [10, 20, 30] }.data[0]`) is laid out, fields
//! included, from its written type arguments.

use std::process::Command;

fn runs(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i454_{name}_{}", std::process::id()));
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
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains(" ok"),
        "{name}: {stdout}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn inline_generic_constructs_run() {
    runs("field", FIELD);
    runs("argument", ARGUMENT);
    runs("array_field", ARRAY_FIELD);
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

const ARRAY_FIELD: &str = r#"module test_generic_construct_field;

struct Buffer<N: integer> {
    pub data: integer[N],
}

#[test]
entity TestGenericConstructField {}
impl TestGenericConstructField {
    let x: integer = Buffer<3> { .data = [10, 20, 30] }.data[0];
    assert!(x == 10, "x is 10");
}
"#;
