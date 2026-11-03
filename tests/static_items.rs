//! `static fn` (an implementation member without a receiver) and
//! `static let` (a local that keeps its value between activations and
//! calls; in a function, one copy per function, as C's `static`).

use std::process::Command;

fn sioxc(name: &str, source: &str, args: &[&str]) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_static_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(args)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text, dir)
}

#[test]
fn misplaced_static_is_a_syntax_error() {
    let (ok, text, _) = sioxc("syntax", SYNTAX, &["--emit", "metadata"]);
    assert!(!ok, "{text}");
    for message in [
        "`static` belongs to implementation members and locals",
        "`static let` stands directly in a process or function body",
        "a `static fn` has no receiver",
        "`static let` is only available in processes and functions",
    ] {
        assert!(text.contains(message), "missing `{message}`:\n{text}");
    }
}

#[test]
fn static_let_rules_are_checked() {
    let (ok, text, _) = sioxc("rules", BAD, &["--emit", "metadata"]);
    assert!(!ok, "{text}");
    for message in [
        "a `static let` initializer must be a constant",
        "a `static let` cannot hold an entity",
        "`static let` in a generic function is not supported yet",
        "`static let` in a hardware process is not supported yet",
        "`counted` reaches `static let`, which only a testbench can run",
    ] {
        assert!(text.contains(message), "missing `{message}`:\n{text}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn a_function_static_is_shared_by_every_call_and_fresh_per_test() {
    let (ok, text, dir) = sioxc("run", GOOD, &["--test", "-o", "static.bin"]);
    assert!(ok, "{text}");
    let run = Command::new(dir.join("static.bin")).output().unwrap();
    let text =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{text}");
    assert!(text.contains("test probe::Shared ... ok"), "{text}");
    assert!(text.contains("test probe::Fresh ... ok"), "{text}");
    assert_eq!(text.matches("bump call 1").count(), 2, "{text}");
    assert!(text.contains("bump call 2"), "{text}");
}

const GOOD: &str = r#"module probe;

fn next_id() -> integer {
    static let n: integer = 10;
    n = n + 1;
    return n;
}

fn bump() {
    static let calls: integer = 0;
    calls = calls + 1;
    print!("bump call {}", calls);
}

struct Counter { pub value: integer }

impl Counter {
    pub static fn at(value: integer) -> Counter {
        return Counter { .value = value };
    }
}

#[test]
entity Shared {}

impl Shared {
    let start: integer = 5;        // gives the root an initialization CFG
    first: process {
        assert!(next_id() == 11, "first call");
        await 2ns;
        assert!(next_id() == 13, "after the other process's call");
        bump();
        bump();
    }
    second: process {
        await 1ns;
        assert!(next_id() == 12, "shared with the first process");
        let c: Counter = Counter::at(start);
        assert!(c.value == 5, "static fn constructor");
    }
    local: process {
        static let k: integer = 3;
        k = k + 1;
        assert!(k == 4, "process static acts as a local");
        print!("shared test done");
    }
}

#[test]
entity Fresh {}

impl Fresh {
    check: process {
        assert!(next_id() == 11, "a new test starts from the initializer");
        bump();
        print!("fresh test done");
    }
}
"#;

const BAD: &str = r#"module probe;

entity Cell { a: Bit in }
impl Cell {}

fn reads(x: integer) -> integer {
    static let n: integer = x;
    return n;
}

fn holds() {
    static let cell: Cell;
}

fn generic<W: integer>(v: unsigned[W]) -> integer {
    static let g: integer = 0;
    return g;
}

fn counted() -> integer {
    static let k: integer = 0;
    k = k + 1;
    return k;
}

entity Hw { clk: Bit in, y: integer out }
impl Hw {
    y = counted();
    p: process {
        static let r: integer = 0;
    }
}
"#;

const SYNTAX: &str = r#"module probe;

static fn loose() {}

fn nested(c: Bool) -> integer {
    if c {
        static let m: integer = 0;
    }
    return 0;
}

struct P { pub x: integer }
impl P {
    pub static fn make(self) -> P { return self; }
}

entity Hw { y: integer out }
impl Hw {
    static let state: integer = 0;
}
"#;
