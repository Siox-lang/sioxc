//! Logic and numeric-vector operators with metavalues, checked against
//! answers from `nvc` (`std_logic_1164` / `numeric_std`):
//!
//! - `(a xor b) xor c` on `Logic[N]` was rejected: a lifted custom operator
//!   on arrays typed as an error.
//! - `xor`/`nand`/`nor`/`xnor` on `unsigned`/`signed` were arithmetic in std,
//!   so any metavalue made the testbench result all `X`.
//! - A testbench comparison of numeric vectors read value planes only:
//!   `"01X1" == "0101"` was true. `numeric_std` makes every comparison
//!   against a metavalue false (`/=` true), as hardware already did.
//! - `"H01L"` stored in an `unsigned` read back as `"1010"`.

#[cfg(feature = "llvm")]
fn run(name: &str, source: &str) -> String {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_logic_ops_{name}_{}", std::process::id()));
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
    String::from_utf8_lossy(&output.stdout).replace('\'', "")
}

#[cfg(feature = "llvm")]
#[test]
fn nested_logic_vector_operators() {
    // nvc: "UX01ZWLH-" xnor "1X0Z01LH1", then xor "0000UUUU1".
    let report = run(
        "nested",
        r#"module g;
use std::logic::Logic;
entity E { a: Logic[9] in, b: Logic[9] in, c: Logic[9] in, o: Logic[9] out }
impl E { o = (a xnor b) xor c; }
#[test] entity T {}
impl T {
    let a: Logic[9] = "UX01ZWLH-"; let b: Logic[9] = "1X0Z01LH1"; let c: Logic[9] = "0000UUUU1";
    let e: E = { .a = a, .b = b, .c = c };
    check: process { let t: Logic[9] = (a xnor b) xor c; await 1ns; let h: Logic[9] = e.o; print!("|{} {}", h, t); }
}
"#,
    );
    assert!(report.contains("|UX1XUUUUX UX1XUUUUX"), "{report}");
}

#[cfg(feature = "llvm")]
#[test]
fn numeric_vectors_follow_numeric_std() {
    for family in ["unsigned", "signed"] {
        let report = run(
            family,
            &format!(
                r#"module g;
use std::bits::{family};
use std::logic::Bit;
entity E {{ a: {family}[4] in, b: {family}[4] in, x: {family}[4] out, n: {family}[4] out, q: Bit[4] out }}
impl E {{
    x = a xor b; n = a nand b;
    q[0] = if a == b {{ '1' }} else {{ '0' }}; q[1] = if a != b {{ '1' }} else {{ '0' }};
    q[2] = if a <= b {{ '1' }} else {{ '0' }}; q[3] = if a == a {{ '1' }} else {{ '0' }};
}}
#[test] entity T {{}}
impl T {{
    let a: {family}[4] = "0UX1"; let b: {family}[4] = "0010"; let w: {family}[4] = "H01L";
    let e: E = {{ .a = a, .b = b }};
    check: process {{
        let x: {family}[4] = a xor b; let n: {family}[4] = a nand b;
        await 1ns;
        let hx: {family}[4] = e.x; let hn: {family}[4] = e.n; let hq: Bit[4] = e.q;
        print!("|TB {{}}{{}}{{}}{{}} {{}}{{}}{{}}{{}} {{}} {{}} {{}} {{}}", x[3], x[2], x[1], x[0], n[3], n[2], n[1], n[0], a == b, a != b, a <= b, a == a);
        print!("|HW {{}}{{}}{{}}{{}} {{}}{{}}{{}}{{}} {{}} {{}} {{}} {{}}", hx[3], hx[2], hx[1], hx[0], hn[3], hn[2], hn[1], hn[0], hq[0], hq[1], hq[2], hq[3]);
        print!("|W {{}}{{}}{{}}{{}} {{}}", w[3], w[2], w[1], w[0], w + 1);
    }}
}}
"#
            ),
        );
        // nvc: xor "0UX1"/"0010" = "0UX1"; nand = "11X1"; every comparison false, /= true.
        assert!(
            report.contains("|TB 0UX1 11X1 false true false false"),
            "{family}: {report}"
        );
        assert!(
            report.contains("|HW 0UX1 11X1 0 1 0 0"),
            "{family}: {report}"
        );
        assert!(report.contains("|W H01L "), "{family}: {report}");
    }
}

/// Hardware read a computed comparison operand as known: `(a or b) != a`
/// with `b = "0UU1"` was false (numeric_std: true). And an element of a
/// two-element vector read element 0's discriminant: its one-bit position
/// scaled by 4 wrapped to 0.
#[cfg(feature = "llvm")]
#[test]
fn computed_operands_and_two_element_reads() {
    let report = run(
        "computed",
        r#"module g;
use std::bits::unsigned;
use std::logic::{Logic, Bit};
entity E { a: unsigned[4] in, b: unsigned[4] in, m: unsigned[4] in, x: Logic in, y: Logic in, q: Bit[3] out, o: unsigned[2] out }
impl E {
    q[0] = if (a or b) != a { '1' } else { '0' };
    q[1] = if (a and b) == (a and b) { '1' } else { '0' };
    q[2] = if (a and m) == 0 { '1' } else { '0' };
    o[1] = x; o[0] = y;
}
#[test] entity T {}
impl T {
    let a: unsigned[4] = "0011"; let b: unsigned[4] = "0UU1"; let m: unsigned[4] = "0000";
    let x: Logic = 'X'; let y: Logic = '1';
    let e: E = { .a = a, .b = b, .m = m, .x = x, .y = y };
    check: process {
        await 1ns;
        let q: Bit[3] = e.q;
        print!("|{} {} {} {} {}", q[0], q[1], q[2], e.o[1], e.o[0]);
    }
}
"#,
    );
    // The mask idiom stays known: `'0' and 'U'` is `'0'`.
    assert!(report.contains("|1 0 1 X 1"), "{report}");
}
