//! Regression for sioxc #486: a literal of an instantiated generic struct
//! (`Box<integer>`, `Reg<8>`) checks each field against the struct's
//! declared field type with the parameters bound to the arguments, nested
//! and typed literals included; it checked them against an unknown `T`.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i486_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
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
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text, dir.join("t.bin"))
}

#[test]
fn generic_literal_fields_are_checked() {
    let (ok, text, _) = compile("issue", ISSUE);
    assert!(!ok, "{text}");
    assert!(text.contains("Box.val` (integer) with real"), "{text}");
    assert!(text.contains("Box.val` (integer) with Char[5]"), "{text}");
    let (ok, text, _) = compile("more", MORE);
    assert!(!ok, "{text}");
    assert!(text.contains("Box.val` (integer) with real"), "{text}");
    assert!(text.contains("Box.val` (integer) with Char[1]"), "{text}");
    assert!(
        text.contains("`300` does not fit in a 8-bit vector"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn valid_generic_literals_still_run() {
    let (ok, text, binary) = compile("valid", VALID);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const ISSUE: &str = r#"module test_generic_struct_literal_bypass;

struct Box<T> {
    pub val: T,
}

#[test]
entity Top {}
impl Top {
    check: process {
        // Case 1: Assigning float literal to Box<integer>
        let b1: Box<integer> = { .val = 3.14 };
        print!("b1.val = {}\n", b1.val);

        // Case 2: Assigning string literal to Box<integer>
        let b2: Box<integer> = { .val = "hello" };
        print!("b2.val = {}\n", b2.val);
    }
}
"#;

const MORE: &str = r#"module i486_more;

struct Box<T> {
    pub val: T,
}

struct Reg<W: integer> {
    pub v: unsigned[W],
}

#[test]
entity Top {}
impl Top {
    check: process {
        let b: Box<integer> = Box<integer> { .val = 3.14 };
        let n: Box<Box<integer>> = { .val = { .val = "x" } };
        let r: Reg<8> = { .v = 300 };
        print!("{}\n", b.val);
    }
}
"#;

const VALID: &str = r#"module p486_ok;

struct Box<T> {
    pub val: T,
}

struct Reg<W: integer> {
    pub v: unsigned[W],
}

#[test]
entity Top {}
impl Top {
    check: process {
        let b: Box<integer> = { .val = 3 };
        let r: Box<real> = Box<real> { .val = 2.5 };
        let n: Box<Box<integer>> = { .val = { .val = 4 } };
        let g: Reg<8> = { .v = 200 };
        assert!(b.val == 3 and n.val.val == 4 and g.v == 200, "generic literals");
        print!("{}\n", r.val);
    }
}
"#;
