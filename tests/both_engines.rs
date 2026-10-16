//! Programs computed both as a DUT output and as a testbench local, which
//! must agree:
//!
//! - `{ v[3..0], v[7..4] }` into a `Bit[8]` was rejected; it fills from
//!   `'left`, as VHDL's `&` does.
//! - `l and '0'` on a `Logic` was rejected: the char did not read through
//!   the other operand's enum, as it does in `l == '0'`.
//! - A `return` inside a `for` in a hardware function left the call
//!   unlowered.
//! - A loop whose iterations read the previous result over `v[k]` made LLVM
//!   lowering exponential (eight iterations took 7 GB).
//! - A ranged-integer DUT port printed directly failed to generate, and a
//!   negative `integer<-8..7>` copied into a local came out positive.
//! - A widening `resize` of a `signed` zero-extended (-3 into 16 bits read
//!   253); it is the family's conversion, as `signed[16](s)` is.
//! - A method on a conversion (`unsigned[8](x).reverse_bits()`) had no shape
//!   for `self`, and failed in both engines.
//! - An element of a computed vector lost its metavalue in hardware
//!   (`id(u)[2]` read `'0'` for an `'X'`), and failed to lower in the
//!   testbench (`(not u)[2]`).
//! - A conversion from `Logic[N]` to another width (`unsigned[8](a)` of a
//!   `Logic[4]`) did not lower in the testbench; it resizes the converted
//!   vector.
//! - A struct's own `Index` contract (`regs[2]`, `regs[1..3]`) did not lower
//!   in the testbench; it now selects the impl by the index's type, as
//!   hardware does. Spelled out (`regs.index(2)`) beside the `Range`
//!   overload, the checker rejected it and the testbench took the wrong one.
//! - `regs[1] = v` through `IndexAssign` did not lower in the testbench.

