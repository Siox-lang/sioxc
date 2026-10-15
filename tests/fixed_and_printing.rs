//! Fixed-point operators, real printing and newtype printing, found by a
//! generated fixed-point differential (both engines against exact dyadic
//! arithmetic):
//!
//! - `(if p { a } else { b }) * c` on `sfixed` failed to lower in hardware:
//!   an `if` operand had no `self'low`.
//! - `a < 3` on `ufixed` compared the raw word 3 in hardware, not 3.0.
//! - An integer stored into a fixed format was its word: `= 1` was 1/16.
//! - `{}` of a real kept six digits (`166.215`), where Rust's is exact.
//! - `{:?}` of a newtype over `Logic[N]` (and so of `sfixed`) did not
//!   generate, and `B("0110")` for `struct B(Bit[4])` did not lower.

#[cfg(feature = "llvm")]
fn compile(name: &str, source: &str) -> siox::compiler::Compilation {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir =
        std::env::temp_dir().join(format!("siox_fixed_printing_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join(format!("{name}.siox")), source),
            Emit::TestExecutable,
        )
        .with_output(dir.join(name)),
    )
}

#[cfg(feature = "llvm")]
fn run(name: &str, source: &str) -> String {
    let compilation = compile(name, source);
    assert!(
        compilation.succeeded(),
        "{} {:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    let binary = std::env::temp_dir()
        .join(format!("siox_fixed_printing_{name}_{}", std::process::id()))
        .join(name);
    let output = std::process::Command::new(&binary).output().unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Each expression as a DUT output and a testbench local of `ty`; the
/// report must hold `expected` twice.
#[cfg(feature = "llvm")]
fn check(name: &str, ty: &str, family: &str, cases: &[(&str, &str)]) {
    let outputs = (0..cases.len())
        .map(|i| format!("o{i}: {ty} out"))
        .collect::<Vec<_>>()
        .join(", ");
    let drivers: String = cases
        .iter()
        .enumerate()
        .map(|(i, (e, _))| format!("o{i} = {e};"))
        .collect();
    let locals: String = cases
        .iter()
        .enumerate()
        .map(|(i, (e, _))| format!("let t{i}: {ty} = {e};"))
        .collect();
    let prints: String = (0..cases.len())
        .map(|i| format!("print!(\"|{i} {{}} {{}}\\n\", dut.o{i}, t{i});"))
        .collect();
    let source = format!(
        "module m;\nuse std::fixed::{family};\n\
         entity D {{ a: {ty} in, b: {ty} in, c: {ty} in, {outputs} }}\n\
         impl D {{ {drivers} }}\n\
         #[test] entity T {{}}\n\
         impl T {{\n\
             let a: {ty} = {ty}(2.3125); let b: {ty} = {ty}(0.0 - 3.1875); let c: {ty} = {ty}(4.625);\n\
             let dut: D = {{ .a = a, .b = b, .c = c }};\n\
             check: process {{ {locals} await 1ns; {prints} }}\n\
         }}\n"
    );
    let report = run(name, &source);
    let failures: Vec<_> = cases
        .iter()
        .enumerate()
        .filter(|(i, (_, expected))| !report.contains(&format!("|{i} {expected} {expected}\n")))
        .map(|(_, (e, expected))| format!("`{e}` expected {expected} in both engines"))
        .collect();
    assert!(failures.is_empty(), "{failures:#?}\n{report}");
}

#[cfg(feature = "llvm")]
#[test]
fn fixed_point_if_operands_and_integer_comparisons() {
    // b saturates to 0 in ufixed; in sfixed b * c = -14.74 wraps to 1.25.
    check(
        "sfixed",
        "sfixed<8, 4>",
        "sfixed",
        &[
            ("(if a < b { a } else { b }) * c", "1.25"),
            ("(if a < b { a } else { b }) / c", "-0.75"),
            ("if a < 3 { c } else { b }", "4.625"),
            ("if (a + 0) >= 3 { c } else { b }", "-3.1875"),
            ("if 3 > a { c } else { b }", "4.625"),
            ("a + 2", "4.3125"),
            // An order inside a receiver is the caller's, not recursion:
            // marked as recursing, `b > c` compared unsigned and chose `b`.
            (
                "if (if b > c { b } else { c }) < c { a } else { b }",
                "-3.1875",
            ),
        ],
    );
    check(
        "ufixed",
        "ufixed<16, 8>",
        "ufixed",
        &[
            ("if a < 3 { c } else { a }", "4.625"),
            ("if a > 2 { c } else { a }", "4.625"),
            ("if a <= 2 { c } else { a }", "2.3125"),
            ("(if a < c { a } else { c }) * c", "10.6953125"),
        ],
    );
}

#[cfg(feature = "llvm")]
#[test]
fn integers_do_not_store_into_a_fixed_format() {
    let program = |body: &str| {
        format!(
            "module m;\nuse std::fixed::ufixed;\n#[test] entity T {{}}\n\
             impl T {{ check: process {{ {body} print!(\"{{}}\", x); }} }}\n"
        )
    };
    for body in [
        "let x: ufixed<8, 4> = 1;",
        "let k: integer = 3; let x: ufixed<8, 4> = k;",
        "let x: ufixed<8, 4> = ufixed<8, 4>(1); x = 2;",
    ] {
        let compilation = compile("store", &program(body));
        assert!(
            compilation
                .render_diagnostics()
                .contains("without an explicit conversion"),
            "{body}: {}",
            compilation.render_diagnostics()
        );
    }
    let accepted = run(
        "zero",
        &program("let x: ufixed<8, 4> = 0; x = ufixed<8, 4>(3);"),
    );
    assert!(accepted.contains('3'), "{accepted}");
}

#[cfg(feature = "llvm")]
#[test]
fn reals_print_as_rust_does() {
    let report = run(
        "reals",
        "module m;\n#[test] entity T {}\nimpl T { check: process {\n\
             let x: real = 166.21484375; let y: real = 0.1 + 0.2; let z: real = 1.0;\n\
             let big: real = 100000000000000000000.0; let small: real = 0.00001;\n\
             print!(\"|{} {} {} {:?} {} {:?} {:?}\\n\", x, y, z, z, big, big, small);\n\
         } }\n",
    );
    assert!(
        report
            .contains("|166.21484375 0.30000000000000004 1 1.0 100000000000000000000 1e20 1e-5\n"),
        "{report}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn logic_newtypes_print_and_construct() {
    let report = run(
        "newtypes",
        "module m;\nuse std::logic::{Logic, Bit};\nuse std::fixed::sfixed;\n\
         struct B(Bit[4]);\n\
         #[test] entity T {}\nimpl T { check: process {\n\
             let s: sfixed<8, 4> = sfixed<8, 4>(0.0 - 1.5);\n\
             let b: B = B(\"0110\");\n\
             print!(\"|{:?} {} {:?}\\n\", s, s, b);\n\
         } }\n",
    );
    assert!(report.contains("|11101000 -1.5 0110\n"), "{report}");
}
