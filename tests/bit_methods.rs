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

/// `%` is the remainder with the dividend's sign (Rust's and Verilog's `%`,
/// VHDL's `rem`), the same in hardware and testbenches, on kernel integers,
/// reals, `unsigned` and `signed`; `%=` desugars like `/=`.
#[cfg(feature = "llvm")]
#[test]
fn remainder_takes_the_dividends_sign() {
    let stdout = run(
        "remainder",
        r#"
module remainder;
entity Dut { a: unsigned[8] in, b: unsigned[8] in, s: signed[8] in, t: signed[8] in,
    i: integer in, ur: unsigned[8] out, sr: signed[8] out, ir: integer out }
impl Dut { ur = a % b; sr = s % t; ir = i % 4 + 1; }
#[test] entity T {}
impl T {
    let a: unsigned[8] = 0; let b: unsigned[8] = 1; let s: signed[8] = 0; let t: signed[8] = 1;
    let i: integer = 0;
    let ur: unsigned[8]; let sr: signed[8]; let ir: integer;
    let d: Dut = { .a = a, .b = b, .s = s, .t = t, .i = i, .ur = ur, .sr = sr, .ir = ir };
    check: process {
        a = 200; b = 7; s = 0 - 7; t = 2; i = 0 - 9;
        await 1ns;
        print!("hw {} {} {}", ur, sr, ir);
        s = 7; t = 0 - 2; await 1ns; let p: signed[8] = sr;
        s = 0 - 128; t = 0 - 1; await 1ns; let q: signed[8] = sr;
        print!("hw signs {} {}", p, q);
        let x: unsigned[8] = 200; let u: signed[8] = 0 - 7; let v: signed[8] = 2;
        let n: integer = 0 - 9;
        let w: signed[8] = 7; let m: signed[8] = 0 - 2;
        let k: integer = 17;
        k %= 5;
        print!("tb {} {} {} {} {} {} {}", x % 7, u % v, n % 4 + 1, w % m, 7.5 % 2.0,
            0.0 - 7.5 % 2.0, k);
    }
}
"#,
    );
    for line in ["hw 4 -1 0", "hw signs 1 0", "tb 4 -1 0 1 1.5 -1.5 2"] {
        assert!(
            stdout.lines().any(|printed| printed == line),
            "missing `{line}` in:\n{stdout}"
        );
    }
}

/// A function returning `Logic[]` returns a vector of the size its body sets:
/// a returned sized `let`, or a returned argument. Hardware and testbenches
/// agree, including a slice written into the local.
#[cfg(feature = "llvm")]
#[test]
fn logic_vector_results_take_their_size_from_the_body() {
    let stdout = run(
        "vector_results",
        r#"
module vector_results;
fn same(v: Logic[]) -> Logic[] { return v; }
fn make() -> Logic[] { let r: Logic[3..0] = "10XZ"; return r; }
fn widen(v: Logic[]) -> Logic[] { let r: Logic[5..0] = "000000"; r[3..0] = v; return r; }
entity Dut { a: Logic[3..0] in, x: Logic[3..0] out, y: Logic[3..0] out, z: Logic[5..0] out }
impl Dut { x = same(a); y = make(); z = widen(a); }
#[test] entity T {}
impl T {
    let a: Logic[3..0] = "1010"; let x: Logic[3..0]; let y: Logic[3..0]; let z: Logic[5..0];
    let d: Dut = { .a = a, .x = x, .y = y, .z = z };
    check: process {
        await 1ns;
        let b: Logic[3..0] = "1010";
        print!("hw {} {} {}", x, y, z);
        print!("tb {} {} {}", same(b), make(), widen(b));
    }
}
"#,
    );
    for line in ["hw 1010 10XZ 001010", "tb 1010 10XZ 001010"] {
        assert!(
            stdout.lines().any(|printed| printed == line),
            "missing `{line}` in:\n{stdout}"
        );
    }
}

