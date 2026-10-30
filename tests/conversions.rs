//! Conversions (docs/proposals/conversions.md): `Default`, `From` and `Into`.

use std::process::Command;

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("siox_conversions_{}", std::process::id()));
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

/// `T::default()` and `T()` build the default, `new` is a type's own
/// constructor, and an undeclared `T::new()` still builds the default with
/// the `renamed_default` warning.
#[test]
fn default_replaces_new() {
    let out = run(
        "default",
        r#"module d;
enum Phase { Idle, Run, }
struct Pair { pub a: integer, pub b: integer, }
struct S { pub v: integer, }
impl S { pub fn new(x: integer) -> S { return S { .v = x }; } }
struct K { pub k: integer, }
impl Default for K { fn default() -> K { return K { .k = 7 }; } }
#[test] entity T {}
impl T {
    let l: Logic;
    process {
        let p: Phase = Phase::default();
        let q: Pair = Pair::default();
        let r: Pair = Pair();
        let s: S = S::new(42);
        let old: Pair = Pair::new();
        let k: K = K::default();
        print!("{:?} {} {} {} {} {}\n", p, q.a, r.b, s.v, old.a, l);
        assert!(p == Phase::Idle and q.a == 0 and s.v == 42 and l == 'U');
    }
}
"#,
    );
    assert!(out.contains("passed;") && !out.contains("FAILED"), "{out}");
    assert!(out.contains("Idle 0 0 42 0 'U'"), "{out}");
    assert!(
        out.contains("W-P020") && out.contains("`Pair::new()` is now `Pair::default()`"),
        "{out}"
    );
}

/// `Logic[]` and the vector families over it convert implicitly where a
/// value is stored, both ways, through std's `impl Into` (#298, #301).
#[test]
fn into_converts_between_logic_arrays_and_vector_families() {
    let out = run(
        "into",
        r#"module into;
use std::fixed::{ufixed, sfixed};
use std::float::float;
#[test] entity T {}
impl T { process {
    let arr: Logic[8] = "00011000";
    let u: unsigned[8] = arr;
    let s: signed[8] = arr;
    let f: ufixed<8, 4> = arr;
    let g: sfixed<8, 4> = arr;
    let b1: Logic[8] = u;
    let b2: Logic[8] = f;
    let w: Logic[32] = "00111111110000000000000000000000";
    let x: float<32, 23> = w;
    let b3: Logic[32] = x;
    let n: Logic[8] = "11101000";
    let ns: sfixed<8, 4> = n;
    let bs: Logic[8] = x"80";
    print!("{} {} {} {} {} {} {} {} {} {}\n", u, s, real(f), real(g), b1, b2, real(x), b3, real(ns), bs);
    assert!(u == 24 and real(f) == 1.5 and real(g) == 1.5 and real(x) == 1.5 and real(ns) == -1.5);
    assert!(b1 == arr and b2 == arr and b3 == w and bs == "10000000");
} }
"#,
    );
    assert!(out.contains("passed;") && !out.contains("FAILED"), "{out}");
    assert!(
        out.contains(
            "24 24 1.5 1.5 00011000 00011000 1.5 00111111110000000000000000000000 -1.5 10000000"
        ),
        "{out}"
    );
}

