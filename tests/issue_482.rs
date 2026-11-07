//! Regression for sioxc #482: a generic struct's methods are checked again
//! for each instantiation, by a `let b: Box<integer>` or a qualified call
//! `Box<integer>::f()`, with `T` bound, so a write invalid for that type is
//! reported; valid generic methods still run.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i482_{name}_{}", std::process::id()));
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
fn generic_struct_methods_are_checked_per_instantiation() {
    let (ok, text, _) = compile("method", METHOD);
    assert!(!ok, "{text}");
    assert!(text.contains("cannot assign real to integer"), "{text}");
    let (ok, text, _) = compile("qualified", QUALIFIED);
    assert!(!ok, "{text}");
    assert!(text.contains("cannot assign Char[13] to integer"), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn valid_generic_methods_still_run() {
    let (ok, text, binary) = compile("valid", VALID);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const METHOD: &str = r#"module test_struct_instance_method;

struct Box<T> {
    pub val: T,
}

impl<T> Box<T> {
    pub fn set_real() {
        // 'val' is type T, but 3.14 is real.
        // Stage 4 typechecking passes with 0 errors!
        val = 3.14;
    }
}

#[test]
entity Top {}
impl Top {
    check: process {
        let b: Box<integer> = Box { .val = 10 };
        b.set_real();
        // Prints: b.val = 4614253070214989087 (raw IEEE-754 bitcast of 3.14!)
        print!("b.val = {}\n", b.val);
    }
}
"#;

const QUALIFIED: &str = r#"module test_struct_method_bypass;

struct Box<T> {
    pub val: T,
}

impl<T> Box<T> {
    pub static fn bad_method() -> integer {
        let b: Box<T>;
        b.val = "string into T";
        return 0;
    }
}

#[test]
entity Top {}
impl Top {
    check: process {
        let x: integer = Box<integer>::bad_method();
        print!("x = {}\n", x);
    }
}
"#;

const VALID: &str = r#"module p482_ok;

struct Box<T> {
    pub val: T,
}

impl<T> Box<T> {
    pub fn set(v: T) {
        val = v;
    }
    pub fn get() -> T {
        return val;
    }
}

#[test]
entity Top {}
impl Top {
    check: process {
        let b: Box<integer> = Box { .val = 10 };
        b.set(12);
        let r: Box<real> = Box { .val = 1.5 };
        assert!(b.get() == 12 and b.val == 12, "box");
        print!("{}\n", r.get());
    }
}
"#;
