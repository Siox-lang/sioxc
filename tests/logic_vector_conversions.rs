//! `unsigned(v)` / `signed(v)` for a `Logic[N]` (VHDL's `unsigned(slv)`),
//! and the hardware gaps it ran into:
//!
//! - The conversion gave 0 in the testbench and failed to lower in hardware.
//! - In hardware, a kernel-integer constant index (`v[0 + 1]`, `v[v'low + 1]`)
//!   was a one-bit signed sum, so `1` sign-extended to 3.
//! - An inlined body's array argument bound `v'length` to one element's width,
//!   and a local sized by it (`unsigned[v'length]`) had no width.
//! - An element written from a `Logic` value lost its metavalue.
//! - A loop of guarded element writes made LLVM lowering exponential.

#[cfg(feature = "llvm")]
fn run(name: &str, source: &str) -> String {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_logic_vector_{name}_{}", std::process::id()));
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
fn logic_vectors_convert_to_numeric_families() {
    let report = run(
        "convert",
        r#"module g;
use std::logic::Logic;
use std::bits::{unsigned, signed};
entity E {
    a: Logic[4] in, d: Logic[3 .. 0] in, n: Logic[4] in, w: Logic[70] in, m: Logic[4] in,
    oa: unsigned[4] out, od: unsigned[4] out, on: signed[4] out, ou: unsigned[4] out,
    ow: unsigned[70] out, om: unsigned[4] out,
}
impl E {
    oa = unsigned[4](a); od = unsigned[4](d); on = signed[4](n); ou = unsigned(a);
    ow = unsigned[70](w); om = unsigned[4](m);
}
#[test] entity T {}
impl T {
    let a: Logic[4] = "0110"; let d: Logic[3 .. 0] = "0110"; let n: Logic[4] = "1110";
    let m: Logic[4] = "1X0Z";
    let w: Logic[70] = "1000000000000000000000000000000000000000000000000000000000000000000011";
    let e: E = { .a = a, .d = d, .n = n, .w = w, .m = m };
    check: process {
        let ta: unsigned[4] = unsigned[4](a); let td: unsigned[4] = unsigned[4](d);
        let tn: signed[4] = signed[4](n); let tu: unsigned[4] = unsigned(a);
        let tw: unsigned[70] = unsigned[70](w); let tm: unsigned[4] = unsigned[4](m);
        let lm: unsigned[4] = "1X0Z";
        await 1ns;
        print!("|TB {} {} {} {} {} {} {}\n", ta, td, tn, tu, tw, tm == lm, tm[2]);
        let hm: unsigned[4] = e.om;
        print!("|HW {} {} {} {} {} {} {}\n", e.oa, e.od, e.on, e.ou, e.ow, hm == lm, hm[2]);
    }
}
"#,
    );
    for engine in ["TB", "HW"] {
        let expected = format!("|{engine} 6 6 -2 6 590295810358705651715 true 'X'\n");
        assert!(report.contains(&expected), "{engine}: {report}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn array_arguments_in_hardware_bodies() {
    let report = run(
        "arguments",
        r#"module g;
use std::logic::Logic;
use std::bits::unsigned;
fn at(v: Logic[]) -> Logic { return v[0 + 1]; }
fn low(v: Logic[]) -> Logic { return v[v'low + 1]; }
fn high(v: Logic[]) -> Logic { return v[v'high - 1]; }
fn reverse(x: Logic[], up: Bool) -> unsigned[24] {
    let r: unsigned[x'length] = 0;
    for k in 0..23 {
        if up { r[k] = x[k]; } else { r[k] = x[23 - k]; }
    }
    return r;
}
entity E { v: Logic[4] in, x: Logic[24] in, a: Logic out, b: Logic out, c: Logic out, r: unsigned[24] out }
impl E { a = at(v); b = low(v); c = high(v); r = reverse(x, false); }
#[test] entity T {}
impl T {
    let v: Logic[4] = "0110";
    let x: Logic[24] = "000000000000000000000001";
    let e: E = { .v = v, .x = x };
    check: process { await 1ns; print!("|{} {} {} {}\n", e.a, e.b, e.c, e.r); }
}
"#,
    );
    assert!(report.contains("|'1' '1' '1' 1\n"), "{report}");
}
