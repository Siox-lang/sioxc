//! Regressions for sioxc #378 and #392 (parts 1-3).
//!
//! #378: a call whose function writes its parameter or
//! receiver runs when the argument is a constant (`inc(5)`) or a default
//! (`Counter::default().bumped()`): the constant is copied into a call local.
//!
//! #392: a statement call to a function with a result, a const-generic
//! procedure, and `Trait::method(r)` calls to statement-body or procedure
//! trait members all run.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_mutated_constant_argument_gets_its_own_local() {
    let dir = std::env::temp_dir().join(format!("siox_i378_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("i378.siox");
    std::fs::write(&file, I378).unwrap();
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
    let text = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "{text}");
    assert!(text.contains("c.count = 10, n = 6"), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn statement_value_generic_and_ufcs_calls_run() {
    let dir = std::env::temp_dir().join(format!("siox_i392_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("i392.siox");
    std::fs::write(&file, I392).unwrap();
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
    let text = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "{text}");
    assert!(text.contains("3 passed"), "{text}");
}

const I378: &str = r#"module test_method_on_default;

struct Counter {
    pub count: integer,
}

impl Counter {
    pub fn bumped() -> Counter {
        count = count + 10;
        return self;
    }
}

fn inc(x: integer) -> integer {
    x = x + 1;
    return x;
}

#[test]
entity TB {}

impl TB {
    run: process {
        let c: Counter = Counter::default().bumped();
        let n: integer = inc(5);
        print!("c.count = {}, n = {}\n", c.count, n);
        assert!(c.count == 10 and n == 6);
    }
}
"#;

const I392: &str = r#"module test_proc_call_bugs;
use std::sim::*;

trait Stepper {
    fn step_loop() -> integer;
    fn reset_proc();
}

struct Counter {
    pub v: integer,
}

impl Stepper for Counter {
    fn step_loop() -> integer {
        let acc: integer = 0;
        for i in 1..3 {
            acc = acc + i;
        }
        return self.v + acc;
    }

    fn reset_proc() {
        self.v = 0;
    }
}

fn bump_and_return(x: integer) -> integer {
    x = x + 10;
    return x;
}

fn clear_array<N: integer>(arr: integer[N]) {
    for i in 0..N - 1 {
        arr[i] = 99;
    }
}

#[test]
entity TbStmtValueCall {}

impl TbStmtValueCall {
    process {
        let x: integer = 5;
        bump_and_return(x);
        assert!(x == 15, "expected x == 15");
    }
}

#[test]
entity TbGenericProc {}

impl TbGenericProc {
    process {
        let arr: integer[3] = [1, 2, 3];
        clear_array(arr);
        assert!(arr[0] == 99 and arr[2] == 99, "expected cleared array");
    }
}

#[test]
entity TbUfcsTraitCalls {}

impl TbUfcsTraitCalls {
    process {
        let c: Counter = { .v = 10 };
        let r: integer = Stepper::step_loop(c);
        Stepper::reset_proc(c);
        assert!(r == 16 and c.v == 0, "expected r == 16 and c.v == 0");
    }
}
"#;
