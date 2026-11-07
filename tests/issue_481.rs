//! Regression for sioxc #481: a generic entity's impl is checked again for
//! each instantiation with its type parameters bound (`Adder<integer>`:
//! `T` is `integer`), so an operation or assignment invalid for that type
//! is reported; an unbound `T` silenced every check that touched it.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i481_{name}_{}", std::process::id()));
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
fn instantiated_generic_entities_are_checked() {
    let (ok, text, _) = compile("operator", OPERATOR);
    assert!(!ok, "{text}");
    assert!(
        text.contains("cannot apply `*` to integer and Char[11]"),
        "{text}"
    );
    let (ok, text, _) = compile("assign", ASSIGN);
    assert!(!ok, "{text}");
    assert!(text.contains("cannot assign real to integer"), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_valid_instantiation_still_runs() {
    let (ok, text, binary) = compile("valid", VALID);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const OPERATOR: &str = r#"module test_generic_typecheck_bypass;

entity Adder<T> {
    a: T in,
    sum: T out,
}
impl<T> Adder<T> {
    // Incompatible operation: multiplying generic input by a string literal!
    sum = a * "some string";
}

entity Top {
    res: integer out,
}
impl Top {
    let add: Adder<integer> = { .a = 10, .sum = res };
}
"#;

const ASSIGN: &str = r#"module test_generic_real_int;

entity Source<T> {
    out_data: T out,
}
impl<T> Source<T> {
    // Real literal assigned to generic port
    out_data = 3.14;
}

#[test]
entity Top {}
impl Top {
    let x: integer;
    let src: Source<integer> = { .out_data = x };
    check: process {
        await 1ns;
        // Prints: x = 4614253070214989087 (raw IEEE-754 float bits of 3.14!)
        print!("x = {}\n", x);
    }
}
"#;

const VALID: &str = r#"module p481_ok;

entity Adder<T> {
    a: T in,
    b: T in,
    sum: T out,
}
impl<T> Adder<T> {
    sum = a + b;
}

#[test]
entity Top {}
impl Top {
    let x: integer = 2;
    let y: integer = 3;
    let s: integer;
    let add: Adder<integer> = { .a = x, .b = y, .sum = s };
    check: process {
        await 1ns;
        assert!(s == 5, "s={}", s);
    }
}
"#;