#[cfg(feature = "llvm")]
fn run(name: &str, source: &str) -> String {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_both_engines_{name}_{}", std::process::id()));
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
    let output = std::process::Command::new(&binary).output().unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[cfg(feature = "llvm")]
#[test]
fn concatenation_loops_and_ranged_ports_agree() {
    let report = run(
        "agree",
        r#"module g;
use std::bits::unsigned;
use std::logic::{Bit, Logic};
fn first_set(v: Bit[8]) -> unsigned[8] { for k in 0..7 { if v[k] == '1' { return k; } } return 255; }
fn chain(v: Bit[8], i: unsigned[3]) -> unsigned[8] {
    let r: unsigned[8] = 255;
    for k in 0..7 { if v[k] == '1' { if r == 255 { r = k; } } if v[i] == '1' { r = r + 1; } }
    return r;
}
entity E {
    v: Bit[8] in, i: unsigned[3] in, l: Logic in, k: integer<0..15> in, n: integer<-8..7> in,
    c: Bit[8] out, d: Bit[4] out, a: Logic out, f: unsigned[8] out, h: unsigned[8] out,
    ko: integer<0..15> out, no: integer<-8..7> out,
}
impl E {
    c = { v[3..0], v[7..4] }; d = { v[0], '1', v[7], '0' }; a = l and '0';
    f = first_set(v); h = chain(v, i); ko = k; no = n;
}
#[test] entity T {}
impl T {
    let v: Bit[8] = "00010100"; let i: unsigned[3] = 3; let l: Logic = 'X';
    let k: integer<0..15> = 13; let n: integer<-8..7> = -5;
    let e: E = { .v = v, .i = i, .l = l, .k = k, .n = n };
    check: process {
        let c: Bit[8] = { v[3..0], v[7..4] }; let d: Bit[4] = { v[0], '1', v[7], '0' };
        let a: Logic = l and '0'; let f: unsigned[8] = first_set(v); let h: unsigned[8] = chain(v, i);
        let ko: integer<0..15> = k; let no: integer<-8..7> = n;
        await 1ns;
        print!("|TB {} {} {} {} {} {} {}\n", c, d, a, f, h, ko, no);
        print!("|HW {} {} {} {} {} {} {}\n", e.c, e.d, e.a, e.f, e.h, e.ko, e.no);
    }
}
"#,
    );
    // v = "00010100" is v[0]..v[7], so v[3] and v[5] are set.
    for engine in ["TB", "HW"] {
        let expected = format!("|{engine} 10000010 0100 '0' 3 7 13 -5\n");
        assert!(report.contains(&expected), "{engine}: {report}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn signed_resize_and_methods_on_conversions() {
    let report = run(
        "resize",
        r#"module g;
use std::bits::{unsigned, signed};
entity E { s: signed[8] in, a: unsigned[8] in, w: signed[16] out, x: signed[70] out, r: unsigned[8] out, c: integer out, p: Bool out }
impl E {
    w = resize(s, 16); x = resize(s, 70); r = unsigned[8](a + 1).reverse_bits();
    c = unsigned[8](64).count_ones(); p = unsigned[8](64).is_power_of_two();
}
#[test] entity T {}
impl T {
    let s: signed[8] = 0 - 3; let a: unsigned[8] = 200;
    let e: E = { .s = s, .a = a };
    check: process {
        let w: signed[16] = resize(s, 16); let x: signed[70] = resize(s, 70);
        let r: unsigned[8] = unsigned[8](a + 1).reverse_bits();
        let c: integer = unsigned[8](64).count_ones(); let p: Bool = unsigned[8](64).is_power_of_two();
        await 1ns;
        print!("|TB {} {} {} {} {}\n", w, x, r, c, p);
        print!("|HW {} {} {} {} {}\n", e.w, e.x, e.r, e.c, e.p);
    }
}
"#,
    );
    for engine in ["TB", "HW"] {
        let expected = format!("|{engine} -3 -3 147 1 true\n");
        assert!(report.contains(&expected), "{engine}: {report}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn elements_of_computed_vectors_keep_metavalues() {
    let report = run(
        "computed",
        r#"module g;
use std::bits::unsigned;
use std::logic::Logic;
fn id(u: unsigned[3]) -> unsigned[3] { return u; }
fn put(l: Logic) -> unsigned[1] { let r: unsigned[1] = 0; r[0] = l; return r; }
entity E { u: unsigned[3] in, l: Logic in, a: Logic out, b: Logic out, c: Logic out, d: Logic out, e: Logic out }
impl E { a = id(u)[2]; b = put(l)[0]; c = (not u)[2]; d = (u and u)[2]; e = id(u)[0]; }
#[test] entity T {}
impl T {
    let u: unsigned[3] = "X11"; let l: Logic = 'X';
    let dut: E = { .u = u, .l = l };
    check: process {
        let a: Logic = id(u)[2]; let b: Logic = put(l)[0]; let c: Logic = (not u)[2];
        let d: Logic = (u and u)[2]; let e: Logic = id(u)[0];
        await 1ns;
        print!("|TB {} {} {} {} {}\n", a, b, c, d, e);
        print!("|HW {} {} {} {} {}\n", dut.a, dut.b, dut.c, dut.d, dut.e);
    }
}
"#,
    );
    for engine in ["TB", "HW"] {
        let expected = format!("|{engine} 'X' 'X' 'X' 'X' '1'\n");
        assert!(report.contains(&expected), "{engine}: {report}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn logic_vector_conversions_change_width() {
    let report = run(
        "widths",
        r#"module g;
use std::bits::unsigned;
use std::logic::Logic;
entity E { a: Logic[4] in, w: unsigned[8] out, n: unsigned[2] out, x: Logic out }
impl E { w = unsigned[8](a); n = unsigned[2](a); x = unsigned[8](a)[2]; }
#[test] entity T {}
impl T {
    let a: Logic[4] = "1X01";
    let e: E = { .a = a };
    check: process {
        let w: unsigned[8] = unsigned[8](a); let n: unsigned[2] = unsigned[2](a);
        let x: Logic = unsigned[8](a)[2];
        await 1ns;
        print!("|TB {} {} {} {}\n", w, n, x, w[7]);
        print!("|HW {} {} {} {}\n", e.w, e.n, e.x, e.w[7]);
    }
}
"#,
    );
    // "1X01": the value plane reads 9, the `'X'` kept as an element.
    for engine in ["TB", "HW"] {
        let expected = format!("|{engine} 9 1 'X' '0'\n");
        assert!(report.contains(&expected), "{engine}: {report}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn custom_index_contracts_agree() {
    let report = run(
        "index",
        r#"module g;
use std::bits::unsigned;
use std::ops::{Index, Range};
struct Regs { pub r: unsigned[8][4] }
impl Index<integer, unsigned[8]> for Regs { fn index(self, i: integer) -> unsigned[8] { return self.r[i]; } }
impl Index<Range, unsigned[8]> for Regs { fn index(self, r: Range) -> unsigned[8] { return self.r[r.left] + self.r[r.right]; } }
entity E { g: Regs in, a: unsigned[8] out, b: unsigned[8] out, c: unsigned[8] out }
impl E { a = g[2]; b = g[1..3]; c = g.index(3); }
#[test] entity T {}
impl T {
    let g: Regs = { .r = [5, 6, 7, 8] };
    let e: E = { .g = g };
    check: process {
        let a: unsigned[8] = g[2]; let b: unsigned[8] = g[1..3]; let c: unsigned[8] = g.index(3);
        await 1ns;
        print!("|TB {} {} {}\n", a, b, c);
        print!("|HW {} {} {}\n", e.a, e.b, e.c);
    }
}
"#,
    );
    for engine in ["TB", "HW"] {
        let expected = format!("|{engine} 7 14 8\n");
        assert!(report.contains(&expected), "{engine}: {report}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn index_assign_contracts_agree() {
    let report = run(
        "index_assign",
        r#"module g;
use std::bits::unsigned;
use std::ops::{Index, IndexAssign};
struct Regs { pub r: unsigned[8][4] }
impl Index<integer, unsigned[8]> for Regs { fn index(self, i: integer) -> unsigned[8] { return self.r[i]; } }
impl IndexAssign<integer, unsigned[8]> for Regs { fn index_assign(self, i: integer, v: unsigned[8]) { self.r[i] = v; } }
entity E { a: unsigned[8] in, o: Regs out }
impl E { p: process { o = { .r = [1, 2, 3, 4] }; o[2] = a; } }
#[test] entity T {}
impl T {
    let a: unsigned[8] = 99; let o: Regs;
    let e: E = { .a = a, .o = o };
    check: process {
        let g: Regs = { .r = [5, 6, 7, 8] };
        g[1] = 50; g[3] = g[1] + 1;
        await 1ns;
        print!("|TB {} {} {}\n", g[0], g[1], g[3]);
        print!("|HW {} {} {}\n", o[1], o[2], o[3]);
    }
}
"#,
    );
    assert!(report.contains("|TB 5 50 51\n"), "{report}");
    assert!(report.contains("|HW 2 99 4\n"), "{report}");
}
