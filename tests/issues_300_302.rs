//! Antigravity's issues #300–#302. Each case is named after its GitHub issue.

use std::process::Command;

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("siox_issues_300_{}", std::process::id()));
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
            "300_direct",
            r#"module test;

use std::bits::unsigned;

entity Cell {
    din: unsigned[8] in,
    dout: unsigned[8] out,
}

impl Cell {
    dout = din + 1;
}

entity Top {
    data: unsigned[8] in,
    out_val: unsigned[8] out,
}

impl Top {
    let grid: Cell[2][2];
    for r in 0..1 {
        for c in 0..1 {
            grid[r][c].din = data;
        }
    }
    out_val = grid[1][1].dout;
}

#[test]
entity TestTopDirect {}
impl TestTopDirect {
    let data: unsigned[8] = 50;
    let out_val: unsigned[8];
    let t: Top = { .data = data, .out_val = out_val };
    run: process {
        await 1ns;
        assert!(out_val == 51);
    }
}
"#,
        ),
        (
            "300_dynamic",
            r#"module test;

use std::bits::unsigned;

entity Cell {
    din: unsigned[8] in,
    dout: unsigned[8] out,
}

impl Cell {
    dout = din + 1;
}

entity Top {
    r: unsigned[1] in,
    c: unsigned[1] in,
    data: unsigned[8] in,
    out_val: unsigned[8] out,
}

impl Top {
    let grid: Cell[2][2];
    for row in 0..1 {
        for col in 0..1 {
            grid[row][col].din = data + row * 10 + col;
        }
    }
    out_val = grid[r][c].dout;
}

#[test]
entity TestTopDynamic {}
impl TestTopDynamic {
    let r: unsigned[1] = 1;
    let c: unsigned[1] = 0;
    let data: unsigned[8] = 100;
    let out_val: unsigned[8];
    let t: Top = { .r = r, .c = c, .data = data, .out_val = out_val };
    run: process {
        await 1ns;
        assert!(out_val == 111);
    }
}
"#,
        ),
        (
            "301_fixed_slices",
            r#"module v;
use std::fixed::ufixed;
entity Top { x: ufixed<8, 4> in, s: ufixed<3, 2> out, s1: ufixed<6, 2> out, s2: ufixed<5, 4> out, }
impl Top { s = x[0..-2]; s1 = x[..-2]; s2 = x[0..]; }
#[test] entity T {}
impl T {
    let x: ufixed<8, 4> = ufixed<8, 4>(2.75);
    let s: ufixed<3, 2>; let s1: ufixed<6, 2>; let s2: ufixed<5, 4>;
    let t: Top = { .x = x, .s = s, .s1 = s1, .s2 = s2 };
    process {
        await 1ns;
        print!("{} {} {}\n", real(s), real(s1), real(s2));
        assert!(real(s) == 0.75 and real(s1) == 2.75 and real(s2) == 0.75);
    }
}
"#,
        ),
        (
            "302",
            r#"module test;

#[test]
entity TestTupleRangeMatch {}

impl TestTupleRangeMatch {
    run: process {
        let p: (integer, integer) = (5, 5);
        let r: integer = match p {
            (..0, 1..) => 1,
            (0..10, 0..10) => 2,
            _ => 3,
        };
        print!("r = {}\n", r);
        assert!(r == 2);
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
            "300",
            r#"module test;

use std::bits::unsigned;

entity Cell {
    din: unsigned[8] in,
    dout: unsigned[8] out,
}

impl Cell {
    dout = din + 1;
}

entity Top {
    data: unsigned[8] in,
    out_val: unsigned[8] out,
}

impl Top {
    let grid: Cell[2][2];
    for r in 0..1 {
        for c in 0..1 {
            grid[r][c].din = data;
        }
    }
    out_val = grid[1][1].dout;
}

#[test]
entity TestTopDirect {}
impl TestTopDirect {
    let data: unsigned[8] = 50;
    let out_val: unsigned[8];
    let t: Top = { .data = data, .out_val = out_val };
    run: process {
        await 1ns;
        assert!(out_val == 51);
    }
}
"#,
        ),
        (
            "301",
            r#"module test;

use std::fixed::ufixed;

entity Top {
    x: ufixed<8, 4> in,
    s: ufixed<3, 2> out,
}

impl Top {
    s = x[0..-2];
}
"#,
        ),
        (
            "302",
            r#"module test;

entity Top {
    a: integer in,
    b: integer in,
    y: integer out,
}

impl Top {
    y = match (a, b) {
        (..0, 1..) => 1,
        (1..10, 1..10) => 2,
        _ => 3,
    };
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
