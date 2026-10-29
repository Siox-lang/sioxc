//! Antigravity's issues #330-#338.

use std::process::Command;

fn run(name: &str, source: &str, extra: &[(&str, &str)]) -> String {
    let dir = std::env::temp_dir().join(format!("siox_issues_330_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file, text) in extra {
        std::fs::write(dir.join(file), text).unwrap();
    }
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
    }
    text
}

/// The diagnostics a design object build reports.
fn build(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_issues_330_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std"), "-o"])
        .arg(dir.join(format!("{name}.o")))
        .arg(&file)
        .output()
        .unwrap();
    assert!(output.status.success(), "{name}: {output:?}");
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// Programs that failed to lower or to run, each asserting its values in
/// hardware and in the testbench.
#[test]
fn programs_that_failed_now_run() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "generic_tuple_swap",
            r#"module t330;
fn swap<A, B>(pair: (A, B)) -> (B, A) { return (pair.1, pair.0); }
entity Top { a: integer in, b: Bool in, p: (Bool, integer) out, r_a: Bool out, r_b: integer out }
impl Top {
    p = swap((a, b));
    (r_a, r_b) = swap((a, b));
}
#[test] entity T {}
impl T {
    let a: integer = 5; let b: Bool = true; let p: (Bool, integer); let r_a: Bool; let r_b: integer;
    let t: Top = { .a = a, .b = b, .p = p, .r_a = r_a, .r_b = r_b };
    process {
        await 1ns;
        let s: (Bool, integer) = swap((a, b));
        print!("{} {} {} {} {} {}\n", p.0, p.1, r_a, r_b, s.0, s.1);
        assert!(p.0 and p.1 == 5 and r_a and r_b == 5 and s.1 == 5);
    }
}
"#,
            "1 passed",
        ),
        (
            "concat_member_places",
            r#"module test_concat_struct_tb;
use std::bits::unsigned;

struct Point {
    pub x: unsigned[4],
    pub y: unsigned[4],
}

#[test]
entity TB {}
impl TB {
    process {
        let in_val: unsigned[8] = 0xAB;
        let p: Point = Point { .x = 0, .y = 0 };
        { p.x, p.y } = in_val;
        print!("{:x} {:x}\n", p.x, p.y);
    }
}
"#,
            "a b",
        ),
        (
            "generic_composite_params",
            r#"module p333;
struct Box<T> { pub val: T }
struct Pair<A, B> { pub a: A, pub b: B }
fn wrap_box<T>(b: Box<T>) -> Box<T> { return Box { .val = b.val }; }
fn reverse<T>(arr: T[2]) -> T[2] { return [arr[1], arr[0]]; }
fn swap_pair<A, B>(p: Pair<A, B>) -> Pair<B, A> { return Pair { .a = p.b, .b = p.a }; }
entity Top { v_in: integer in, v_out: integer out, r0: integer out, s: Bool out }
impl Top {
    let b: Box<integer>; b.val = v_in;
    let b2: Box<integer>; b2 = wrap_box(b);
    v_out = b2.val;
    let arr: integer[2]; arr[0] = v_in; arr[1] = 7;
    let rev: integer[2]; rev = reverse(arr);
    r0 = rev[0];
    let p: Pair<integer, Bool>; p.a = v_in; p.b = true;
    let q: Pair<Bool, integer>; q = swap_pair(p);
    s = q.a;
}
#[test] entity T {}
impl T {
    let v_in: integer = 5; let v_out: integer; let r0: integer; let s: Bool;
    let t: Top = { .v_in = v_in, .v_out = v_out, .r0 = r0, .s = s };
    process { await 1ns; print!("{} {} {}\n", v_out, r0, s); assert!(v_out == 5 and r0 == 7 and s); }
}
"#,
            "1 passed",
        ),
        (
            "generic_method_and_tuple",
            r#"module p333b;
struct Box<T> { pub val: T }
impl<T> Box<T> { pub fn clone_box(self) -> Box<T> { return Box { .val = self.val }; } }
fn swap_mono(pair: (integer, Bool)) -> (Bool, integer) { return (pair.1, pair.0); }
entity Top { v_in: integer in, v_out: integer out, r_a: Bool out, r_b: integer out }
impl Top {
    let b: Box<integer>; b.val = v_in;
    let b2: Box<integer>; b2 = b.clone_box();
    v_out = b2.val;
    (r_a, r_b) = swap_mono((v_in, true));
}
#[test] entity T {}
impl T {
    let v_in: integer = 5; let v_out: integer; let r_a: Bool; let r_b: integer;
    let t: Top = { .v_in = v_in, .v_out = v_out, .r_a = r_a, .r_b = r_b };
    process { await 1ns; print!("{} {} {}\n", v_out, r_a, r_b); assert!(v_out == 5 and r_a and r_b == 5); }
}
"#,
            "1 passed",
        ),
        (
            "sized_trait_args",
            r#"module v334;
use std::bits::unsigned;
struct Value { pub v: unsigned[8] }
impl From<unsigned[8]> for Value { fn from(v: unsigned[8]) -> Value { return Value { .v = v + 1 }; } }
impl Ord<unsigned[8]> for Value {
    fn lt(self, rhs: unsigned[8]) -> Bool { return self.v < rhs; }
    fn le(self, rhs: unsigned[8]) -> Bool { return self.v <= rhs; }
    fn gt(self, rhs: unsigned[8]) -> Bool { return self.v > rhs; }
    fn ge(self, rhs: unsigned[8]) -> Bool { return self.v >= rhs; }
}
impl Eq<unsigned[8]> for Value { fn eq(self, rhs: unsigned[8]) -> Bool { return self.v == rhs; } }
struct Meter { pub count: unsigned[8] }
impl From<Meter> for integer { fn from(m: Meter) -> integer { return integer(m.count) * 10; } }
entity Top { a_v: unsigned[8] in, limit: unsigned[8] in, lt: Bool out, gt: Bool out, eq: Bool out, m_in: Meter in, n: integer out }
impl Top {
    let a: Value; a = Value(a_v);
    lt = a < limit; gt = a > limit; eq = a == limit;
    n = integer(m_in);
}
#[test] entity T {}
impl T {
    let a_v: unsigned[8] = 30; let limit: unsigned[8] = 20; let lt: Bool; let gt: Bool; let eq: Bool;
    let m: Meter = Meter { .count = 7 }; let n: integer;
    let t: Top = { .a_v = a_v, .limit = limit, .lt = lt, .gt = gt, .eq = eq, .m_in = m, .n = n };
    process {
        await 1ns;
        let b: Value = Value(a_v);
        print!("{} {} {} {} {} {}\n", lt, gt, eq, n, b > limit, integer(m));
        assert!(not lt and gt and not eq and n == 70 and b > limit and integer(m) == 70);
        limit = 31; await 1ns;
        assert!(eq and not gt and not lt);
    }
}
"#,
            "1 passed",
        ),
        (
            "kernel_from_enum",
            r#"module main;

enum Opcode {
    Add = 0,
    Sub = 1,
}

impl From<Opcode> for integer {
    fn from(op: Opcode) -> integer {
        match op {
            Opcode::Add => return 100,
            Opcode::Sub => return 200,
        }
    }
}

entity Converter {
    op_in: Opcode in,
    val_out: integer out,
}

impl Converter {
    val_out = integer(op_in);
}

#[test]
entity TestBench {}

impl TestBench {
    let op: Opcode;
    let hw_val: integer;
    let c: Converter = {
        .op_in = op,
        .val_out = hw_val,
    };

    process {
        op = Opcode::Sub;
        await 1ns;
        let tb_val: integer = integer(Opcode::Sub);
        assert!(tb_val == 200); // PASSES: testbench invokes From::from
        assert!(hw_val == 200); // FAILS: hw_val is 1 because hardware sliced raw discriminant!
    }
}
"#,
            "1 passed",
        ),
        (
            "computed_aggregate_fields",
            r#"module p338;
struct Complex { pub re: integer, pub im: integer }
impl Add<Complex, Complex> for Complex {
    fn add(self, rhs: Complex) -> Complex { return Complex { .re = self.re + rhs.re, .im = self.im + rhs.im }; }
}
impl Neg<Complex> for Complex {
    fn neg(self) -> Complex { return Complex { .re = 0 - self.re, .im = 0 - self.im }; }
}
entity Top { cond: Bool in, c1: Complex in, c2: Complex in, out_if: integer out, out_add: integer out, out_neg: integer out, out_match: integer out }
impl Top {
    out_if = (if cond { c1 } else { c2 }).re;
    out_add = (c1 + c2).im;
    out_neg = (-c1).re;
    out_match = (match cond { true => c2, false => c1 }).im;
}
#[test] entity T {}
impl T {
    let cond: Bool = true; let c1: Complex = Complex { .re = 3, .im = 4 }; let c2: Complex = Complex { .re = 10, .im = 20 };
    let out_if: integer; let out_add: integer; let out_neg: integer; let out_match: integer;
    let t: Top = { .cond = cond, .c1 = c1, .c2 = c2, .out_if = out_if, .out_add = out_add, .out_neg = out_neg, .out_match = out_match };
    process {
        await 1ns;
        print!("{} {} {} {}\n", out_if, out_add, out_neg, out_match);
        assert!(out_if == 3 and out_add == 24 and out_neg == -3 and out_match == 20);
        cond = false; await 1ns;
        assert!(out_if == 10 and out_match == 4);
    }
}
"#,
            "1 passed",
        ),
    ];
    for (name, source, expected) in cases {
        let out = run(name, source, &[]);
        assert!(
            out.contains(expected) && !out.contains("FAILED"),
            "{name}: {out}"
        );
    }
}

