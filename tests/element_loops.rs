//! `for x in xs` over an array's or a vector's elements, computed both in
//! hardware and in the testbench:
//!
//! - In a hardware function only the index form (`for k in 0..2 { xs[k] }`)
//!   lowered; the element form left the call unlowered (E-P017), returning
//!   from inside it or not.
//! - The testbench's native loop iterates ranges and arrays, not a packed
//!   vector's elements (`for b in v`, `v: unsigned[8]`).
//! - A `Goto` into a loop with no native state (such a vector loop) made
//!   the loop-entry reset panic instead of reporting the process
//!   unsupported.

#[cfg(feature = "llvm")]
#[test]
fn element_loops_agree() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let source = r#"module g;
use std::bits::unsigned;
fn sum(xs: unsigned[4][3]) -> unsigned[8] { let acc: unsigned[8] = 0; for x in xs { acc = acc + resize(x, 8); } return acc; }
fn first_big(xs: unsigned[4][3]) -> unsigned[4] { for x in xs { if x > 9 { return x; } } return 0; }
fn unknowns(v: unsigned[6]) -> unsigned[4] { let n: unsigned[4] = 0; for b in v { if b == 'X' or b == 'Z' { n = n + 1; } } return n; }
entity E { xs: unsigned[4][3] in, v: unsigned[6] in, s: unsigned[8] out, f: unsigned[4] out, u: unsigned[4] out }
impl E { s = sum(xs); f = first_big(xs); u = unknowns(v); }
#[test] entity T {}
impl T {
    let xs: unsigned[4][3] = [5, 12, 15]; let v: unsigned[6] = "1XZ0X1";
    let dut: E = { .xs = xs, .v = v };
    check: process {
        await 1ns;
        let ones: integer = 0;
        for b in v { if b == '1' { ones = ones + 1; } }
        print!("|TB {} {} {} {}", sum(xs), first_big(xs), unknowns(v), ones);
        print!("|HW {} {} {}", dut.s, dut.f, dut.u);
    }
}
"#;
    let dir = std::env::temp_dir().join(format!("siox_element_loops_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("loops");
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("loops.siox"), source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let output = std::process::Command::new(&binary).output().unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("|TB 32 12 3 2\n"), "{report}");
    assert!(report.contains("|HW 32 12 3\n"), "{report}");
}
