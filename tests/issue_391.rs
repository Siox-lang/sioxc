//! Regressions for sioxc #391: a bare call to an impl member is checked
//! like a free function's call, a bare call cannot supply a receiver, and
//! bare and entity-qualified calls to statement-body `static fn`s run.

use std::process::Command;

fn build(name: &str, source: &str, test: bool) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i391_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    command.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")]);
    if test {
        command.arg("--test").arg("-o").arg(dir.join("t.bin"));
    } else {
        command.args(["--emit", "ir"]);
    }
    let out = command.arg(&file).output().unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text, dir.join("t.bin"))
}

#[test]
fn a_bare_call_to_an_impl_member_is_checked() {
    let (ok, text, _) = build("bad", I391_BAD, false);
    assert!(!ok, "{text}");
    assert!(text.contains("`read_v` has a receiver"), "{text}");
    assert!(
        text.contains("`add_pair` takes 2 argument(s) but 1 were given"),
        "{text}"
    );
    assert_eq!(
        text.matches("cannot initialize Bit with integer").count(),
        2,
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn bare_and_entity_qualified_static_calls_run() {
    let (ok, text, binary) = build("good", I391_GOOD, true);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    let text =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{text}");
}

const I391_BAD: &str = r#"module test_impl_bare_fn_call;
use std::sim::*;

struct Counter {
    pub v: integer,
}

impl Counter {
    // Non-static method (has implicit `self` receiver)
    pub fn read_v(scale: integer) -> integer {
        return self.v * scale;
    }

    // Static function
    pub static fn add_pair(a: integer, b: integer) -> integer {
        return a + b;
    }

    // Static function with statement body (expanded by cfg_calls)
    pub static fn sum_to(n: integer) -> integer {
        let acc: integer = 0;
        for i in 1..n {
            acc = acc + i;
        }
        return acc;
    }

    pub static fn unchecked_bare_calls() -> integer {
        // 1. Bare call to non-static method `read_v` with wrong arity & arg type
        //    and assigning to Bit — ALL pass Stage 4 with zero diagnostics!
        let bad1: Bit = read_v("wrong", 1, 2, 3);

        // 2. Bare call to `static fn add_pair` with wrong arity, wrong arg type,
        //    and wrong return type — ALL pass Stage 4 with zero diagnostics!
        let bad2: Bit = add_pair("not an int");

        return 0;
    }

    pub static fn valid_bare_call() -> integer {
        // 3. Valid bare call to `static fn sum_to(4)` inside `impl Counter`
        //    fails in `cfg_calls::inline_call_once` (`get_definition` only checks `free`).
        return sum_to(4);
    }
}

#[test]
entity TbBareStructCall {}

impl TbBareStructCall {
    process {
        let a: integer = Counter::valid_bare_call();
        assert!(a == 10, "expected 10");
    }
}

#[test]
entity TbEntityStaticCall {}

impl TbEntityStaticCall {
    static fn step_count(n: integer) -> integer {
        let acc: integer = 0;
        for i in 1..n {
            acc = acc + i;
        }
        return acc;
    }

    process {
        // 4. Valid `Entity::func(...)` call to statement-body `static fn` fails in
        //    `cfg_calls::inline_call_once`.
        let b: integer = TbEntityStaticCall::step_count(4);
        assert!(b == 10, "expected 10");
    }
}
"#;

const I391_GOOD: &str = r#"module test_impl_bare_fn_good;

struct Counter {
    pub v: integer,
}

impl Counter {
    pub static fn sum_to(n: integer) -> integer {
        let acc: integer = 0;
        for i in 1..n {
            acc = acc + i;
        }
        return acc;
    }

    pub static fn valid_bare_call() -> integer {
        return sum_to(4);
    }
}

#[test]
entity TbBareStructCall {}

impl TbBareStructCall {
    process {
        let a: integer = Counter::valid_bare_call();
        assert!(a == 10, "expected 10, got {}", a);
    }
}

#[test]
entity TbEntityStaticCall {}

impl TbEntityStaticCall {
    static fn step_count(n: integer) -> integer {
        let acc: integer = 0;
        for i in 1..n {
            acc = acc + i;
        }
        return acc;
    }

    process {
        let b: integer = TbEntityStaticCall::step_count(4);
        let c: integer = step_count(5);
        assert!(b == 10, "expected 10, got {}", b);
        assert!(c == 15, "expected 15, got {}", c);
    }
}
"#;
