//! Antigravity's issues #291–#296. Each case is named after its GitHub issue.

use std::process::Command;

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("siox_issues_291_{}", std::process::id()));
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
            "291_293",
            r#"module v;
struct S { pub v: integer, }
impl S { pub fn new(x: integer) -> S { return S { .v = x }; } }
struct Inner { pub a: integer, pub b: integer, }
struct Outer { pub inner: Inner, pub c: integer, }
const O: Outer = Outer { .inner = Inner { .a = 10, .b = 20 }, .c = 30, };
struct Pkt { pub len: integer, pub payload: integer[3], }
const PKT: Pkt = Pkt { .len = 3, .payload = [7, 8, 9] };
entity Top { y: integer out, a: integer out, c: integer out, l: integer out, p: integer out, inn: Inner out, pl: integer[3] out, }
impl Top {
    let s: S = S::new(42);
    y = s.v;
    a = O.inner.a; c = O.c; l = PKT.len; p = PKT.payload[2];
    inn = O.inner; pl = PKT.payload;
}
#[test] entity T {}
impl T {
    let y: integer; let a: integer; let c: integer; let l: integer; let p: integer; let inn: Inner; let pl: integer[3];
    let t: Top = { .y = y, .a = a, .c = c, .l = l, .p = p, .inn = inn, .pl = pl };
    process {
        let s2: S = S::new(5);
        await 1ns;
        print!("{} {} {} {} {} {} {} {}\n", y, a, c, l, p, inn.b, pl[0], s2.v);
        assert!(y == 42 and a == 10 and c == 30 and l == 3 and p == 9 and inn.b == 20 and pl[0] == 7 and s2.v == 5);
    }
}
"#,
        ),
        (
            "292_294_295_296",
            r#"module v;
struct S { pub val: integer, }
impl S {
    pub fn make(v: integer) -> Self { return S { .val = v }; }
    pub fn create(v: integer) -> S { return make(v + 1); }
}
struct Box<T> { pub val: T, }
impl<T> Box<T> { pub fn make(v: T) -> Box<T> { return Box { .val = v }; } }
entity Top {
    x: unsigned[8] in, i: integer in,
    y: unsigned[x'length] out, h: integer out,
    d: integer out, s1: integer out, s2: integer out, b1: integer out,
    tail: unsigned[8][2] out, head: unsigned[8][2] out, rows: integer[2][2] out,
}
impl Top {
    pub fn double(v: integer) -> integer { return v * 2; }
    let tmp: unsigned[x'length];
    tmp = x;
    y = tmp;
    h = x'high;
    d = double(i);
    let s: S; s = S::make(i); s1 = s.val;
    let c: S; c = S::create(i); s2 = c.val;
    let b: Box<integer>; b = Box<integer>::make(i); b1 = b.val;
    let arr: unsigned[8][4];
    for k in 0..3 { arr[k] = x + k; }
    tail = arr[2..];
    head = arr[..1];
    let grid: integer[2][3];
    grid[0] = [1, 2]; grid[1] = [3, 4]; grid[2] = [5, 6];
    rows = grid[1..];
}
#[test] entity T {}
impl T {
    pub fn triple(v: integer) -> integer { return v * 3; }
    let x: unsigned[8] = 10; let i: integer = 7;
    let y: unsigned[8]; let h: integer; let d: integer; let s1: integer; let s2: integer; let b1: integer;
    let tail: unsigned[8][2]; let head: unsigned[8][2]; let rows: integer[2][2];
    let t: Top = { .x = x, .i = i, .y = y, .h = h, .d = d, .s1 = s1, .s2 = s2, .b1 = b1, .tail = tail, .head = head, .rows = rows };
    let z: integer = triple(14);
    process {
        await 1ns;
        print!("{} {} {} {} {} {} {} {} {} {} {}\n", y, h, d, s1, s2, b1, tail[0], tail[1], head[1], rows[0][0], rows[1][1]);
        assert!(y == 10 and h == 7, "295");
        assert!(d == 14 and z == 42, "292");
        assert!(s1 == 7 and s2 == 8 and b1 == 7, "294");
        assert!(tail[0] == 12 and tail[1] == 13 and head[1] == 11, "296 1d");
        assert!(rows[0][0] == 3 and rows[1][1] == 6, "296 2d");
    }
}
"#,
        ),
        (
            "292_testbench",
            r#"module test;

#[test]
entity TestTop {}
impl TestTop {
    pub fn double(v: integer) -> integer {
        return v * 2;
    }
    let y: integer = double(21);
    assert!(y == 42);
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
            "292",
            r#"module test;

entity Top {
    x: integer in,
    y: integer out,
}
impl Top {
    pub fn double(v: integer) -> integer {
        return v * 2;
    }

    y = double(x);
}
"#,
        ),
        (
            "294",
            r#"module test;

struct Box<T> {
    pub val: T,
}

impl<T> Box<T> {
    pub fn make(v: T) -> Box<T> {
        return Box { .val = v };
    }
}

entity Top {
    x: integer in,
    y: integer out,
}

impl Top {
    let b: Box<integer>;
    b = Box<integer>::make(x);
    y = b.val;
}
"#,
        ),
        (
            "295",
            r#"module test;

use std::bits::unsigned;

entity Top {
    x: unsigned[8] in,
    y: unsigned[x'length] out,
}

impl Top {
    y = x;
}
"#,
        ),
        (
            "296",
            r#"module test;

use std::bits::unsigned;

entity Top {
    x: unsigned[8] in,
    y: unsigned[8] out,
}

impl Top {
    let grid: unsigned[8][3][2];
    grid[0][0] = x;
    grid[0][1] = x + 1;
    grid[0][2] = x + 2;
    grid[1][0] = x + 3;
    grid[1][1] = x + 4;
    grid[1][2] = x + 5;

    let sub_grid: unsigned[8][3][1];
    sub_grid = grid[1..];
    y = sub_grid[0][2];
}

#[test]
entity TestTop {}
impl TestTop {
    let x: unsigned[8] = 10;
    let y: unsigned[8];
    let t: Top = { .x = x, .y = y };
    check: process {
        await 1ns;
        print!("|y={}\n", y);
        assert!(y == 15);
    }
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
