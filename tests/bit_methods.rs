//! Rust's integer methods on `unsigned`/`signed` (std::bits) and
//! `std::math::{cmp, clamp}`, run in hardware and in a testbench, plus the
//! function-body forms they rely on: loops, reassignment and width-generic
//! locals.

use std::process::Command;

#[cfg(feature = "llvm")]
fn run(name: &str, source: &str) -> String {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join(name);
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join(format!("{name}.siox")), source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compilation.succeeded(),
        "{} {:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    let output = Command::new(&binary)
        .output()
        .expect("test executable runs");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(output.status.success(), "{stdout}");
    stdout
}

#[cfg(feature = "llvm")]
#[test]
fn bit_methods_agree_in_hardware_and_testbench() {
    let stdout = run(
        "bit_methods",
        r#"
module bit_methods;
use std::math::{clamp, cmp};
entity Dut { a: unsigned[8] in, b: unsigned[8] in, s: signed[8] in, t: signed[8] in,
    co: integer out, lz: integer out, tz: integer out, lo: integer out, to: integer out,
    rev: unsigned[8] out, rl: unsigned[8] out, rr: unsigned[8] out, p2: Bool out,
    np: unsigned[8] out, pw: unsigned[8] out, ad: unsigned[8] out, sa: unsigned[8] out,
    ss: unsigned[8] out, sco: integer out, slz: integer out, ssa: signed[8] out,
    sss: signed[8] out, spw: signed[8] out, cl: integer out }
impl Dut {
    co = a.count_ones(); lz = a.leading_zeros(); tz = a.trailing_zeros();
    lo = a.leading_ones(); to = a.trailing_ones();
    rev = a.reverse_bits(); rl = a.rotate_left(3); rr = a.rotate_right(3);
    p2 = b.is_power_of_two(); np = b.next_power_of_two(); pw = b.pow(3);
    ad = a.abs_diff(b); sa = a.saturating_add(b); ss = b.saturating_sub(a);
    sco = s.count_ones(); slz = s.leading_zeros();
    ssa = s.saturating_add(t); sss = s.saturating_sub(t); spw = t.pow(3);
    cl = clamp(integer(b), 10, 20);
}
#[test] entity T {}
impl T {
    let a: unsigned[8] = 0; let b: unsigned[8] = 0; let s: signed[8] = 0; let t: signed[8] = 0;
    let co: integer; let lz: integer; let tz: integer; let lo: integer; let to: integer;
    let rev: unsigned[8]; let rl: unsigned[8]; let rr: unsigned[8]; let p2: Bool;
    let np: unsigned[8]; let pw: unsigned[8]; let ad: unsigned[8]; let sa: unsigned[8];
    let ss: unsigned[8]; let sco: integer; let slz: integer; let ssa: signed[8];
    let sss: signed[8]; let spw: signed[8]; let cl: integer;
    let d: Dut = { .a = a, .b = b, .s = s, .t = t, .co = co, .lz = lz, .tz = tz, .lo = lo,
        .to = to, .rev = rev, .rl = rl, .rr = rr, .p2 = p2, .np = np, .pw = pw, .ad = ad,
        .sa = sa, .ss = ss, .sco = sco, .slz = slz, .ssa = ssa, .sss = sss, .spw = spw,
        .cl = cl };
    check: process {
        a = 200; b = 5; s = 0 - 100; t = 0 - 50;
        await 1ns;
        print!("hw {} {} {} {} {} {:08b} {:08b} {:08b} {} {} {} {} {} {} {} {} {} {} {} {}",
            co, lz, tz, lo, to, rev, rl, rr, p2, np, pw, ad, sa, ss, sco, slz, ssa, sss, spw, cl);
        let x: unsigned[8] = 200; let y: unsigned[8] = 5;
        let u: signed[8] = 0 - 100; let v: signed[8] = 0 - 50;
        print!("tb {} {} {} {} {} {:08b} {:08b} {:08b} {} {} {} {} {} {} {} {} {} {} {} {}",
            x.count_ones(), x.leading_zeros(), x.trailing_zeros(), x.leading_ones(),
            x.trailing_ones(), x.reverse_bits(), x.rotate_left(3), x.rotate_right(3),
            y.is_power_of_two(), y.next_power_of_two(), y.pow(3), x.abs_diff(y),
            x.saturating_add(y), y.saturating_sub(x), u.count_ones(), u.leading_zeros(),
            u.saturating_add(v), u.saturating_sub(v), v.pow(3), clamp(integer(y), 10, 20));
        let big: unsigned[8] = 129; let one: unsigned[8] = 64; let z: unsigned[8] = 0;
        print!("edge {} {} {} {} {} {} {}", big.next_power_of_two(), one.next_power_of_two(),
            z.next_power_of_two(), z.leading_zeros(), one.is_power_of_two(),
            x.saturating_add(x), u.saturating_sub(0 - v - v));
        let w: unsigned[80] = 1;
        print!("wide {} {} {}", w.leading_zeros(), w.rotate_right(1), w.rotate_left(79));
        print!("cmp {} {} {} {}", cmp(1, 2), cmp(2.5, 2.5), cmp(u, v), clamp(0 - 5, 0, 9));
    }
}
"#,
    );
    let row = "3 0 3 2 0 00010011 01000110 00011001 false 8 125 195 205 0 4 0 -128 -50 -72 10";
    for line in [
        format!("hw {row}"),
        format!("tb {row}"),
        "edge 0 64 1 8 true 255 -128".to_string(),
        "wide 79 604462909807314587353088 604462909807314587353088".to_string(),
        "cmp Less Equal Less 0".to_string(),
    ] {
        assert!(
            stdout.lines().any(|printed| printed == line),
            "missing `{line}` in:\n{stdout}"
        );
    }
}

