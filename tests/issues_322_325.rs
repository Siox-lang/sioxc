//! Antigravity's issues #322–#325. Each case is named after its GitHub issue.

use std::process::Command;

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("siox_issues_322_{}", std::process::id()));
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
            "322_325_values",
            r#"module v;
use std::bits::unsigned;
entity Top {
    idx: unsigned[3] in, i: unsigned[2] in,
    m: unsigned[8][2][4] in, a: unsigned[8] in, b: unsigned[4] in,
    c1: Char out, c2: Char out, s: Char[3] out,
    row: unsigned[8][2] out,
    x: unsigned[8] out, o: unsigned[8] out,
}
impl Top {
    c1 = "hello"[1];
    c2 = "hello"[idx];
    s = "hello"[1..3];
    row = m[i][0..1];
    x = a and b;
    o = a or b;
}
#[test] entity T {}
impl T {
    let idx: unsigned[3] = 4; let i: unsigned[2] = 2;
    let m: unsigned[8][2][4] = [[1, 2], [3, 4], [5, 6], [7, 8]];
    let a: unsigned[8] = 0xF5; let b: unsigned[4] = 0x5;
    let c1: Char; let c2: Char; let s: Char[3]; let row: unsigned[8][2]; let x: unsigned[8]; let o: unsigned[8];
    let t: Top = { .idx = idx, .i = i, .m = m, .a = a, .b = b, .c1 = c1, .c2 = c2, .s = s, .row = row, .x = x, .o = o };
    process {
        await 1ns;
        let y: unsigned[8] = a xor b;
        print!("{} {} {}{}{} {} {} {:x} {:x} {:x}\n", c1, c2, s[0], s[1], s[2], row[0], row[1], x, o, y);
        assert!(c1 == 'e' and c2 == 'o' and s[0] == 'e' and s[2] == 'l', "strings");
        assert!(row[0] == 5 and row[1] == 6, "row slice");
        assert!(x == 0x05 and o == 0xF5 and y == 0xF0, "mixed widths");
        i = 3; idx = 0; await 1ns;
        assert!(row[1] == 8 and c2 == 'h', "changed");
    }
}
"#,
        ),
        (
            "322",
            r#"module test;
use std::bits::unsigned;

#[test]
entity TestAnd {
}

impl TestAnd {
    main: process {
        let a: unsigned[8] = 0xF5;
        let b: unsigned[4] = 0x5;
        let c: unsigned[8] = a and b;
        assert!(c == 0x5);
    }
}
"#,
        ),
        (
            "325_tb",
            r#"module test;
use std::bits::unsigned;

#[test]
entity TestStrLit {}

impl TestStrLit {
    process {
        let ch: Char = "hello"[1];
        assert!(ch == 'e');

        let sub: Char[3] = "hello"[1..3];
        assert!(sub[0] == 'e');
        assert!(sub[1] == 'l');
        assert!(sub[2] == 'l');

        let idx: unsigned[2] = 1;
        let d: Char = "hello"[idx];
        assert!(d == 'e');
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
            "323_0",
            r#"module test;
use std::bits::unsigned;

entity Top {
    din: unsigned[8][4][4] in,
    idx: unsigned[2] in,
    dout: unsigned[8][2] out,
}

impl Top {
    dout = din[idx][0..1];
}
"#,
        ),
        (
            "323_1",
            r#"module test;
use std::logic::Logic;
use std::bits::unsigned;

entity Top {
    clk: Logic in,
    din: unsigned[8][4][4] in,
    idx: unsigned[2] in,
    dout: unsigned[8][2] out,
}

impl Top {
    main: process {
        if clk.rising() {
            dout = din[idx][0..1];
        }
    }
}
"#,
        ),
        (
            "325_0",
            r#"module test;

entity Top {
    ch: Char out,
}

impl Top {
    ch = "hello"[1];
}
"#,
        ),
        (
            "325_1",
            r#"module test;

entity Top {
    sub: Char[3] out,
}

impl Top {
    sub = "hello"[1..3];
}
"#,
        ),
        (
            "325_2",
            r#"module test;
use std::bits::unsigned;

entity Top {
    idx: unsigned[2] in,
    dout: Char out,
}

impl Top {
    dout = "hello"[idx];
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

/// An array of an enum has no constructor, as an array of `integer` has
/// none: `Bit[4](din)` passed the checker and failed to lower (#324).
#[test]
fn an_enum_array_has_no_constructor() {
    let out = run(
        "enum_array_constructor",
        r#"module test;
use std::logic::Bit;
use std::bits::unsigned;

entity Top {
    din: unsigned[4] in,
    dout: Bit[4] out,
}

impl Top {
    dout = Bit[4](din);
}
"#,
    );
    assert!(
        out.contains("`Bit[4]` is an array, not a vector family, and has no constructor"),
        "{out}"
    );
}
