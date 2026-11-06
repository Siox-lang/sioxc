//! Regression for sioxc #480: an enum discriminant that is not a constant
//! integer (a string, a real, an array, a runtime call) is reported, where
//! it was skipped and numbering counted on past it; a constant expression
//! still sets one.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i480_{name}_{}", std::process::id()));
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
fn a_discriminant_that_is_not_a_constant_integer_is_reported() {
    let (ok, text, _) = compile("bad", BAD);
    assert!(!ok, "{text}");
    for variant in ["Str", "Float", "Arr", "Call"] {
        assert!(
            text.contains(&format!("`State::{variant}` has the discriminant")),
            "{variant}: {text}"
        );
    }
}

#[cfg(feature = "llvm")]
#[test]
fn constant_expressions_set_discriminants() {
    let (ok, text, binary) = compile("constant", CONSTANT);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const BAD: &str = r#"module test_enum_discriminants;

fn compute() -> integer { return 42; }

enum State {
    Str = "hello",       // non-integer string
    Float = 3.14,        // non-integer real
    Arr = [1, 2, 3],     // non-integer array
    Call = compute(),    // non-constant function call
    Colliding = 0,       // collides with Str which defaulted to 0!
}

#[test]
entity Top {}
impl Top {
    check: process {
        print!("Str = {}\n", integer(State::Str));
        print!("Float = {}\n", integer(State::Float));
        print!("Arr = {}\n", integer(State::Arr));
        print!("Call = {}\n", integer(State::Call));
        print!("Colliding = {}\n", integer(State::Colliding));
    }
}
"#;

const CONSTANT: &str = r#"module p480_ok;

const BASE: integer = 4;

enum State {
    A = BASE,
    B,
    C = BASE * 4,
}

#[test]
entity Top {}
impl Top {
    check: process {
        assert!(integer(State::A) == 4 and integer(State::B) == 5 and integer(State::C) == 16, "discriminants");
    }
}
"#;
