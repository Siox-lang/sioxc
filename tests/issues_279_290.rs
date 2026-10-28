//! Antigravity's issues #279–#290. Each case is named after its GitHub issue.

use std::process::Command;

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("siox_issues_279_{}", std::process::id()));
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
            "279",
            r#"module v;
struct Point { pub x: integer, }
impl Point {
    const OFFSET: integer = 5;
    pub fn with_offset(self) -> Point { return Point { .x = self.x + OFFSET }; }
}
enum Mode { Slow, Fast, }
impl Mode {
    const STEP: integer = 3;
    pub fn step(self) -> integer { if self == Mode::Fast { return STEP * 2; } return STEP; }
}
entity Top { val: integer in, outp: integer out, s: integer out, }
impl Top {
    let p: Point = Point { .x = 0 };
    p = Point { .x = val };
    outp = p.with_offset().x;
    let m: Mode = Mode::Fast;
    s = m.step();
}
#[test] entity T {}
impl T {
    let val: integer = 10; let outp: integer; let s: integer;
    let t: Top = { .val = val, .outp = outp, .s = s };
    process { await 1ns; assert!(outp == 15, "outp"); assert!(s == 6, "s"); }
}
"#,
        ),
        (
            "280_283_285_289",
            r#"module v;
const R: range = 1..2;
const RV: range = 3..0;
struct Point { pub x: integer, pub y: integer, }
struct S { pub v: integer, }
impl S { pub fn make_array(self) -> integer[3] { return [self.v, self.v * 2, self.v * 3]; } }
fn get_arr() -> integer[4] { return [1, 2, 3, 4]; }
fn sum2(a: integer[2]) -> integer { return a[0] + a[1]; }
entity Child { p_in: Point in, p_out: Point out, }
impl Child { p_out = p_in; }
entity Pass<T> { input: T in, output: T out, }
impl<T> Pass<T> { output = input; }
entity Top {
    arr: integer[4] in, v: unsigned[8] in,
    m1: integer[2][2] in, m2: integer[2][2] in, p1: Point[2] in, p2: Point[2] in,
    s: S in, pin: Point in,
    sl: integer[2] out, vs: unsigned[4] out,
    eqm: Bool out, nem: Bool out, eqp: Bool out,
    pout: Point out, aout: integer[4] out,
    c1: integer[2] out, c2: integer[2] out, c3: integer out,
}
impl Top {
    sl = arr[R];
    vs = v[RV];
    eqm = m1 == m2;
    nem = m1 != m2;
    eqp = p1 == p2;
    let c: Child;
    c.p_in = pin;
    pout = c.p_out;
    let p: Pass<integer[4]>;
    p.input = arr;
    aout = p.output;
    c1 = get_arr()[1..2];
    c2 = s.make_array()[0..1];
    c3 = sum2(get_arr()[2..3]);
}
#[test] entity T {}
impl T {
    let arr: integer[4] = [10, 20, 30, 40]; let v: unsigned[8] = 0xA5;
    let m1: integer[2][2] = [[1, 2], [3, 4]]; let m2: integer[2][2] = [[1, 2], [3, 5]];
    let p1: Point[2] = [Point { .x = 1, .y = 2 }, Point { .x = 3, .y = 4 }];
    let p2: Point[2] = [Point { .x = 1, .y = 2 }, Point { .x = 3, .y = 4 }];
    let s: S = S { .v = 7 }; let pin: Point = Point { .x = 8, .y = 9 };
    let sl: integer[2]; let vs: unsigned[4]; let eqm: Bool; let nem: Bool; let eqp: Bool;
    let pout: Point; let aout: integer[4]; let c1: integer[2]; let c2: integer[2]; let c3: integer;
    let t: Top = { .arr = arr, .v = v, .m1 = m1, .m2 = m2, .p1 = p1, .p2 = p2, .s = s, .pin = pin,
        .sl = sl, .vs = vs, .eqm = eqm, .nem = nem, .eqp = eqp, .pout = pout, .aout = aout,
        .c1 = c1, .c2 = c2, .c3 = c3 };
    process {
        await 1ns;
        print!("{} {} {} {} {} {} {} {} {} {} {} {} {} {} {}\n", sl[0], sl[1], vs, eqm, nem, eqp, pout.x, pout.y, aout[3], c1[0], c1[1], c2[0], c2[1], c3, 0);
        assert!(sl[0] == 20 and sl[1] == 30, "named range slice");
        assert!(vs == 5, "named range vector slice");
        assert!(not eqm and nem and eqp, "composite compare");
        assert!(pout.x == 8 and pout.y == 9 and aout[3] == 40, "child aggregate ports");
        assert!(c1[0] == 2 and c1[1] == 3 and c2[0] == 7 and c2[1] == 14 and c3 == 7, "call slices");
        m2 = [[1, 2], [3, 4]];
        await 1ns;
        assert!(eqm and not nem, "composite equal");
    }
}
"#,
        ),
        (
            "281_282",
            r#"module v;
entity Top { clk: Bit in, x: integer in, y: integer out, z: integer out, }
impl Top {
    let a: integer;
    (a, _) = (x, 10);
    y = a;
    let r: integer;
    process { if clk.rising() { (r, _) = (x, 10); } }
    z = r;
}
entity Sl { arr: integer[4] in, matrix: integer[4][4] in, e: integer out, sub: integer[2] out, row: integer[4] out, }
impl Sl {
    e = arr[0..2][1];
    sub = arr[0..3][1..2];
    row = matrix[0..1][1];
}
#[test] entity T {}
impl T {
    let clk: Bit = '0'; let x: integer = 7; let y: integer; let z: integer;
    let t: Top = { .clk = clk, .x = x, .y = y, .z = z };
    let arr: integer[4] = [10, 20, 30, 40];
    let m: integer[4][4] = [[1,2,3,4],[5,6,7,8],[9,10,11,12],[13,14,15,16]];
    let e: integer; let sub: integer[2]; let row: integer[4];
    let s: Sl = { .arr = arr, .matrix = m, .e = e, .sub = sub, .row = row };
    process {
        await 1ns; clk = '1'; await 1ns;
        print!("{} {} {} {} {} {} {}\n", y, z, e, sub[0], sub[1], row[0], row[3]);
        assert!(y == 7 and z == 7 and e == 20 and sub[0] == 20 and sub[1] == 30 and row[0] == 5 and row[3] == 8);
    }
}
"#,
        ),
        (
            "284_286_287",
            r#"module v;
struct Pair { pub a: integer, pub b: integer, }
struct Point { pub x: integer, pub y: integer, }
fn step(p: Pair) -> Pair { return Pair { .a = p.b, .b = p.a + p.b }; }
fn make_arr(x: integer) -> integer[2] { return [x, x + 1]; }
const P1: Pair = Pair { .a = 1, .b = 2 };
const P2: Pair = P1;
const P3: Pair = step(Pair { .a = 1, .b = 1 });
const M: integer[2][2] = [[1, 2], [3, 4]];
const PT: Point[2] = [Point { .x = 1, .y = 2 }, Point { .x = 3, .y = 4 }];
const A1: integer[2] = [1, 2];
const A2: integer[2] = A1;
const ARR: integer[2] = make_arr(5);
entity Top {
    i: integer<0..1> in,
    o1: integer out, o2: integer out, o3: integer out, o4: integer out,
    pw: Pair out, row: integer[2] out, pt: Point out, arr: integer[2] out, a2: integer[2] out,
    dyn: integer out,
}
impl Top {
    o1 = P2.b;
    o2 = P3.b;
    o3 = M[1][0];
    o4 = ARR[1];
    pw = P3;
    row = M[1];
    pt = PT[1];
    arr = ARR;
    a2 = A2;
    dyn = M[1][i];
}
#[test] entity T {}
impl T {
    let i: integer<0..1> = 1;
    let o1: integer; let o2: integer; let o3: integer; let o4: integer;
    let pw: Pair; let row: integer[2]; let pt: Point; let arr: integer[2]; let a2: integer[2]; let dyn: integer;
    let t: Top = { .i = i, .o1 = o1, .o2 = o2, .o3 = o3, .o4 = o4, .pw = pw, .row = row, .pt = pt, .arr = arr, .a2 = a2, .dyn = dyn };
    process {
        await 1ns;
        print!("{} {} {} {} {} {} {} {} {} {} {} {} {} {}\n", o1, o2, o3, o4, pw.a, pw.b, row[0], row[1], pt.x, pt.y, arr[0], arr[1], a2[1], dyn);
        assert!(o1 == 2 and o2 == 2 and o3 == 3 and o4 == 6, "scalars");
        assert!(pw.a == 1 and pw.b == 2, "struct whole");
        assert!(row[0] == 3 and row[1] == 4, "row");
        assert!(pt.x == 3 and pt.y == 4, "struct element");
        assert!(arr[0] == 5 and arr[1] == 6 and a2[1] == 2, "arrays");
        assert!(dyn == 4, "runtime inner index");
    }
}
"#,
        ),
        (
            "288",
            r#"module test;
impl CustomOperator<"++", integer, integer> for integer {
    fn apply(self, rhs: integer) -> integer { return self + rhs + 1; }
    attr precedence = 40;
}
impl CustomOperator<"**", integer, integer> for integer {
    fn apply(self, rhs: integer) -> integer { return self * rhs * 10; }
    attr precedence = 95;
}
#[test] entity T {}
impl T { process {
    let a: integer = 3; let b: integer = 4;
    assert!((a ++ b) == 8);
    assert!((a ** b) == 120);
    assert!(a + b == 7 and a * b == 12 and a - -b == 7);
} }
entity Top { a: integer in, b: integer in, c: integer out, }
impl Top { c = a ++ b; }
"#,
        ),
        (
            "290_declared",
            r#"module v;
entity Cell { din: unsigned[8] in, dout: unsigned[8] out, }
impl Cell { dout = not din; }
entity Top { a: unsigned[8][2] in, y: unsigned[8][2] out, }
impl Top {
    let stage: Cell[2];
    for i in 0..1 {
        stage[i].din = a[i];
        y[i] = stage[i].dout;
    }
}
#[test] entity T {}
impl T {
    let a: unsigned[8][2] = [0x0F, 0xF0]; let y: unsigned[8][2];
    let t: Top = { .a = a, .y = y };
    let stage: Cell[2];
    for i in 0..1 { stage[i].din = 42; }
    process {
        await 1ns;
        print!("{} {} {} {}\n", y[0], y[1], stage[0].dout, stage[1].dout);
        assert!(y[0] == 0xF0 and y[1] == 0x0F, "hardware");
        assert!(stage[0].dout == 213 and stage[1].dout == 213, "testbench");
    }
}
"#,
        ),
        (
            "290_nameless",
            r#"module v;
entity Cell { din: unsigned[8] in, dout: unsigned[8] out, }
impl Cell { dout = not din; }
entity Top { a: unsigned[8][2] in, y: unsigned[8][2] out, }
impl Top {
    let stage: Cell[2];
    for i in 0..1 {
        stage[i] = { .din = a[i] };
        y[i] = stage[i].dout;
    }
}
#[test] entity T {}
impl T {
    let a: unsigned[8][2] = [0x0F, 0xF0]; let y: unsigned[8][2];
    let t: Top = { .a = a, .y = y };
    process { await 1ns; assert!(y[0] == 0xF0 and y[1] == 0x0F, "name-less"); }
}
"#,
        ),
        (
            "290_issue",
            r#"module test;

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
    for i in 0..1 {
        stage[i] = Cell {};
        stage[i].din = 42;
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

/// An instance array compiled as a design: its declaration made width-less
/// signals, which only the object build rejected (#290).
#[test]
fn instance_array_design_compiles() {
    let dir = scratch();
    let file = dir.join("design_290.siox");
    std::fs::write(
        &file,
        r#"module test;

entity Cell {
    din: unsigned[8] in,
    dout: unsigned[8] out,
}
impl Cell {
    dout = not din;
}

entity Top {
    a: unsigned[8][2] in,
    y: unsigned[8][2] out,
}
impl Top {
    let stage: Cell[2];
    for i in 0..1 {
        stage[i] = Cell {};
        stage[i].din = a[i];
        y[i] = stage[i].dout;
    }
}
"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--top",
            "Top",
            "-o",
        ])
        .arg(dir.join("design_290.o"))
        .arg(&file)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}");
}
