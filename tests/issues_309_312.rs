//! Antigravity's issues #309–#312. Each case is named after its GitHub issue.

use std::process::Command;

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("siox_issues_309_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build `source` as a test binary and run it, returning everything printed.
fn run(name: &str, source: &str) -> String {
    let dir = scratch();
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
    let mut text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        let ran = Command::new(&binary).output().unwrap();
        text += &String::from_utf8_lossy(&ran.stdout);
        text += &String::from_utf8_lossy(&ran.stderr);
    }
    text
}

/// Every case asserts its own values; the test passes when they all hold.
#[test]
fn programs_that_failed_to_lower_now_run() {
    let cases: &[(&str, &str)] = &[
        (
            "309",
            r#"module test;
use std::bits::unsigned;

entity Cell {
    din: unsigned[8] in,
    dout: unsigned[8] out,
}
impl Cell {
    dout = not din;
}

#[test]
entity TestTop {}
impl TestTop {
    let stage: Cell[2];
    run: process {
        for i in 0..1 {
            stage[i].din = 42;
        }
        await 1ns;
        for i in 0..1 {
            let v: unsigned[8] = stage[i].dout;
            assert!(v == 213);
        }
    }
}
"#,
        ),
        (
            "310",
            r#"module test;
use std::bits::unsigned;

entity Receiver {
    data: unsigned[8][4] in
}
impl Receiver {}

#[test]
entity TestInstDynWrite {}
impl TestInstDynWrite {
    let rx: Receiver;
    run: process {
        let i: unsigned[2] = 2;
        rx.data[i] = 42;
        assert!(rx.data[2] == 42);
    }
}
"#,
        ),
        (
            "312_read",
            r#"module test;
use std::bits::unsigned;

entity Receiver {
    data: unsigned[8] in,
    out_val: unsigned[8] out,
}
impl Receiver {
    out_val = data;
}

#[test]
entity TestInstDynRead {}
impl TestInstDynRead {
    let rxs: Receiver[2];
    run: process {
        rxs[0].data = 42;
        rxs[1].data = 99;
        await 1ns;
        let idx: unsigned[8] = 1;
        let val: unsigned[8] = rxs[idx].out_val;
        assert!(val == 99);
    }
}
"#,
        ),
        (
            "312_write",
            r#"module test;
use std::bits::unsigned;

entity Receiver {
    data: unsigned[8] in,
    out_val: unsigned[8] out,
}
impl Receiver {
    out_val = data;
}

#[test]
entity TestInstDynWrite {}
impl TestInstDynWrite {
    let rxs: Receiver[2];
    run: process {
        let idx: unsigned[8] = 0;
        rxs[idx].data = 42;
        await 1ns;
        let val: unsigned[8] = rxs[0].out_val;
        assert!(val == 42);
    }
}
"#,
        ),
    ];
    for (issue, source) in cases {
        let out = run(&format!("run_{issue}"), source);
        assert!(
            out.contains("passed;") && !out.contains("FAILED"),
            "#{issue}: {out}"
        );
    }
}

/// The issues' designs, compiled to objects with `Top` as the root.
#[test]
fn designs_compile() {
    let cases: &[(&str, &str)] = &[
        (
            "311_0",
            r#"module test;
use std::bits::unsigned;

fn calc(a: unsigned[8]) -> unsigned[8] {
    let x: unsigned[8];
    x = a + 1;
    return x * 2;
}

entity Top {
    din: unsigned[8] in,
    dout: unsigned[8] out,
}
impl Top {
    dout = calc(din);
}
"#,
        ),
        (
            "311_1",
            r#"module test;
use std::bits::unsigned;

struct Point {
    pub x: unsigned[8],
    pub y: unsigned[8],
}

fn make_point(a: unsigned[8]) -> Point {
    let p: Point;
    p.x = a;
    p.y = a + 1;
    return p;
}

entity Top {
    din: unsigned[8] in,
    dout: unsigned[8] out,
}
impl Top {
    let pt: Point;
    pt = make_point(din);
    dout = pt.x;
}
"#,
        ),
        (
            "311_2",
            r#"module test;
use std::bits::unsigned;

fn table() -> unsigned[8][4] {
    let t: unsigned[8][4];
    t[0] = 10;
    t[1] = 20;
    t[2] = 30;
    t[3] = 40;
    return t;
}

entity Top {
    dout: unsigned[8] out,
}
impl Top {
    dout = table()[0];
}
"#,
        ),
    ];
    let dir = scratch();
    for (issue, source) in cases {
        let file = dir.join(format!("design_{issue}.siox"));
        std::fs::write(&file, source).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
            .args([
                "--std",
                concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
                "--top",
                "Top",
                "-o",
            ])
            .arg(dir.join(format!("design_{issue}.o")))
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "#{issue}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
