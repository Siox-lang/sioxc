//! Antigravity's issue #361: a declared `T::new()` / `T::default()`.

use std::process::Command;

fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_issue_361_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let binary = dir.join(name);
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
            "-o",
        ])
        .arg(&binary)
        .arg(&file)
        .output()
        .unwrap();
    assert!(output.status.success(), "{name}: {output:?}");
    String::from_utf8_lossy(&Command::new(&binary).output().unwrap().stdout).to_string()
}

/// An inherent `new` and an `impl Default` run their bodies, in hardware
/// and testbench; an undeclared `default()` is still the structural one.
#[test]
fn a_declared_constructor_runs_its_body() {
    let issue = run(
        "issue",
        r#"module test_new_zero_args;

struct Counter {
    pub count: integer,
    pub step: integer,
}

impl Counter {
    pub fn new() -> Counter {
        return Counter { .count = 100, .step = 5 };
    }
}

#[test]
entity TestBench {}

impl TestBench {
    run: process {
        let c: Counter = Counter::new();
        print!("count: {}, step: {}\n", c.count, c.step);
        assert!(c.count == 100 and c.step == 5);
    }
}
"#,
    );
    assert!(
        issue.contains("count: 100, step: 5") && issue.contains("1 passed"),
        "{issue}"
    );
    let both = run(
        "both",
        r#"module v361;
use std::bits::unsigned;
struct Counter { pub count: integer, pub step: integer }
impl Counter { pub fn new() -> Counter { return Counter { .count = 100, .step = 5 }; } }
struct Pt { pub x: unsigned[8], pub y: unsigned[8] }
impl Default for Pt { fn default() -> Pt { return Pt { .x = 7, .y = 9 }; } }
struct Plain { pub a: unsigned[8] }
entity Top { cx: integer out, py: unsigned[8] out, pa: unsigned[8] out }
impl Top {
    p: process {
        let c: Counter = Counter::new();
        let q: Pt = Pt::default();
        let z: Plain = Plain::default();
        cx = c.count; py = q.y; pa = z.a;
    }
}
#[test] entity T {}
impl T {
    let cx: integer; let py: unsigned[8]; let pa: unsigned[8];
    let d: Top = { .cx = cx, .py = py, .pa = pa };
    process {
        await 1ns;
        let c: Counter = Counter::new(); let q: Pt = Pt::default(); let z: Plain = Plain::default();
        print!("HW {} {} {} TB {} {} {} {}\n", cx, py, pa, c.count, c.step, q.x, z.a);
        assert!(cx == 100 and py == 9 and pa == 0 and c.step == 5 and q.x == 7 and z.a == 0);
    }
}
"#,
    );
    assert!(
        both.contains("HW 100 9 0 TB 100 5 7 0") && both.contains("1 passed"),
        "{both}"
    );
}