/// A `Logic` vector compared with a string literal or another vector, in
/// hardware (one signal per element) and testbenches: identity per element,
/// as VHDL's predefined `=` on `std_logic_vector`. Compared as text, the
/// literal was never equal; in hardware the vector had no scalar form.
#[cfg(feature = "llvm")]
#[test]
fn logic_vectors_compare_with_literals_and_vectors() {
    let stdout = run(
        "vector_compare",
        r#"
module vector_compare;
entity Pass { a: Logic[3..0] in, b: Logic[3..0] in, e: Bool out, n: Bool out, s: Bool out }
impl Pass { e = a == "1010"; n = a != b; s = a == b; }
#[test] entity T {}
impl T {
    let a: Logic[3..0] = "1010"; let b: Logic[3..0] = "10X0";
    let e: Bool; let n: Bool; let s: Bool;
    let d: Pass = { .a = a, .b = b, .e = e, .n = n, .s = s };
    check: process {
        await 1ns;
        let q: Logic[3..0] = "10XZ";
        let text: string = "1010";
        print!("hw {} {} {}", e, n, s);
        print!("tb {} {} {} {} {}", q == "10XZ", "10XZ" == q, q != "10XZ", q == "1010", text == "1010");
    }
}
"#,
    );
    for line in ["hw true true false", "tb true true false false true"] {
        assert!(
            stdout.lines().any(|printed| printed == line),
            "missing `{line}` in:\n{stdout}"
        );
    }
}

/// A vector element holds whatever value of its enum is written, metavalues
/// included, as in VHDL: only a resolver gives `'X'` meaning. Packed
/// `unsigned` (value plane plus companion), and array `Logic`, `Bit` and user
/// enum vectors, written in hardware functions and testbenches alike; an
/// element also reads back into a scalar.
#[cfg(feature = "llvm")]
#[test]
fn enum_vector_elements_take_any_value() {
    let stdout = run(
        "element_values",
        r#"
module element_values;
enum State { Idle, Run, Done }
fn marks(v: unsigned) -> unsigned {
    let r: unsigned = v;
    r[v'low] = 'X'; r[v'low + 1] = 'Z'; r[v'low + 2] = 'H'; r[v'low + 3] = 'U';
    return r;
}
fn copies(v: unsigned) -> unsigned {
    let r: unsigned = v;
    r[v'low] = v[v'high]; r[v'low + 1] = v[v'high - 1];
    return r;
}
fn states(v: State[]) -> State[] { let r: State[3..0] = v; r[0] = State::Done; r[3] = State::Run; return r; }
fn logics(v: Logic[]) -> Logic[] { let r: Logic[3..0] = v; r[2] = 'W'; r[0] = '-'; return r; }
entity Dut { a: unsigned[8] in, z: unsigned[8] in, st: State[3..0] in, lg: Logic[3..0] in,
    m: unsigned[8] out, c: unsigned[8] out, so: State[3..0] out, lo: Logic[3..0] out }
impl Dut { m = marks(a); c = copies(z); so = states(st); lo = logics(lg); }
#[test] entity T {}
impl T {
    let a: unsigned[8] = 0; let z: unsigned[8] = "ZH000000";
    let st: State[3..0] = [State::Idle, State::Idle, State::Idle, State::Idle];
    let lg: Logic[3..0] = "1010";
    let m: unsigned[8]; let c: unsigned[8]; let so: State[3..0]; let lo: Logic[3..0];
    let d: Dut = { .a = a, .z = z, .st = st, .lg = lg, .m = m, .c = c, .so = so, .lo = lo };
    check: process {
        await 1ns;
        print!("hw {}{}{}{} {}{} {} {}", m[3], m[2], m[1], m[0], c[1], c[0], so, lo);
        let x: unsigned[8] = 0; let y: unsigned[8] = "ZH000000";
        let s: State[3..0] = [State::Idle, State::Idle, State::Idle, State::Idle];
        let g: Logic[3..0] = "1010";
        let mm: unsigned[8] = marks(x); let cc: unsigned[8] = copies(y);
        print!("tb {}{}{}{} {}{} {} {}", mm[3], mm[2], mm[1], mm[0], cc[1], cc[0], states(s), logics(g));
        let e: Logic = mm[0];
        print!("read {}", e);
    }
}
"#,
    );
    let row = "'U''H''Z''X' 'H''Z' [Run, Idle, Idle, Done] 1W1-";
    for line in [
        format!("hw {row}"),
        format!("tb {row}"),
        "read 'X'".to_string(),
    ] {
        assert!(
            stdout.lines().any(|printed| printed == line),
            "missing `{line}` in:\n{stdout}"
        );
    }
}

