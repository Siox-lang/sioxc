//! Regressions for sioxc #389: Process IR call inlining keeps a let-bound
//! statement call and its order; a recursive function keeps each
//! activation's range-loop state; a parameter written through a helper call
//! reaches the caller; a method recurses on its receiver; and an associated
//! call with explicit type arguments inlines.

use std::process::Command;

fn passes(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_i389_{name}_{}", std::process::id()));
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
    let text = String::from_utf8_lossy(&run.stdout).to_string();
    assert!(run.status.success(), "{text}");
    text
}

#[cfg(feature = "llvm")]
#[test]
fn a_let_bound_statement_call_runs_in_order() {
    let text = passes("let_call", LET_CALL);
    assert!(text.contains("res1=105 a=105 res2=106 b=105"), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn recursion_keeps_each_activations_loop() {
    passes("loop", LOOP);
}

#[cfg(feature = "llvm")]
#[test]
fn a_helper_write_to_a_recursive_parameter_reaches_the_caller() {
    passes("helper", HELPER);
}

#[cfg(feature = "llvm")]
#[test]
fn a_method_recurses_and_type_arguments_inline() {
    passes("method", METHOD);
}

const LET_CALL: &str = r#"module test_dropped_and_reordered_call;
use std::sim::*;

fn mutate_and_check(x: integer) -> integer {
    x = x + 100;
    print!("mutate_and_check ran! x={}\n", x);
    return 0;
}

fn wrapper(v: integer) -> integer {
    let unused: integer = mutate_and_check(v);
    return v;
}

fn bump_and_ret(x: integer) -> integer {
    for k in 0..0 {
        x = x + 100;
    }
    return 1;
}

fn reorder_bug(v: integer) -> integer {
    let delta: integer = bump_and_ret(v);
    return v + delta;
}

#[test]
entity Tb {}

impl Tb {
    process {
        let a: integer = 5;
        let res1: integer = wrapper(a);
        let b: integer = 5;
        let res2: integer = reorder_bug(b);
        print!("res1={} a={} res2={} b={}\n", res1, a, res2, b);
        assert!(res1 == 105 and res2 == 106);
    }
}
"#;

const LOOP: &str = r#"module t389_loop;

fn tree_sum(depth: integer) -> integer {
    if depth <= 0 {
        return 1;
    }
    let total: integer = 0;
    for i in 0..1 {
        total = total + tree_sum(depth - 1);
    }
    return total;
}

fn down_sum(n: integer) -> integer {
    if n <= 0 {
        return 0;
    }
    let total: integer = 0;
    for i in n..1 {
        total = total + i;
    }
    return total + down_sum(n - 1);
}

#[test]
entity Tb {}

impl Tb {
    process {
        let t: integer = tree_sum(3);
        let d: integer = down_sum(3);
        assert!(t == 8, "t={}", t);
        assert!(d == 10, "d={}", d);
    }
}
"#;

const HELPER: &str = r#"module test_recursive_bugs;
use std::sim::*;

fn add_into(target: integer, delta: integer) -> integer {
    for k in 0..0 {
        target = target + delta;
    }
    return target;
}

fn rec_mut_via_helper(n: integer, acc: integer) -> integer {
    if n <= 0 {
        return acc;
    }
    let ignored: integer = add_into(acc, 10);
    return rec_mut_via_helper(n - 1, acc);
}

#[test]
entity Tb {}

impl Tb {
    process {
        let my_acc: integer = 5;
        let r2: integer = rec_mut_via_helper(3, my_acc);
        print!("rec_mut_via_helper: r2={} my_acc={} (expected 35 and 35)\n", r2, my_acc);
        assert!(r2 == 35 and my_acc == 35);
    }
}
"#;

const METHOD: &str = r#"module test_method_rec_and_turbofish;
use std::bits::unsigned;
use std::sim::*;

struct Counter {
    pub step: integer,
}

impl Counter {
    pub fn sum_down(n: integer) -> integer {
        if n <= 0 {
            return 0;
        }
        let cur: integer = step;
        cur = cur * n;
        return cur + self.sum_down(n - 1);
    }

    pub static fn bounce<T>(x: T) -> T {
        let acc: T = x;
        for i in 0..1 {
            acc = x;
        }
        return acc;
    }
}

#[test]
entity Tb {}

impl Tb {
    process {
        let c: Counter = { .step = 10 };
        let s: integer = c.sum_down(3);
        let a: unsigned[8] = Counter::bounce<unsigned[8]>(42);
        assert!(s == 60 and a == 42);
    }
}
"#;