/// `pkg::ORIGIN`, a struct constant named through its module, read whole
/// and inside a literal (#336).
#[test]
fn a_qualified_struct_constant_lowers() {
    let pkg = r#"module pkg;

use std::bits::unsigned;

pub struct Point {
    pub x: unsigned[8],
    pub y: unsigned[8],
}

pub const ORIGIN: Point = Point { .x = 10, .y = 20 };
"#;
    let out = run(
        "main",
        r#"module main;
use std::bits::unsigned;
use pkg::Point;
struct Outer { pub pt: Point }
entity Top { p_out: Point out, o_out: Outer out }
impl Top {
    p_out = pkg::ORIGIN;
    o_out = Outer { .pt = pkg::ORIGIN };
}
#[test] entity T {}
impl T {
    let p: Point; let o: Outer;
    let t: Top = { .p_out = p, .o_out = o };
    process {
        await 1ns;
        let q: Point = pkg::ORIGIN;
        print!("{} {} {} {} {}\n", p.x, p.y, o.pt.x, o.pt.y, q.y);
        assert!(p.x == 10 and p.y == 20 and o.pt.x == 10 and o.pt.y == 20 and q.y == 20);
    }
}
"#,
        &[("pkg.siox", pkg)],
    );
    assert!(
        out.contains("10 20 10 20 20") && out.contains("1 passed"),
        "{out}"
    );
}

