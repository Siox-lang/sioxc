//! Regressions for sioxc #380 (a field spelled like a type hid the type in
//! signatures and `state::Busy`) and #385 (a function `static let`
//! initialized by a multi-statement call, or of a generic struct type).

use std::process::Command;

fn run(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i380_{name}_{}", std::process::id()));
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
    let built = String::from_utf8_lossy(&build.stdout).to_string()
        + &String::from_utf8_lossy(&build.stderr);
    assert!(build.status.success(), "{built}");
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let text =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_field_spelled_like_a_type_leaves_the_type_alone() {
    run("field_type", I380);
}

#[cfg(feature = "llvm")]
#[test]
fn a_function_static_runs_a_call_in_its_initializer() {
    run("call_init", I385_CALL);
}

#[cfg(feature = "llvm")]
#[test]
fn a_function_static_of_a_generic_struct_keeps_its_layout() {
    run("generic_struct", I385_GENERIC);
}

const I380: &str = r#"module test_bare_field_type_shadow;

enum state {
    Idle,
    Busy,
}

struct Machine {
    pub state: state,
}

impl Machine {
    pub fn current() -> state {
        return state;
    }

    pub fn is_busy() -> Bool {
        return self.state == state::Busy;
    }
}

#[test]
entity TB {}

impl TB {
    run: process {
        let m: Machine = Machine { .state = state::Busy };
        assert!(m.current() == state::Busy);
        assert!(m.is_busy());
    }
}
"#;

const I385_CALL: &str = r#"module test_static_let_call_init;

fn compute_start() -> integer {
    let x: integer = 10;
    x = x + 5;
    return x;
}

fn next_count() -> integer {
    static let c: integer = compute_start();
    c = c + 1;
    return c;
}

#[test]
entity Tb {}

impl Tb {
    check: process {
        let a: integer = next_count();
        let b: integer = next_count();
        assert!(a == 16, "expected 16");
        assert!(b == 17, "expected 17");
    }
}
"#;

const I385_GENERIC: &str = r#"module test_fn_static_generic_struct;

pub struct Boxed<T> {
    pub val: T,
}

fn next_boxed() -> integer {
    static let b: Boxed<(integer, integer)> = { .val = (10, 20) };
    b.val = (b.val.0 + 1, b.val.1 + 2);
    return b.val.0 + b.val.1;
}

#[test]
entity Tb {}
impl Tb {
    let seed: integer = 5;
    check: process {
        let r1: integer = next_boxed() + seed - 5;
        let r2: integer = next_boxed();
        assert!(r1 == 33, "expected 33");
        assert!(r2 == 36, "expected 36");
    }
}
"#;
