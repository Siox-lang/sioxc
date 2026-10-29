//! Antigravity's issues #314–#320. Each case is named after its GitHub issue.

use std::process::Command;

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("siox_issues_314_{}", std::process::id()));
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
            "314_320_values",
            r#"module v;
use std::bits::unsigned;
fn sum2(x: unsigned[8][2]) -> unsigned[8] { return x[0] + x[1]; }
fn get_val() -> unsigned[8] { return 0xA5; }
entity Top {
    clk: Logic in,
    sel: unsigned[1] in, idx: unsigned[2] in, r: unsigned[2] in,
    arr: unsigned[8][4] in, a: unsigned[8] in, b: unsigned[8] in, din: unsigned[8][2] in, d: unsigned[8] in,
    s314: unsigned[8] out, m314: unsigned[8][2] out, i314: unsigned[8][2] out,
    s315: unsigned[8] out,
    bit1: Logic out, bit2: Logic out,
    l1: unsigned[8] out, l2: unsigned[8][2] out,
    w319: unsigned[8][4] out,
    w320: unsigned[8][2][4] out,
    q320: unsigned[8] out,
}
impl Top {
    s314 = sum2(arr[1..2]);
    i314 = if sel == 0 { arr[0..1] } else { arr[2..3] };
    m314 = match sel { 0 => arr[0..1], _ => arr[2..3] };
    s315 = arr[1..3][idx];
    bit1 = (a + b)[idx];
    bit2 = get_val()[idx];
    l1 = [a, b, d, 7][idx];
    l2 = [a, b, d, 7][1..2];
    w319[0..1] = din;
    w319[2..3] = din;
    let mem: unsigned[8][2][4];
    reg: process {
        if clk.rising() {
            mem[r][0..1] = [d, d + 1];
        }
    }
    q320 = mem[2][1];
    w320[r][0..1] = [d, d];
}
#[test] entity T {}
impl T {
    let clk: Logic = '0';
    let sel: unsigned[1] = 1; let idx: unsigned[2] = 1; let r: unsigned[2] = 2;
    let arr: unsigned[8][4] = [10, 20, 30, 40]; let a: unsigned[8] = 3; let b: unsigned[8] = 4;
    let din: unsigned[8][2] = [5, 6]; let d: unsigned[8] = 9;
    let s314: unsigned[8]; let m314: unsigned[8][2]; let i314: unsigned[8][2]; let s315: unsigned[8];
    let bit1: Logic; let bit2: Logic; let l1: unsigned[8]; let l2: unsigned[8][2];
    let w319: unsigned[8][4]; let w320: unsigned[8][2][4]; let q320: unsigned[8];
    let t: Top = { .clk = clk, .sel = sel, .idx = idx, .r = r, .arr = arr, .a = a, .b = b, .din = din, .d = d,
        .s314 = s314, .m314 = m314, .i314 = i314, .s315 = s315, .bit1 = bit1, .bit2 = bit2,
        .l1 = l1, .l2 = l2, .w319 = w319, .w320 = w320, .q320 = q320 };
    process {
        await 1ns; clk = '1'; await 1ns;
        print!("{} {} {} {} {} {} {} {} {} {} {} {}\n", s314, i314[0], m314[1], s315, bit1, bit2, l1, l2[0], w319[3], w320[2][1], q320, w319[0]);
        assert!(s314 == 50 and i314[0] == 30 and m314[1] == 40 and s315 == 30, "slices");
        assert!(bit1 == '1' and bit2 == '0' and l1 == 4 and l2[0] == 4 and l2[1] == 9, "computed and literals");
        assert!(w319[0] == 5 and w319[3] == 6 and w320[2][1] == 9 and q320 == 10, "slice writes");
        idx = 2; sel = 0; await 1ns;
        assert!(s315 == 40 and i314[1] == 20 and bit1 == '1' and bit2 == '1' and l1 == 9, "changed indices");
    }
}
"#,
        ),
        (
            "315_tb",
            r#"module test;
use std::bits::unsigned;

#[test]
entity Tb {}
impl Tb {
    test: process {
        let row: unsigned[8][4];
        row[0] = 10;
        row[1] = 20;
        row[2] = 30;
        row[3] = 40;
        let idx: unsigned[2] = 1;
        let v: unsigned[8] = row[0..2][idx];
        assert!(v == 20);
    }
}
"#,
        ),
        (
            "316_tb",
            r#"module test;
use std::bits::unsigned;
use std::logic::Logic;

#[test]
entity TestExprBitTb {}
impl TestExprBitTb {
    run: process {
        let a: unsigned[8] = 0b0000_1000;
        let b: unsigned[8] = 0b0000_0010;
        let idx: unsigned[8] = 3;
        let bit: Logic = (a + b)[idx];
        assert!(bit == '1');
    }
}
"#,
        ),
        (
            "317",
            r#"module test;
use std::bits::unsigned;

#[test]
entity Tb {}
impl Tb {
    test: process {
        let arr: unsigned[8][4];
        arr[0] = 10;
        arr[1] = 20;
        arr[2] = 30;
        arr[3] = 40;
        let lo: unsigned[2] = 1;
        let hi: unsigned[2] = 2;
        let sub: unsigned[8][] = arr[lo..hi];
        assert!(sub[0] == 20);
        assert!(sub[1] == 30);
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
            "314_0",
            r#"module test;
use std::bits::unsigned;

fn sum2(x: unsigned[8][2]) -> unsigned[8] {
    return x[0] + x[1];
}

entity Top {
    arr: unsigned[8][4] in,
    dout: unsigned[8] out,
}

impl Top {
    dout = sum2(arr[0..1]);
}
"#,
        ),
        (
            "314_1",
            r#"module test;
use std::bits::unsigned;

entity Top {
    sel: unsigned[1] in,
    arr: unsigned[8][4] in,
    dout: unsigned[8][2] out,
}

impl Top {
    dout = if sel == 0 { arr[0..1] } else { arr[2..3] };
}
"#,
        ),
        (
            "314_2",
            r#"module test;
use std::bits::unsigned;

entity Top {
    sel: unsigned[1] in,
    arr: unsigned[8][4] in,
    dout: unsigned[8][2] out,
}

impl Top {
    dout = match sel {
        0 => arr[0..1],
        _ => arr[2..3],
    };
}
"#,
        ),
        (
            "315_0",
            r#"module test;
use std::bits::unsigned;

entity Top {
    row: unsigned[8][4] in,
    idx: unsigned[2] in,
    dout: unsigned[8] out,
}
impl Top {
    dout = row[0..2][idx];
}
"#,
        ),
        (
            "316_0",
            r#"module test;
use std::bits::unsigned;
use std::logic::Logic;

entity Top {
    a: unsigned[8] in,
    b: unsigned[8] in,
    idx: unsigned[3] in,
    dout: Logic out,
}
impl Top {
    dout = (a + b)[idx];
}
"#,
        ),
        (
            "316_1",
            r#"module test;
use std::bits::unsigned;
use std::logic::Logic;

fn get_val() -> unsigned[8] {
    return 42;
}

entity Top {
    idx: unsigned[3] in,
    dout: Logic out,
}
impl Top {
    dout = get_val()[idx];
}
"#,
        ),
        (
            "318_0",
            r#"module test;
use std::bits::unsigned;

entity Top {
    a: unsigned[8] in,
    b: unsigned[8] in,
    c: unsigned[8] in,
    d: unsigned[8] in,
    idx: unsigned[2] in,
    x: unsigned[8] out,
    y: unsigned[8][2] out,
}

impl Top {
    x = [a, b, c, d][idx];
    y = [a, b, c, d][1..2];
}
"#,
        ),
        (
            "319_0",
            r#"module test;
use std::bits::unsigned;

entity Top {
    din: unsigned[8][2] in,
    dout: unsigned[8][4] out,
}

impl Top {
    dout[0..1] = din;
}
"#,
        ),
        (
            "319_1",
            r#"module test;
use std::logic::Logic;
use std::bits::unsigned;

entity Top {
    clk: Logic in,
    din: unsigned[8][2] in,
    dout: unsigned[8][4] out,
}

impl Top {
    reg: process {
        if clk.rising() {
            dout[0..1] = din;
        }
    }
}
"#,
        ),
        (
            "320_0",
            r#"module test;
use std::bits::unsigned;

entity Top {
    din: unsigned[8] in,
    i: unsigned[2] in,
    dout: unsigned[8][4][4] out,
}

impl Top {
    dout[i][0..1] = [din, din];
}
"#,
        ),
        (
            "320_1",
            r#"module test;
use std::logic::Logic;
use std::bits::unsigned;

entity Top {
    clk: Logic in,
    din: unsigned[8] in,
    i: unsigned[2] in,
    dout: unsigned[8] out,
}

impl Top {
    let mem: unsigned[8][4][4];
    reg: process {
        if clk.rising() {
            mem[i][0..1] = [din, din];
        }
    }
    dout = mem[0][0];
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