/// The function-body forms behind those methods, each of which was wrong or
/// unlowered before: a width-generic local written by element, a literal
/// operand of a `signed` parameter, a sized local with an `integer`
/// initializer, and a loop with reassignment and a return-free `if` in
/// hardware.
#[cfg(feature = "llvm")]
#[test]
fn function_bodies_keep_widths_and_loop_in_hardware() {
    let stdout = run(
        "function_bodies",
        r#"
module function_bodies;
fn set_low(v: unsigned) -> unsigned { let r: unsigned = v; r[v'low] = '1'; return r; }
fn dec(x: signed) -> signed { return x - 1; }
fn ones(v: unsigned) -> integer {
    let n: integer = 0;
    for k in 0..v'length - 1 { if v[v'low + k] == '1' { n = n + 1; } }
    return n;
}
entity Dut { a: unsigned[8] in, r: integer out }
impl Dut { r = ones(a); }
#[test] entity T {}
impl T {
    let a: unsigned[8] = 0; let r: integer;
    let d: Dut = { .a = a, .r = r };
    check: process {
        a = 200;
        await 1ns;
        let x: unsigned[8] = 200;
        let five: signed[8] = 5;
        let u: signed[8] = 0 - 100;
        print!("bodies {} {} {} {} {}", set_low(x), dec(five), u.count_ones(), r, ones(x));
    }
}
"#,
    );
    assert!(
        stdout.lines().any(|line| line == "bodies 201 4 4 3 3"),
        "{stdout}"
    );
}

/// Hardware function bodies agree with the testbench on the shapes that used
/// to differ or fail: a comparison on a reassigned `let` (it compared
/// unsigned), element writes to a local, and width-generic conversions,
/// including a method on one.
#[cfg(feature = "llvm")]
#[test]
fn hardware_function_locals_match_the_testbench() {
    let stdout = run(
        "function_locals",
        r#"
module function_locals;
fn below(x: signed) -> integer { let m: signed = x; m = x - 1; if m < 0 { return 1; } return 0; }
fn wrap(x: integer) -> integer { let m: integer = x; m = m - 300; if m < 0 { return 1; } return 0; }
fn set_low(v: unsigned) -> unsigned { let r: unsigned = v; r[v'low] = '1'; return r; }
fn reversed(v: unsigned) -> unsigned {
    let r: unsigned = v;
    for k in 0..v'length - 1 { r[v'low + k] = v[v'high - k]; }
    return r;
}
fn as_unsigned(v: signed) -> unsigned { return unsigned(v); }
fn ones(v: signed) -> integer { return unsigned(v).count_ones(); }
entity Dut { s: signed[8] in, a: unsigned[8] in,
    b: integer out, w: integer out, l: unsigned[8] out, r: unsigned[8] out,
    c: unsigned[8] out, o: integer out }
impl Dut {
    b = below(s); w = wrap(integer(a)); l = set_low(a); r = reversed(a);
    c = as_unsigned(s); o = ones(s);
}
#[test] entity T {}
impl T {
    let s: signed[8] = 0; let a: unsigned[8] = 0;
    let b: integer; let w: integer; let l: unsigned[8]; let r: unsigned[8];
    let c: unsigned[8]; let o: integer;
    let d: Dut = { .s = s, .a = a, .b = b, .w = w, .l = l, .r = r, .c = c, .o = o };
    check: process {
        s = 0 - 100; a = 200;
        await 1ns;
        let u: signed[8] = 0 - 100; let x: unsigned[8] = 200;
        print!("hw {} {} {} {} {} {}", b, w, l, r, c, o);
        print!("tb {} {} {} {} {} {}", below(u), wrap(integer(x)), set_low(x), reversed(x),
            as_unsigned(u), ones(u));
    }
}
"#,
    );
    for line in ["hw 1 1 201 19 156 4", "tb 1 1 201 19 156 4"] {
        assert!(
            stdout.lines().any(|printed| printed == line),
            "missing `{line}` in:\n{stdout}"
        );
    }
}