/// Bit patterns match through the element type's `Match` (`std_match` for
/// `Logic`: `'H'`/`'L'` read as `'1'`/`'0'`, a metavalue matches only a
/// `-`), and ranges take any constant bounds of the scrutinee's type, open
/// ends and either order, through its `Ord`, in hardware (also inside a
/// function) and testbenches.
#[cfg(feature = "llvm")]
#[test]
fn bit_patterns_match_like_std_match_and_ranges_take_expressions() {
    let stdout = run(
        "match_patterns",
        r#"
module match_patterns;
const DEPTH: integer = 8;
fn decode(op: unsigned[4]) -> integer {
    match op {
        "00--" => { return 1; }
        "01-1" => { return 2; }
        _ => { return 0; }
    }
}
fn bucket(n: integer) -> integer {
    match n {
        ..-1 => { return 0; }
        0..DEPTH - 1 => { return 1; }
        DEPTH.. => { return 2; }
    }
    return 9;
}
entity Dut { op: unsigned[4] in, n: integer in, d: integer out, b: integer out, e: integer out,
    w: integer out }
impl Dut {
    d = decode(op);
    b = bucket(n);
    e = match n { 9..3 => 7, _ => 0 };
    w = match op { "01-1" => 2, _ => 0 };
}
#[test] entity T {}
impl T {
    let op: unsigned[4] = 0; let n: integer = 0;
    let d: integer; let b: integer; let e: integer; let w: integer;
    let dut: Dut = { .op = op, .n = n, .d = d, .b = b, .e = e, .w = w };
    check: process {
        op = "0011"; n = 0 - 5; await 1ns; print!("hw {} {} {} {}", d, b, e, w);
        op = "0H11"; n = 5; await 1ns; print!("hw {} {} {} {}", d, b, e, w);
        op = "0X11"; n = 12; await 1ns; print!("hw {} {} {} {}", d, b, e, w);
        op = "0LH1"; n = 8; await 1ns; print!("hw {} {} {} {}", d, b, e, w);
        let a: unsigned[4] = "0011"; let h: unsigned[4] = "0H11"; let x: unsigned[4] = "0X11";
        print!("tb {} {} {} {} {} {} {}", decode(a), decode(h), decode(x), bucket(0 - 5),
            bucket(5), bucket(12), bucket(8));
        let t: time = 15ns;
        let slot: integer = match t { 0ns..9ns => 1, 10ns..19ns => 2, _ => 3 };
        let r: real = 2.5;
        let k: integer = match r { ..0.0 => 0, 0.0..1.0 => 1, 1.0.. => 2 };
        print!("typed {} {}", slot, k);
    }
}
"#,
    );
    for line in [
        "hw 1 0 0 0",
        "hw 2 1 7 2",
        "hw 0 2 0 0",
        "hw 1 2 7 0",
        "tb 1 2 0 0 1 2 2",
        "typed 2 2",
    ] {
        assert!(
            stdout.lines().any(|printed| printed == line),
            "missing `{line}` in:\n{stdout}"
        );
    }
}

