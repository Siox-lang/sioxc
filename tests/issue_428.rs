//! Regression for sioxc #428: a macro body that declares `W` keeps a named
//! generic argument's key (`Cell<W = 8>`, `Buffer<W = 8>`) as written, the
//! parameter's name, instead of renaming it with the macro's own `W`.

use std::process::Command;

fn passes(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i428_{name}_{}", std::process::id()));
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
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_named_entity_argument_survives_hygiene() {
    passes("entity", ENTITY);
}

#[cfg(feature = "llvm")]
#[test]
fn a_named_struct_argument_survives_hygiene() {
    passes("struct", STRUCT);
}

const ENTITY: &str = r#"module m;

entity Cell<W: integer> {
    out_sig: unsigned[W] out,
}
impl<W: integer> Cell<W> {
    out_sig = 0;
}

macro test_macro() {
    let W: integer = 1;
    let c: Cell<W = 8>;
}

entity Top {}
impl Top {
    test_macro!();
}

#[test]
entity Test {}
impl Test {
    let t: Top;
    check: process {
        await 1ns;
    }
}
"#;

const STRUCT: &str = r#"module m;

struct Buffer<W: integer> {
    pub data: unsigned[W],
}

macro test_macro() {
    let W: integer = 1;
    let b: Buffer<W = 8> = { .data = 42 };
    out_data = b.data;
}

entity Top {
    out_data: unsigned[8] out,
}
impl Top {
    test_macro!();
}

#[test]
entity Test {}
impl Test {
    let out_data: unsigned[8];
    let dut: Top = { .out_data = out_data };

    check: process {
        await 1ns;
        assert!(out_data == 42);
    }
}
"#;