/// A match whose `_` arm completes it covers what every arm assigns, an
/// aggregate included; a target one arm leaves is a latch (#331).
#[test]
fn a_wildcard_match_covers_only_what_every_arm_assigns() {
    let covered = build(
        "covered",
        r#"module test_match_array;

entity Top {
    sel: integer in,
    arr: integer[2] out,
}
impl Top {
    match sel {
        0 => { arr = [1, 2]; }
        _ => { arr = [3, 4]; }
    }
}
"#,
    );
    assert!(!covered.contains("W-P002"), "{covered}");
    let latch = build(
        "latch",
        r#"module test_false_negative_latch;

entity Top {
    sel: integer in,
    y: integer out,
}
impl Top {
    match sel {
        0 => { }
        _ => { y = 1; }
    }
}
"#,
    );
    assert!(
        latch.contains("W-P002") && latch.contains("`Top.y`"),
        "{latch}"
    );
}

/// `a > limit` through `impl Ord<unsigned> for Value` without `gt`: its
/// default compares `limit < a`, which nothing defines (#335).
#[test]
fn a_cross_type_greater_than_needs_gt() {
    let out = run(
        "cross_order",
        r#"module test;

use std::bits::unsigned;

struct Value {
    pub v: unsigned[8],
}

impl Ord<unsigned> for Value {
    fn lt(self, rhs: unsigned[8]) -> Bool {
        return self.v < rhs;
    }
    fn le(self, rhs: unsigned[8]) -> Bool {
        return self.v <= rhs;
    }
}

entity Top {
    a_v: unsigned[8] in,
    limit: unsigned[8] in,
    is_lt: Bool out,
    is_gt: Bool out,
}

impl Top {
    let a: Value;
    a.v = a_v;
    is_lt = a < limit;   // Compiles and lowers cleanly
    is_gt = a > limit;   // Fails Stage 6 with E-P017
}
"#,
        &[],
    );
    assert!(
        out.contains("needs `gt` in `impl Ord<unsigned> for Value`"),
        "{out}"
    );
}
