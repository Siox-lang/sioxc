//! Antigravity's issues #340-#343.

use std::process::Command;

fn sioxc(name: &str, source: &str, test: bool) -> (std::path::PathBuf, std::process::Output) {
    let dir = std::env::temp_dir().join(format!("siox_issues_340_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let out = dir.join(name);
    let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    command.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")]);
    if test {
        command.arg("--test");
    }
    let output = command.arg("-o").arg(&out).arg(&file).output().unwrap();
    (out, output)
}

/// Each program asserts its values in hardware and in the testbench.
#[test]
fn programs_that_failed_now_run() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "block_local_element_slices",
            r#"module v340;
use std::bits::unsigned;
entity Top { x: unsigned[4] in, a_read: unsigned[4] out, whole: unsigned[8] out }
impl Top {
    process {
        let tarr: unsigned[8][2] = [0xAA, 0x55];
        a_read = tarr[1][7..4];
        tarr[1][7..4] = x;
        whole = tarr[1];
    }
}
#[test] entity T {}
impl T {
    let x: unsigned[4] = 0xC; let a_read: unsigned[4]; let whole: unsigned[8];
    let t: Top = { .x = x, .a_read = a_read, .whole = whole };
    process { await 1ns; print!("{:x} {:x}\n", a_read, whole); assert!(a_read == 0x5 and whole == 0xC5); }
}
"#,
            "5 c5",
        ),
        (
            "nested_instance_array",
            r#"module v341;
use std::bits::unsigned;
entity Cell { din: unsigned[8] in, dout: unsigned[8] out }
impl Cell { dout = not din; }
entity Top { a: unsigned[8][2][2] in, y: unsigned[8][2][2] out }
impl Top {
    let grid: Cell[2][2];
    for r in 0..1 { for c in 0..1 { grid[r][c].din = a[r][c]; y[r][c] = grid[r][c].dout; } }
}
#[test] entity T {}
impl T {
    let a: unsigned[8][2][2] = [[1, 2], [3, 4]]; let y: unsigned[8][2][2];
    let t: Top = { .a = a, .y = y };
    process { await 1ns; print!("{} {} {} {}\n", y[0][0], y[0][1], y[1][0], y[1][1]); assert!(y[0][0] == 254 and y[1][1] == 251 and y[1][0] == 252); }
}
"#,
            "254 253 252 251",
        ),
        (
            "negative_slice_bounds",
            r#"module test_neg_slice_bench;

entity Top {
    a: unsigned[8][-4..0] in,
    y: unsigned[8][3] out,
}

impl Top {
    y = a[-3..-1];
}

#[test]
entity TestBench {}

impl TestBench {
    let a: unsigned[8][-4..0] = [10, 20, 30, 40, 50];
    let y: unsigned[8][3];
    let dut: Top = { .a = a, .y = y };

    process {
        await 1ns;
        // Expected: y[0] == 20 (a[-3]), y[1] == 30 (a[-2]), y[2] == 40 (a[-1])
        assert!(y[0] == 20, "y[0]");
        assert!(y[1] == 30, "y[1]");
        assert!(y[2] == 40, "y[2]");
    }
}
"#,
            "1 passed",
        ),
    ];
    for (name, source, expected) in cases {
        let (binary, output) = sioxc(name, source, true);
        assert!(output.status.success(), "{name}: {output:?}");
        let ran = Command::new(&binary).output().unwrap();
        let out = String::from_utf8_lossy(&ran.stdout);
        assert!(
            out.contains(expected) && out.contains("1 passed"),
            "{name}: {out}"
        );
    }
}

/// A tuple match naming every combination of its enums covers what every
/// arm assigns; one combination short, it is a latch (#342).
#[test]
fn an_exhaustive_tuple_match_is_not_a_latch() {
    let (_, full) = sioxc(
        "full",
        r#"module test_tuple_match_latch;

enum State { A, B }
enum Mode { X, Y }

entity Top {
    s: State in,
    m: Mode in,
    y: unsigned[8] out,
}

impl Top {
    process {
        match (s, m) {
            (State::A, Mode::X) => { y = 1; }
            (State::A, Mode::Y) => { y = 2; }
            (State::B, Mode::X) => { y = 3; }
            (State::B, Mode::Y) => { y = 4; }
        }
    }
}
"#,
        false,
    );
    let full = String::from_utf8_lossy(&full.stderr);
    assert!(!full.contains("W-P002"), "{full}");
    let (_, partial) = sioxc(
        "partial",
        r#"module test_tuple_match_latch;

enum State { A, B }
enum Mode { X, Y }

entity Top {
    s: State in,
    m: Mode in,
    y: unsigned[8] out,
}

impl Top {
    process {
        match (s, m) {
            (State::A, Mode::X) => { y = 1; }
            (State::A, Mode::Y) => { y = 2; }
            (State::B, Mode::X) => { y = 3; }
        }
    }
}
"#,
        false,
    );
    let partial = String::from_utf8_lossy(&partial.stderr);
    assert!(partial.contains("W-P002"), "{partial}");
}
