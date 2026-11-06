//! Antigravity's issues #354-#358.

use std::process::Command;

/// Each program asserts its values in hardware and in the testbench:
/// slices of block-local and port arrays (#354), a row-slice write from a
/// local array (#356), nested literals on ports (#355), parts of constant
/// tables as initializers (#357), and slices connected to ports (#358).
#[test]
fn programs_that_failed_now_run() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "local_and_port_slices",
            r#"module v;
use std::bits::unsigned;
entity Top { clk: Bit in, row: unsigned[2] in, mat_in: unsigned[8][4] in, s0: unsigned[8] out, s1: unsigned[8] out, l1: unsigned[8] out, mem: unsigned[8][2][3] out }
impl Top {
    p: process {
        let s: unsigned[8][2] = mat_in[1..2];
        let loc: unsigned[8][3] = [5, 6, 7];
        let t: unsigned[8][2] = loc[1..];
        s0 = s[0]; s1 = s[1]; l1 = t[1];
    }
    q: process {
        if clk.rising() {
            let other: unsigned[8][2] = [mat_in[0], mat_in[3]];
            mem[row][0..1] = other;
        }
    }
}
#[test] entity T {}
impl T {
    let clk: Bit; let row: unsigned[2] = 2; let mat_in: unsigned[8][4] = [10, 20, 30, 40];
    let s0: unsigned[8]; let s1: unsigned[8]; let l1: unsigned[8]; let mem: unsigned[8][2][3];
    let d: Top = { .clk = clk, .row = row, .mat_in = mat_in, .s0 = s0, .s1 = s1, .l1 = l1, .mem = mem };
    process {
        clk = '1'; await 1ns;
        print!("{} {} {} | {} {} {}\n", s0, s1, l1, mem[2][0], mem[2][1], mem[0][0]);
        assert!(s0 == 20 and s1 == 30 and l1 == 7 and mem[2][0] == 10 and mem[2][1] == 40 and mem[0][0] == 0);
    }
}
"#,
            "20 30 7 | 10 40 0",
        ),
        (
            "issue_355",
            r#"module test_nested_literals;

use std::bits::unsigned;

struct Bundle {
    pub arr: unsigned[8][2],
}

entity Top2D {
    mat_in: unsigned[8][2][3] in,
    out_val: unsigned[8] out,
}

impl Top2D {
    out_val = mat_in[1][1];
}

entity TopBundle {
    b_in: Bundle in,
    out_val: unsigned[8] out,
}

impl TopBundle {
    out_val = b_in.arr[1];
}

#[test]
entity TestTop {}

impl TestTop {
    let dut_2d: Top2D = {
        .mat_in = [
            [10, 20],
            [30, 40],
            [50, 60]
        ]
    };

    let dut_bundle: TopBundle = {
        .b_in = Bundle {
            .arr = [11, 22]
        }
    };

    check: process {
        await 1ns;
        print!("dut_2d out_val: {}\n", dut_2d.out_val);
        print!("dut_bundle out_val: {}\n", dut_bundle.out_val);
        assert!(dut_2d.out_val == 40);
        assert!(dut_bundle.out_val == 22);
    }
}
"#,
            "1 passed",
        ),
        (
            "nested_literals_and_slice_ports",
            r#"module v;
use std::bits::unsigned;
struct Point { pub x: unsigned[8], pub y: unsigned[8] }
entity Sub { inp: unsigned[8][2] in, outp: unsigned[8][2] out }
impl Sub { outp[0] = inp[0] + 1; outp[1] = inp[1] + 1; }
entity Pts { pts: Point[2] in, s: unsigned[8] out }
impl Pts { s = pts[1].x + pts[0].y; }
entity Grid { m: unsigned[8][2][3] in, c: unsigned[8] out }
impl Grid { c = m[2][1]; }
entity Top { arr_in: unsigned[8][4] in, arr_out: unsigned[8][4] out, s: unsigned[8] out, c: unsigned[8] out }
impl Top {
    let sub: Sub = { .inp = arr_in[1..2], .outp = arr_out[1..2] };
    arr_out[0] = arr_in[0];
    arr_out[3] = arr_in[3];
    let p: Pts = { .pts = [Point { .x = 10, .y = 20 }, Point { .x = 30, .y = 40 }], .s = s };
    let g: Grid = { .m = [[1, 2], [3, 4], [5, 6]], .c = c };
}
#[test] entity T {}
impl T {
    let ai: unsigned[8][4] = [10, 20, 30, 40]; let ao: unsigned[8][4]; let s: unsigned[8]; let c: unsigned[8];
    let dut: Top = { .arr_in = ai, .arr_out = ao, .s = s, .c = c };
    let sub2: Sub = { .inp = ai[2..] };
    process {
        await 1ns;
        print!("{} {} {} {} | {} {} | {} {}\n", ao[0], ao[1], ao[2], ao[3], s, c, sub2.outp[0], sub2.outp[1]);
        assert!(ao[1] == 21 and ao[2] == 31 and ao[0] == 10 and s == 50 and c == 6 and sub2.outp[0] == 31 and sub2.outp[1] == 41);
    }
}
"#,
            "10 21 31 40 | 50 6 | 31 41",
        ),
        (
            "issue_358",
            r#"module test_unpacked_slice_connection;

use std::bits::unsigned;

entity Sub {
    inp: unsigned[8][2] in,
    outp: unsigned[8][2] out,
}

impl Sub {
    outp[0] = inp[0] + 1;
    outp[1] = inp[1] + 1;
}

entity Top {
    arr_in: unsigned[8][4] in,
    arr_out: unsigned[8][4] out,
}

impl Top {
    let sub: Sub = {
        .inp = arr_in[1..2],
        .outp = arr_out[1..2],
    };
    arr_out[0] = arr_in[0];
    arr_out[3] = arr_in[3];
}

#[test]
entity TestTop {}

impl TestTop {
    let ai: unsigned[8][4];
    let ao: unsigned[8][4];
    let dut: Top = { .arr_in = ai, .arr_out = ao };

    ai[0] = 10;
    ai[1] = 20;
    ai[2] = 30;
    ai[3] = 40;
    await 1ns;
    // Expected: ao[1] == 21, ao[2] == 31
    // Actual: ao[1] == 0, ao[2] == 0 (and compiler emits W-P011 for arr_out[1] and arr_out[2])
    assert!(ao[1] == 21, "ao[1] via sub");
    assert!(ao[2] == 31, "ao[2] via sub");
}
"#,
            "1 passed",
        ),
        (
            "issue_357",
            r#"module probe_const_array_elem_init;

use std::bits::unsigned;

const TABLE: unsigned[8][4] = [10, 20, 30, 40];

entity Top {
    val_out: unsigned[8] out,
}

impl Top {
    let x: unsigned[8] = TABLE[1];
    let sub: unsigned[8][2] = TABLE[1..2];
    val_out = x + sub[1];
}

#[test]
entity TopTB {}

impl TopTB {
    let dut: Top;

    check: process {
        await 1ns;
        print!("val_out: {}\n", dut.val_out);
        assert!(dut.val_out == 50);
    }
}
"#,
            "1 passed",
        ),
        (
            "const_table_parts",
            r#"module v357;
use std::bits::unsigned;
const TABLE: unsigned[8][4] = [10, 20, 30, 40];
entity Top { a: unsigned[8] out, b: unsigned[8] out, c: unsigned[8] out }
impl Top {
    let x: unsigned[8] = TABLE[1];
    let sub: unsigned[8][2] = TABLE[1..2];
    let tail: unsigned[8][2] = TABLE[2..];
    a = x; b = sub[1]; c = tail[1];
}
#[test] entity T {}
impl T {
    let a: unsigned[8]; let b: unsigned[8]; let c: unsigned[8];
    let d: Top = { .a = a, .b = b, .c = c };
    process { await 1ns; print!("{} {} {}\n", a, b, c); assert!(a == 20 and b == 30 and c == 40); }
}
"#,
            "20 30 40",
        ),
    ];
    for (name, source, expected) in cases {
        let dir =
            std::env::temp_dir().join(format!("siox_issues_354_{}_{name}", std::process::id()));
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
        let ran = Command::new(&binary).output().unwrap();
        let out = String::from_utf8_lossy(&ran.stdout);
        assert!(
            out.contains(expected) && out.contains("1 passed"),
            "{name}: {out}"
        );
    }
}
