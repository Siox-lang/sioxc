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
