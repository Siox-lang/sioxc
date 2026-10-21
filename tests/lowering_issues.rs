//! Programs that type-checked and then failed to lower: a test binary that
//! reported "direct Process IR lowering is incomplete", or a code generation
//! error. Each is named after its GitHub issue.

use std::process::Command;

/// Build `source` as a test binary and run it, or return the compiler's
/// output when it does not build.
fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_lowering_issues_{}", std::process::id()));
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
    let mut text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        let ran = Command::new(&binary).output().unwrap();
        text += &String::from_utf8_lossy(&ran.stdout);
        text += &String::from_utf8_lossy(&ran.stderr);
    }
    text
}

/// The test passed and printed each of `lines`, in order.
fn passes(out: &str, lines: &str) {
    let mut rest = out;
    for line in lines.lines() {
        let at = rest
            .find(line)
            .unwrap_or_else(|| panic!("missing `{line}`: {out}"));
        rest = &rest[at + line.len()..];
    }
    assert!(out.contains("1 passed"), "{out}");
}

#[test]
fn issue_170_slices_are_written_from_literals_and_slices() {
    let out = run(
        "slice_writes",
        r#"module m;
#[test] entity T {}
impl T {
    process {
        let arr: integer[5] = [1, 2, 3, 4, 5];
        let other: integer[5] = [10, 20, 30, 40, 50];
        arr[1..2] = [7, 8];
        print!("{} {} {}\n", arr[0], arr[1], arr[2]);
        arr[1..2] = other[3..4];
        print!("{} {} {}\n", arr[0], arr[1], arr[2]);
        arr[1..3] = arr[0..2];
        print!("{} {} {} {}\n", arr[0], arr[1], arr[2], arr[3]);
        arr[0..2] = arr[1..3];
        print!("{} {} {} {}\n", arr[0], arr[1], arr[2], arr[3]);
    }
}
"#,
    );
    passes(&out, "1 7 8\n1 40 50\n1 1 40 50\n1 40 50 50\n");
}

#[test]
fn issue_185_hardware_slices_are_written_element_by_element() {
    let out = run(
        "hardware_slice_writes",
        r#"module m;
use std::bits::unsigned;
entity C { y: unsigned[8][4] out, }
impl C { y[0..1] = [unsigned[8](1), unsigned[8](2)]; y[2..3] = [unsigned[8](3), unsigned[8](4)]; }
entity R { clk: Bit in, y: unsigned[8][4] out, }
impl R { process { if clk.rising() { y[1..2] = [unsigned[8](5), unsigned[8](6)]; } } }
#[test] entity T {}
impl T {
    let clk: Bit = '0';
    let c: unsigned[8][4];
    let r: unsigned[8][4];
    let comb: C = { .y = c };
    let reg: R = { .clk = clk, .y = r };
    process {
        await 1ns; clk = '1'; await 1ns;
        print!("{} {} {} {} / {} {}\n", c[0], c[1], c[2], c[3], r[1], r[2]);
    }
}
"#,
    );
    passes(&out, "1 2 3 4 / 5 6\n");
}

#[test]
fn issue_182_composite_arrays_are_sliced() {
    let out = run(
        "composite_slices",
        r#"module m;
use std::bits::unsigned;
struct Pair { pub x: integer, pub y: integer }
entity Top { matrix: unsigned[8][4][4] in, sub: unsigned[8][4][2] out, ps: Pair[3] in, pq: Pair[2] out, }
impl Top { sub = matrix[1..2]; pq = ps[0..1]; }
#[test] entity T {}
impl T {
    let matrix: unsigned[8][4][4];
    let sub: unsigned[8][4][2];
    let ps: Pair[3];
    let pq: Pair[2];
    let dut: Top = { .matrix = matrix, .sub = sub, .ps = ps, .pq = pq };
    process {
        matrix[1][2] = 7; matrix[2][3] = 9; ps[1].y = 4;
        await 1ns;
        print!("{} {} {}\n", sub[0][2], sub[1][3], pq[1].y);
        let arr: Pair[3] = [Pair { .x = 1, .y = 2 }, Pair { .x = 3, .y = 4 }, Pair { .x = 5, .y = 6 }];
        let two: Pair[2] = arr[1..2];
        print!("{} {}\n", two[0].x, two[1].y);
    }
}
"#,
    );
    passes(&out, "7 9 4\n3 6\n");
}

#[test]
fn issue_181_await_takes_a_time_computed_at_run_time() {
    let out = run(
        "runtime_await",
        r#"module m;
use std::sim::time;
#[test] entity T {}
impl T {
    let flag: integer = 0;
    a: process {
        let n: integer = 5;
        let t: time = time(n * 1000000);
        await t;
        flag = 1;
        await 100ns;
    }
    b: process {
        await 4ns;
        assert!(flag == 0, "too early");
        await 2ns;
        assert!(flag == 1, "too late");
        print!("timed\n");
    }
}
"#,
    );
    passes(&out, "timed\n");
    let out = run(
        "negative_await",
        "module m;\nuse std::sim::time;\n#[test] entity T {}\n\
         impl T { process { let n: integer = -5; let t: time = time(n); await t; } }\n",
    );
    assert!(
        out.contains("`await` was given a negative time (-5 fs)"),
        "{out}"
    );
}

#[test]
fn issue_173_a_runtime_packed_slice_is_an_error() {
    let out = run(
        "runtime_packed_slice",
        "module m;\nuse std::bits::unsigned;\n#[test] entity T {}\n\
         impl T { process { let v: unsigned[16] = 0xABCD; let a: integer = 7; let b: integer = 4; \
         let s: unsigned[4] = v[a..b]; } }\n",
    );
    assert!(
        out.contains("`v[a..b]` has bounds known only at run time"),
        "{out}"
    );
}

#[test]
fn issue_176_newtypes_take_compound_literals() {
    let out = run(
        "newtype_literals",
        r#"module m;
use std::bits::unsigned;
struct Inner { pub x: unsigned[8] }
struct Wrapper(Inner);
struct Pair((integer, integer));
#[test] entity T {}
impl T { process { let w: Wrapper = Wrapper(Inner { .x = 42 }); let p: Pair = Pair((1, 2)); print!("{}\n", w.x); } }
"#,
    );
    passes(&out, "42\n");
    let out = run(
        "newtype_over_nested_array",
        "module m;\nuse std::bits::unsigned;\nstruct Table(unsigned[8][4]);\n\
         #[test] entity T {}\nimpl T { process { let t: Table; } }\n",
    );
    assert!(
        out.contains("`Table` is a newtype over an array of arrays"),
        "{out}"
    );
}