/// The same conversions in a design: run under a test, and built as an
/// object (#298 did neither).
#[test]
fn into_converts_in_hardware() {
    let out = run(
        "hardware",
        r#"module hw;
use std::fixed::ufixed;
entity Top {
    u: unsigned[8] in,
    arr_in: Logic[8] in,
    f: ufixed<8, 4> in,
    y1: Logic out,
    y2: unsigned[8] out,
    y3: Logic[4] out,
    y4: Logic[8] out,
    y5: ufixed<8, 4> out,
}
impl Top {
    let arr: Logic[8];
    arr = u;
    y1 = arr[0];
    let u_out: unsigned[8];
    u_out = arr_in;
    y2 = u_out;
    y3 = u[7..4];
    y4 = f;
    y5 = arr_in;
}
#[test] entity T {}
impl T {
    let u: unsigned[8] = 0xA5; let arr_in: Logic[8] = "00011000"; let f: ufixed<8, 4> = ufixed<8, 4>(2.75);
    let y1: Logic; let y2: unsigned[8]; let y3: Logic[4]; let y4: Logic[8]; let y5: ufixed<8, 4>;
    let t: Top = { .u = u, .arr_in = arr_in, .f = f, .y1 = y1, .y2 = y2, .y3 = y3, .y4 = y4, .y5 = y5 };
    process {
        await 1ns;
        print!("{} {} {} {} {}\n", y1, y2, y3, y4, real(y5));
        assert!(y1 == '1' and y2 == 24 and y3 == "1010" and y4 == "00101100" and real(y5) == 1.5);
    }
}
"#,
    );
    assert!(out.contains("passed;") && !out.contains("FAILED"), "{out}");
    assert!(out.contains("'1' 24 1010 00101100 1.5"), "{out}");
    let dir = scratch();
    let file = dir.join("design.siox");
    std::fs::write(&file, r#"module hw;
use std::fixed::ufixed;
entity Top {
    u: unsigned[8] in,
    arr_in: Logic[8] in,
    f: ufixed<8, 4> in,
    y1: Logic out,
    y2: unsigned[8] out,
    y3: Logic[4] out,
    y4: Logic[8] out,
    y5: ufixed<8, 4> out,
}
impl Top {
    let arr: Logic[8];
    arr = u;
    y1 = arr[0];
    let u_out: unsigned[8];
    u_out = arr_in;
    y2 = u_out;
    y3 = u[7..4];
    y4 = f;
    y5 = arr_in;
}
#[test] entity T {}
impl T {
    let u: unsigned[8] = 0xA5; let arr_in: Logic[8] = "00011000"; let f: ufixed<8, 4> = ufixed<8, 4>(2.75);
    let y1: Logic; let y2: unsigned[8]; let y3: Logic[4]; let y4: Logic[8]; let y5: ufixed<8, 4>;
    let t: Top = { .u = u, .arr_in = arr_in, .f = f, .y1 = y1, .y2 = y2, .y3 = y3, .y4 = y4, .y5 = y5 };
    process {
        await 1ns;
        print!("{} {} {} {} {}\n", y1, y2, y3, y4, real(y5));
        assert!(y1 == '1' and y2 == 24 and y3 == "1010" and y4 == "00101100" and real(y5) == 1.5);
    }
}
"#).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--top",
            "Top",
            "-o",
        ])
        .arg(dir.join("design.o"))
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// An operator's operand is never converted: `u + arr` read `arr`'s raw
/// storage and gave 1 for 1 + 2.
#[test]
fn an_operand_is_not_converted_implicitly() {
    let out = run(
        "operand",
        r#"module ops;
#[test] entity T {}
impl T { process {
    let u: unsigned[8] = 1; let arr: Logic[8] = "00000010";
    let s: unsigned[8] = u + arr;
    print!("{}\n", s);
} }
"#,
    );
    assert!(out.contains("`+` mixes unsigned[8] and Logic[8]"), "{out}");
    assert!(out.contains("write `unsigned[8](arr)`"), "{out}");
}

/// A struct-literal field is a store site: `Pkt { .data = u }` with
/// `data: Logic[8]` and `u: unsigned[8]` converts through std's `Into`, in a
/// combinational assignment, a process local and a testbench.
#[test]
fn into_converts_into_struct_fields() {
    let out = run(
        "struct_fields",
        r#"module p;
use std::bits::unsigned;
struct Pkt { pub data: Logic[8], pub id: unsigned[4] }
entity Top { u: unsigned[8] in, pd: Logic[8] out, pr: Logic[8] out }
impl Top {
    let p: Pkt;
    p = Pkt { .data = u, .id = 3 };
    q: process { let r: Pkt = Pkt { .data = u, .id = 2 }; pr = r.data; }
    pd = p.data;
}
#[test] entity T {}
impl T {
    let u: unsigned[8] = 0xA5; let pd: Logic[8]; let pr: Logic[8];
    let t: Top = { .u = u, .pd = pd, .pr = pr };
    process {
        await 1ns;
        let q: Pkt = Pkt { .data = u, .id = 1 };
        print!("{} {} {}\n", pd, pr, q.data);
        assert!(pd == "10100101" and pr == "10100101" and q.data == "10100101");
    }
}
"#,
    );
    assert!(
        out.contains("10100101 10100101 10100101") && out.contains("1 passed"),
        "{out}"
    );
}
