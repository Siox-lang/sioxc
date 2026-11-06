//! Antigravity's issues #347-#349.

use std::process::Command;

fn sioxc(name: &str, source: &str, test: bool) -> (std::path::PathBuf, std::process::Output) {
    let dir = std::env::temp_dir().join(format!("siox_issues_347_{}_{name}", std::process::id()));
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

/// A function matching on an integer, `swap` of runtime-indexed elements,
/// and a const-generic function in hardware: each runs with its values
/// asserted.
#[test]
fn programs_that_failed_now_run() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "integer_match_function",
            r#"module test_fn_u1;

fn decode(b: unsigned[1]) -> unsigned[8] {
    match b {
        0 => { return 10; },
        1 => { return 20; },
    }
}

#[test]
entity TestBench {}

impl TestBench {
    process {
        let v0: unsigned[8] = decode(0);
        let v1: unsigned[8] = decode(1);
        assert!(v0 == 10, "v0 == 10");
        assert!(v1 == 20, "v1 == 20");
    }
}
"#,
            "1 passed",
        ),
        (
            "swap_runtime_elements",
            r#"module test_swap_dyn;

use std::mem::swap;

#[test]
entity TestBench {}

impl TestBench {
    process {
        let arr: unsigned[8][2] = [10, 20];
        let i: unsigned[2] = 0;
        let j: unsigned[2] = 1;
        swap(arr[i], arr[j]);
        assert!(arr[0] == 20, "arr[0] == 20");
        assert!(arr[1] == 10, "arr[1] == 10");
    }
}
"#,
            "1 passed",
        ),
        (
            "const_generic_hardware",
            r#"module test_probe_hw;

fn widen<W: integer>(x: unsigned[W]) -> unsigned[W + 4] {
    let res: unsigned[W + 4] = 0;
    res[W-1..0] = x;
    return res;
}

entity Top {
    a: unsigned[4] in,
    b: unsigned[8] out,
}

impl Top {
    b = widen(a);
}

#[test] entity TopTB {}
impl TopTB {
    let a: unsigned[4] = 10;
    let dut: Top = { .a = a };
    check: process {
        await 1ns;
        assert!(dut.b == 10);
    }
}
"#,
            "1 passed",
        ),
        (
            "const_generic_nested",
            r#"module g;
use std::bits::unsigned;
fn widen<W: integer>(x: unsigned[W]) -> unsigned[W + 4] { let res: unsigned[W + 4] = 0; res[W - 1..0] = x; return res; }
entity Top { a: unsigned[4] in, b: unsigned[8] out, c: unsigned[12] out }
impl Top { b = widen(a); c = widen(b); }
#[test] entity T {}
impl T { let a: unsigned[4] = 10; let b: unsigned[8]; let c: unsigned[12]; let d: Top = { .a = a, .b = b, .c = c }; process { await 1ns; let e: unsigned[8] = widen(a); print!("HW {} {} TB {}\n", b, c, e); } }
"#,
            "HW 10 10 TB 10",
        ),
    ];
    for (name, source, expected) in cases {
        let (binary, output) = sioxc(name, source, true);
        assert!(output.status.success(), "{name}: {output:?}");
        let ran = Command::new(&binary).output().unwrap();
        let out = String::from_utf8_lossy(&ran.stdout);
        assert!(
            out.contains(expected) && !out.contains("FAILED"),
            "{name}: {out}"
        );
    }
}

/// A match naming every value of an `unsigned[1]` covers what every arm
/// assigns; a value short, it is a latch (#347).
#[test]
fn an_exhaustive_numeric_match_is_not_a_latch() {
    let (_, full) = sioxc(
        "full",
        r#"module test_match_u1;

entity U1Matcher {
    b: unsigned[1] in,
    out_val: unsigned[8] out,
}

impl U1Matcher {
    process {
        match b {
            0 => { out_val = 1; },
            1 => { out_val = 2; },
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
        r#"module test_match_u1;

entity U1Matcher {
    b: unsigned[1] in,
    out_val: unsigned[8] out,
}

impl U1Matcher {
    process {
        match b {
            0 => { out_val = 1; },
        }
    }
}
"#,
        false,
    );
    let partial = String::from_utf8_lossy(&partial.stderr);
    assert!(
        partial.contains("W-P002") || partial.contains("E-P"),
        "{partial}"
    );
}