/// `T<args>::f(x)` calls an associated function with `T<args>` as `Self`:
/// fixed-point `resize` with VHDL's overflow and rounding styles, and float
/// `resize`/`From<float>` between formats, in hardware and a testbench.
#[cfg(feature = "llvm")]
#[test]
fn fixed_and_float_resize_take_the_qualifier_as_self() {
    let stdout = run(
        "resize_styles",
        r#"
module resize_styles;
use std::fixed::{ufixed, sfixed};
use std::float::float;
use std::numeric::{Overflow, Rounding};

entity Dut {
    x: ufixed<8, 4> in, y: sfixed<8, 4> in, z: float<32, 23> in,
    a: ufixed<6, 2> out, b: ufixed<6, 2> out, c: ufixed<5, 2> out, d: ufixed<5, 2> out,
    e: sfixed<6, 2> out, f: sfixed<6, 2> out, g: sfixed<5, 2> out, h: sfixed<5, 2> out,
    p: float<16, 10> out, q: float<16, 10> out, r: float<16, 10> out,
}
impl Dut {
    a = ufixed<6, 2>::resize(x, Overflow::Saturate, Rounding::Nearest);
    b = ufixed<6, 2>::resize(x, Overflow::Wrap, Rounding::Truncate);
    c = ufixed<5, 2>::resize(x, Overflow::Saturate, Rounding::Nearest);
    d = ufixed<5, 2>::resize(x, Overflow::Wrap, Rounding::Truncate);
    e = sfixed<6, 2>::resize(y, Overflow::Saturate, Rounding::Nearest);
    f = sfixed<6, 2>::resize(y, Overflow::Saturate, Rounding::Truncate);
    g = sfixed<5, 2>::resize(y, Overflow::Saturate, Rounding::Nearest);
    h = sfixed<5, 2>::resize(y, Overflow::Wrap, Rounding::Truncate);
    p = float<16, 10>::resize(z, Rounding::Nearest);
    q = float<16, 10>::resize(z, Rounding::Truncate);
    r = float<16, 10>(z);
}

#[test] entity T {}
impl T {
    let x: ufixed<8, 4>; let y: sfixed<8, 4>; let z: float<32, 23>;
    let a: ufixed<6, 2>; let b: ufixed<6, 2>; let c: ufixed<5, 2>; let d: ufixed<5, 2>;
    let e: sfixed<6, 2>; let f: sfixed<6, 2>; let g: sfixed<5, 2>; let h: sfixed<5, 2>;
    let p: float<16, 10>; let q: float<16, 10>; let r: float<16, 10>;
    let dut: Dut = { .x = x, .y = y, .z = z, .a = a, .b = b, .c = c, .d = d, .e = e, .f = f,
        .g = g, .h = h, .p = p, .q = q, .r = r };
    check: process {
        x = ufixed<8, 4>(2.6875); y = sfixed<8, 4>(0.0 - 2.5625); z = float<32, 23>(1.000732421875);
        await 1ns;
        print!("hw1 {} {} {} {} | {} {} {} {} | {} {} {}", a, b, c, d, e, f, g, h, p, q, r);
        x = ufixed<8, 4>(13.5); y = sfixed<8, 4>(0.0 - 7.0); z = float<32, 23>(70000.0);
        await 1ns;
        print!("hw2 {} {} {} {} | {} {} {} {} | {} {} {}", a, b, c, d, e, f, g, h, p, q, r);
        let tx: ufixed<8, 4> = ufixed<8, 4>(13.5);
        let ty: sfixed<8, 4> = sfixed<8, 4>(0.0 - 2.5625);
        let tz: float<32, 23> = float<32, 23>(0.0 - 70000.0);
        let t1: ufixed<5, 2> = ufixed<5, 2>::resize(tx, Overflow::Saturate, Rounding::Nearest);
        let t2: ufixed<5, 2> = ufixed<5, 2>::resize(tx, Overflow::Wrap, Rounding::Truncate);
        let t3: sfixed<6, 2> = sfixed<6, 2>::resize(ty, Overflow::Saturate, Rounding::Nearest);
        let t4: sfixed<6, 2> = sfixed<6, 2>::resize(ty, Overflow::Wrap, Rounding::Truncate);
        let t5: float<16, 10> = float<16, 10>::resize(tz, Rounding::Nearest);
        let t6: float<16, 10> = float<16, 10>::resize(tz, Rounding::Truncate);
        print!("tb {} {} {} {} {} {} {}", t1, t2, t3, t4, t5, t6,
            ufixed<6, 2>::resize(tx, Overflow::Wrap, Rounding::Nearest));
    }
}
"#,
    );
    for line in [
        "hw1 2.75 2.5 2.75 2.5 | -2.5 -2.75 -2.5 -2.75 | 1.00098 1 1.00098",
        "hw2 13.5 13.5 7.75 5.5 | -7 -7 -4 1 | inf 65504 inf",
        "tb 7.75 5.5 -2.5 -2.75 -inf -65504 13.5",
    ] {
        assert!(
            stdout.lines().any(|printed| printed == line),
            "missing `{line}` in:\n{stdout}"
        );
    }
}
