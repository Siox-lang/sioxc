//! Antigravity's issues #231–#235. Each case is named after its GitHub issue.

use std::process::Command;

/// Build `source` as a test binary and run it, returning everything printed.
fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_issues_231_{}", std::process::id()));
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

#[test]
fn generic_methods_lower() {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "234",
            r#"module repro_generic_method;

struct Dummy {
    pub dummy: integer,
}

struct Pair {
    pub a: integer,
    pub b: integer,
}

impl Dummy {
    pub fn transform<T>(self, val: T) -> T {
        return val;
    }
}

// Equivalent free function works fine:
fn free_transform<T>(val: T) -> T {
    return val;
}

#[test]
entity TestTb {}

impl TestTb {
    p: process {
        let d: Dummy = Dummy { .dummy = 0 };
        // Free function works:
        let p_ok: Pair = free_transform(Pair { .a = 1, .b = 2 });
        assert!(p_ok.a == 1 and p_ok.b == 2);

        // Method call aborts simulation:
        let p: Pair = d.transform(Pair { .a = 10, .b = 20 });
        assert!(p.a == 10 and p.b == 20);
    }
}

entity TestHw {
    p_out: Pair out,
}

impl TestHw {
    let d: Dummy = Dummy { .dummy = 0 };
    // Fails Stage 6 hardware lowering:
    // error[E-P017]: `d.transform(Pair { .a = 10, .b = 20 })` has no compatible aggregate value form
    p_out = d.transform(Pair { .a = 10, .b = 20 });
}
"#,
            &[],
        ),
        (
            "234_hardware_235",
            r#"module m;
struct D { pub dummy: integer, }
struct Pair { pub a: integer, pub b: integer, }
impl D { pub fn t<T>(self, val: T) -> T { return val; } }
struct Container<T> { pub item: T, }
impl<T> Container<T> { pub fn get(self) -> T { return self.item; } }
entity Hw { p_out: Pair out, q_out: Pair out, }
impl Hw {
    let d: D = D { .dummy = 0 };
    p_out = d.t(Pair { .a = 10, .b = 20 });
    let c: Container<Pair> = Container<Pair> { .item = Pair { .a = 3, .b = 4 } };
    q_out = c.get();
}
#[test] entity T {}
impl T { let p: Pair; let q: Pair; let hw: Hw = { .p_out = p, .q_out = q }; process { await 1ns; print!("{} {} {} {}\n", p.a, p.b, q.a, q.b); } }
"#,
            &[r#"10 20 3 4"#],
        ),
        (
            "235",
            r#"module repro_generic_struct_method;

struct Pair {
    pub a: integer,
    pub b: integer,
}

struct Container<T> {
    pub item: T,
}

impl<T> Container<T> {
    pub fn get(self) -> T {
        return self.item;
    }
}

#[test]
entity TestTb {}

impl TestTb {
    p: process {
        let c: Container<Pair> = Container<Pair> { .item = Pair { .a = 10, .b = 20 } };
        // Testbench aborts simulation at runtime:
        let p: Pair = c.get();
        print!("p.a = {}, p.b = {}\n", p.a, p.b);
        assert!(p.a == 10 and p.b == 20);
    }
}

entity TestHw {
    p_out: Pair out,
}

impl TestHw {
    let c: Container<Pair> = Container<Pair> { .item = Pair { .a = 10, .b = 20 } };
    // Hardware lowering fails Stage 6:
    // error[E-P017]: `c.get()` has no compatible aggregate value form
    p_out = c.get();
}
"#,
            &[r#"p.a = 10, p.b = 20"#],
        ),
    ];
    for (issue, source, printed) in cases {
        let out = run(&format!("run_{issue}"), source);
        assert!(
            out.contains("passed;") && !out.contains("FAILED"),
            "#{issue}: {out}"
        );
        for line in *printed {
            assert!(out.contains(line), "#{issue}: missing `{line}`: {out}");
        }
    }
}

#[test]
fn checks_that_passed_are_diagnosed() {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "231",
            r#"module test_trait_bound_bypass;

trait Doubler {
    fn double(self) -> integer;
}

struct Num { pub v: integer }

impl Doubler for Num {
    fn double(self) -> integer {
        return self.v * 2;
    }
}

fn run_double<T: Doubler>(item: T) -> integer {
    return item.double();
}

#[test]
entity Top {}

impl Top {
    p: process {
        // integer does NOT implement Doubler, but passes typecheck!
        let d: integer = run_double(10);
        print!("d = {}\n", d);
    }
}
"#,
            &[r#"`integer` does not satisfy the bound `T: Doubler`"#],
        ),
        (
            "232",
            r#"module repro_partial_slice;

#[test]
entity TestTb {}

impl TestTb {
    p: process {
        let v: unsigned[8] = 0xAB; // 1010_1011 in binary; low nibble is 0xB (1011)

        let s_explicit: unsigned[4] = v[3..0];
        let s_partial: unsigned[4] = v[..3];

        print!("explicit v[3..0] = {:x}, partial v[..3] = {:x}\n", s_explicit, s_partial);
        // explicit v[3..0] = b, partial v[..3] = d
        assert!(s_partial == s_explicit); // FAILS: 0xd != 0xb

        // Width mismatch bypasses Stage 4 typecheck due to len == 0:
        let s_bad: unsigned[100] = v[..3]; // Compiles cleanly in testbench!
    }
}

entity TestHw {
    v: unsigned[8] in,
    s_bad: unsigned[100] out,
}

impl TestHw {
    // Passes Stage 4 typecheck, fails in Stage 6 hardware lowering:
    // error[E-P003]: width mismatch: `s_bad` is 100 bits but the assigned value is 4 bits
    s_bad = v[..3];
}
"#,
            &[r#"cannot initialize unsigned[100] with unsigned[4]"#],
        ),
        (
            "233",
            r#"module repro_const_literal_bypass;

const OUT_OF_BOUNDS_VAL: integer = 300;
const ZERO: integer = 0;
const OUT_OF_BOUNDS_IDX: integer = 10;

#[test]
entity TestTb {}

impl TestTb {
    p: process {
        // 1. Out-of-bounds constant assignment silently truncates (stores 44 in silence):
        let k: unsigned[8] = OUT_OF_BOUNDS_VAL;
        let c: unsigned[8] = unsigned[8](OUT_OF_BOUNDS_VAL);
        print!("k = {}, c = {}\n", k, c);
        assert!(k == 44 and c == 44);

        // 2. Division by constant zero passes typechecking without error:
        let div_res: integer = 10 / ZERO;
        print!("div_res = {}\n", div_res);
        assert!(div_res == 0);
    }
}

entity TestHw {
    v: unsigned[4] in,
    b: Logic out,
    y_div: integer out,
}

impl TestHw {
    // 3. Lowers division by 0 into hardware IR:
    y_div = 10 / ZERO;

    // 4. Passes Stage 4 typechecking, but fails in Stage 6 with E-P017:
    b = v[OUT_OF_BOUNDS_IDX];
}
"#,
            &[
                r#"`300` does not fit in a 8-bit vector"#,
                r#"division by a constant zero"#,
                r#"bit 10 is outside `0..3`"#,
            ],
        ),
    ];
    for (issue, source, messages) in cases {
        let out = run(&format!("error_{issue}"), source);
        for message in *messages {
            assert!(
                out.contains(message),
                "#{issue}: missing `{message}`: {out}"
            );
        }
    }
}
