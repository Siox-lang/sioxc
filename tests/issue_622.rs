//! Regression for sioxc #622: a type parameter in a nested position
//! (`(T, Bool)`, `Box<T>`) is inferred and bounded like a direct one. A
//! nested `integer` was skipped as a possible literal, so `T: Speak` went
//! unchecked and a typed `integer` conflicted with nothing; a generic
//! struct's arguments were not read at all.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use std::process::Command;

fn diagnostics(name: &str, source: &str) -> String {
    let path = std::env::temp_dir().join(format!("siox_i622_{name}.siox"));
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, source),
            Emit::Metadata,
        ))
        .render_diagnostics()
}

#[test]
fn nested_type_parameters_are_checked() {
    let text = diagnostics("tuple", TUPLE_BOUND);
    assert!(
        text.contains("does not satisfy the bound `T: Speak`"),
        "{text}"
    );
    let text = diagnostics("box", BOX_BOUND);
    assert!(
        text.contains("does not satisfy the bound `T: Speak`"),
        "{text}"
    );
    let text = diagnostics("conflict", TYPED_CONFLICT);
    assert!(
        text.contains("inferred as integer, but this argument gives it real"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn nested_type_parameters_that_fit_run() {
    let dir = std::env::temp_dir().join(format!("siox_i622_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("valid.siox");
    std::fs::write(&file, VALID).unwrap();
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
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success() && stdout.contains("3 5"), "{stdout}");
}

const TUPLE_BOUND: &str = r#"module test_trait_nested;

pub trait Speak {
    fn speak() -> integer;
}

pub fn call_speak<T: Speak>(pair: (T, Bool)) -> T {
    return pair.0;
}

#[test] entity T {}
impl T {
    process {
        // integer does NOT implement Speak, but call_speak compiles and runs cleanly!
        let res: integer = call_speak((42, true));
        print!("res={}\n", res);
        assert!(res == 42);
    }
}
"#;

const BOX_BOUND: &str = r#"module test_trait_struct;

pub trait Speak {
    fn speak() -> integer;
}

pub struct Box<T> {
    pub val: T,
}

pub fn call_box<T: Speak>(b: Box<T>) -> T {
    return b.val;
}

#[test] entity T {}
impl T {
    process {
        let b: Box<integer> = Box { .val = 42 };
        let res: integer = call_box(b);
        print!("res={}\n", res);
        assert!(res == 42);
    }
}
"#;

const TYPED_CONFLICT: &str = r#"module c622;
pub fn combine<T>(p1: (T, Bool), p2: (T, Bool)) -> T {
    return p1.0;
}
#[test] entity Tb {}
impl Tb {
    process {
        let a: integer = 42;
        let res: integer = combine((a, true), (3.14, false));
    }
}
"#;

const VALID: &str = r#"module p622;

pub trait Speak {
    fn speak() -> integer;
}

pub struct Dog { pub n: integer }
impl Speak for Dog {
    fn speak() -> integer { return self.n; }
}

pub struct Box<T> { pub val: T }

pub fn call_box<T: Speak>(b: Box<T>) -> T {
    return b.val;
}

pub fn first<T>(p: (T, Bool), q: (T, Bool)) -> T {
    return p.0;
}

#[test] entity Tb {}
impl Tb {
    process {
        let d: Box<Dog> = Box { .val = Dog { .n = 3 } };
        let r: Dog = call_box(d);
        let a: integer = 5;
        let x: integer = first((a, true), (7, false));
        print!("{} {}\n", r.n, x);
        assert!(r.n == 3 and x == 5);
    }
}
"#;
