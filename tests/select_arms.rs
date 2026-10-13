//! A chain of selections over the previous link, fed by a call that only
//! expands in the CFG, expands each link once. Both arms of
//! `p1 = if c { p0 * 3 } else { p0 }` start with `p0`; expanded per arm the
//! chain doubled at every link. std's `pow2` under `real(sqrt(x))` took ten
//! seconds and 700 KB of code that way.

#[cfg(feature = "llvm")]
#[test]
fn shared_select_arms_expand_a_deferred_call_once() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_select_arms_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("arms");
    let source = r#"
module arms;
// The loop keeps `grow` out of value inlining: it expands in the CFG.
fn grow(x: integer) -> integer {
    let y: integer = x;
    for i in 0..4 { y = y + i; }
    return y;
}
fn chain(v: integer) -> integer {
    let p0: integer = if v > 0 { v * 3 } else { v };
    let p1: integer = if v > 1 { p0 * 3 } else { p0 };
    let p2: integer = if v > 2 { p1 * 3 } else { p1 };
    let p3: integer = if v > 3 { p2 * 3 } else { p2 };
    let p4: integer = if v > 4 { p3 * 3 } else { p3 };
    let p5: integer = if v > 5 { p4 * 3 } else { p4 };
    let p6: integer = if v > 6 { p5 * 3 } else { p5 };
    let p7: integer = if v > 7 { p6 * 3 } else { p6 };
    let p8: integer = if v > 8 { p7 * 3 } else { p7 };
    let p9: integer = if v > 9 { p8 * 3 } else { p8 };
    let p10: integer = if v > 10 { p9 * 3 } else { p9 };
    let p11: integer = if v > 11 { p10 * 3 } else { p10 };
    let p12: integer = if v > 12 { p11 * 3 } else { p11 };
    let p13: integer = if v > 13 { p12 * 3 } else { p12 };
    let p14: integer = if v > 14 { p13 * 3 } else { p13 };
    let p15: integer = if v > 15 { p14 * 3 } else { p14 };
    return p15;
}
#[test] entity T {}
impl T {
    check: process {
        let a: integer = 3;
        await 1ns;
        print!("chain={}", chain(grow(a)));
    }
}
"#;
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("arms.siox"), source),
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
    let values = compilation.design.as_ref().unwrap().process_ir.values.len();
    assert!(
        values < 3_000,
        "the chain expanded per arm: {values} values"
    );
    let output = std::process::Command::new(&binary)
        .output()
        .expect("test executable runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // grow(3) = 3 + 0 + 1 + 2 + 3 + 4 = 13, so links 0..=12 multiply: 13 * 3^13.
    assert!(
        stdout.lines().any(|line| line == "chain=20726199"),
        "{stdout}"
    );
}
