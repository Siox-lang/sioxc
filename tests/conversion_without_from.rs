//! `T(x)` with no route from `x`'s type is an error, not an `Unknown`.
//!
//! A conversion on a named type dispatches to `impl From<S> for T` or to a
//! total derivation (spec 3.17/3.28). With neither, the type checker accepted
//! it and lowering quietly left an `Unknown`, so the failure arrived after
//! parse, resolve and typecheck had all reported success:
//!
//! ```text
//! sioxc --test: the driver for `T.e.y`: contains an Unknown (unlowered) expression
//! ```
//!
//! — naming a signal rather than the expression, with no code and no span.
//!
//! `Bit(sh[0])` is how this is met in practice: shifting a bit out of a vector
//! yields `Logic`, and narrowing that to `Bit` is exactly the conversion std
//! declines to provide, because `Bit` has nowhere to put `'X'` or `'Z'`.
//!
//! The negative controls are the point of the fix — the three routes that do
//! exist have to keep working, or the check has simply banned conversions.

use std::process::Command;

/// Compile a testbench-only source and return the diagnostics. Testbench code
/// is emitted as C by a separate backend, so a rule enforced only in hardware
/// lowering does not reach it.
fn testbench_diagnostics(name: &str, body: &str) -> String {
    let src = format!(
        "module m;\n\
         using std::logic::{{Bit, Logic, ULogic}};\n\
         using std::bits::{{unsigned}};\n\
         #[test] entity T {{}}\n\
         impl T {{ {body} await 1ns; print!(\"r={{}}\", o); }}\n"
    );
    let dir = std::env::temp_dir().join(format!("siox_convtb_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, src).unwrap();
    let bin = dir.join(format!("{name}.bin"));
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .arg("--test")
        .arg(&file)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap();
    let mut text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    // `--test` builds the binary; running it is what reports the assertions.
    if bin.exists() {
        let run = Command::new(&bin).output().unwrap();
        text.push_str(&String::from_utf8_lossy(&run.stdout));
        text.push_str(&String::from_utf8_lossy(&run.stderr));
    }
    text
}

fn diagnostics(name: &str, body: &str, out_ty: &str) -> String {
    let src = format!(
        "module m;\n\
         using std::logic::{{Bit, Logic, ULogic}};\n\
         using std::bits::{{unsigned}};\n\
         entity E {{ y: {out_ty} out }}\n\
         impl E {{ {body} }}\n"
    );
    let dir = std::env::temp_dir().join(format!("siox_conv_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(["--emit", "metadata"])
        .arg(&file)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn narrowing_logic_to_bit_is_reported() {
    let out = diagnostics("bitlogic", "let l: Logic = '1'; y = Bit(l);", "Bit");
    assert!(
        out.contains("no conversion from `Logic` to `Bit`"),
        "expected a conversion error, got:\n{out}"
    );
    assert!(out.contains("E-P003"), "and a stable code, got:\n{out}");
    assert!(
        !out.contains("Unknown (unlowered)"),
        "and not the late internal message, got:\n{out}"
    );
}

#[test]
fn a_bit_taken_out_of_a_vector_is_reported_the_same_way() {
    // The shape that actually turns up: `sh[0]` on an `unsigned[8]` is a
    // `Logic`, and this is the line anyone writes in a shift register.
    let out = diagnostics(
        "vecbit",
        "let sh: unsigned[8] = 165; y = Bit(sh[0]);",
        "Bit",
    );
    assert!(
        out.contains("no conversion from `Logic` to `Bit`"),
        "expected a conversion error, got:\n{out}"
    );
}

#[test]
fn widening_without_a_from_impl_is_reported_too() {
    // `Logic` derives from `ULogic`, and `Bit` is unrelated to both, so there
    // is no route in this direction either even though every `Bit` value is a
    // legal `Logic`. std provides `From<Bit> for ULogic` and not this one.
    let out = diagnostics("logicbit", "let b: Bit = '1'; y = Logic(b);", "Logic");
    assert!(
        out.contains("no conversion from `Bit` to `Logic`"),
        "expected a conversion error, got:\n{out}"
    );
}

#[test]
fn an_explicit_from_impl_still_converts() {
    // std has `impl From<Bit> for ULogic`. If this breaks, the check has
    // banned conversions rather than fixed their diagnostics.
    let out = diagnostics("fromimpl", "let b: Bit = '1'; y = ULogic(b);", "ULogic");
    assert!(
        !out.contains("E-P003"),
        "an explicit From impl must still convert, got:\n{out}"
    );
}

#[test]
fn a_derivation_chain_still_converts_both_ways() {
    // `enum Logic(ULogic)` is a newtype, so both directions are total and
    // synthesized without any `From` impl.
    let down = diagnostics("derivdown", "let u: ULogic = '1'; y = Logic(u);", "Logic");
    assert!(
        !down.contains("E-P003"),
        "base -> derived must still convert, got:\n{down}"
    );
    let up = diagnostics("derivup", "let l: Logic = '1'; y = ULogic(l);", "ULogic");
    assert!(
        !up.contains("E-P003"),
        "derived -> base must still convert, got:\n{up}"
    );
}

#[test]
fn a_vector_conversion_is_untouched() {
    // Kernel width conversions are builtins and never went through `From`;
    // the check must not reach them.
    let out = diagnostics(
        "vec",
        "let s: unsigned[8] = 200; y = unsigned[16](s);",
        "unsigned[16]",
    );
    assert!(
        !out.contains("E-P003"),
        "a width conversion must be untouched, got:\n{out}"
    );
}

#[test]
fn the_testbench_engine_refuses_the_same_conversions() {
    // The two engines disagreed on the same line. Hardware lowering rejects
    // `Bit(l)`; the testbench emitter passed *any* `EnumName(x)` straight
    // through, because its guard checked only that the target was an enum
    // while its comment claimed a derivation chain. `'X'` therefore arrived
    // in a `Bit`, which has nowhere to put it, and printed as `?`.
    for body in [
        "let l: Logic = 'X'; let o: Bit; o = Bit(l);",
        "let l: Logic = '1'; let o: Bit; o = Bit(l);",
        "let b: Bit = '1'; let o: Logic; o = Logic(b);",
    ] {
        let out = testbench_diagnostics("tbbad", body);
        assert!(
            out.contains("no conversion from"),
            "the testbench must refuse `{body}`, got:\n{out}"
        );
    }
}

#[test]
fn the_testbench_engine_still_makes_the_legal_ones() {
    // The three routes that exist, through the emitter this time: an explicit
    // `From` impl, and a derivation chain in both directions. Rejecting these
    // would trade a wrong answer for a broken language.
    for body in [
        "let b: Bit = '1'; let o: ULogic; o = ULogic(b);",
        "let u: ULogic = '1'; let o: Logic; o = Logic(u);",
        "let l: Logic = '1'; let o: ULogic; o = ULogic(l);",
    ] {
        let out = testbench_diagnostics("tbok", body);
        assert!(
            !out.contains("no conversion from"),
            "the testbench must still allow `{body}`, got:\n{out}"
        );
        assert!(
            out.contains("test result: ok"),
            "and the test should run, got:\n{out}"
        );
    }
}
