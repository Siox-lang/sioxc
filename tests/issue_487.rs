//! Regression for sioxc #487: a `const` is checked against its declared
//! type as a `let` initializer is, so a value of another type or one that
//! does not fit a sized vector is reported; valid constants still run.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i487_{name}_{}", std::process::id()));
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
fn a_constant_of_another_type_is_reported() {
    let (ok, text, _) = compile("mismatch", MISMATCH);
    assert!(!ok, "{text}");
    assert!(
        text.contains("cannot initialize integer with Char[5]"),
        "{text}"
    );
    let (ok, text, _) = compile("mixed", MIXED);
    assert!(!ok, "{text}");
    assert_eq!(text.matches("cannot initialize").count(), 4, "{text}");
}

#[test]
fn a_constant_that_does_not_fit_is_reported() {
    let (ok, text, _) = compile("overflow", OVERFLOW);
    assert!(!ok, "{text}");
    assert!(
        text.contains("`300` does not fit in a 8-bit vector"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn valid_constants_still_run() {
    let (ok, text, binary) = compile("valid", VALID);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const MISMATCH: &str = r#"module test_const_type_mismatch;

const X: integer = "hello";

#[test]
entity Top {}
impl Top {
    check: process {
        let y: integer = X + 1;
        print!("y = {}\n", y);
    }
}
"#;

const OVERFLOW: &str = r#"module test_const_width_overflow;

const K: unsigned[8] = 300;

#[test]
entity Top {}
impl Top {
    check: process {
        print!("K = {}\n", K);
    }
}
"#;

const MIXED: &str = r#"module i487_mixed;

const A: real = "a string";
const B: unsigned[8] = 3.14;
const C: Char = 1000;
const D: (integer, integer) = 42;

#[test]
entity Top {}
impl Top {
    check: process {
        print!("{} {} {}\n", A, B, C);
    }
}
"#;

const VALID: &str = r#"module p487_ok;

const W: integer = 8;
const K: unsigned[8] = 200;
const R: real = 1.5;
const T: (integer, integer) = (3, 4);
const NAME: Char[3] = "abc";

#[test]
entity Top {}
impl Top {
    const LOCAL: unsigned[W] = 0xff;
    check: process {
        assert!(K == 200 and LOCAL == 255 and T.1 == 4, "consts");
        print!("{} {}\n", R, NAME);
    }
}
"#;
