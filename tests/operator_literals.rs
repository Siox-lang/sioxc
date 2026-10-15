//! Operators whose operand is a literal, a nested custom operator, or a
//! product, computed in hardware and in the testbench against hand-worked
//! values. Found by a generated differential: each engine disagreed with
//! exact two's-complement arithmetic in at least one of these.
//!
//! - `5 xor d` was rejected; `d xor 5` was not.
//! - Nested `xnor`/`nand` complemented within one bit in hardware.
//! - A product read by another operator was not wrapped in hardware:
//!   `(c * d) / b` divided 405, not 149.
//! - A literal dividend divided a signed operand as unsigned (`102 / c`).
//! - A literal compared with a signed operand compared unsigned in hardware
//!   (`c > 100` with `c = -7`).
//! - A signed division by an `and`/`or` expression failed to lower.
//! - Literal-only arithmetic wrapped in 64 bits (`0 - 17262465143367426459`).

#[cfg(feature = "llvm")]
fn run(name: &str, source: &str) -> String {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!(
        "siox_operator_literals_{name}_{}",
        std::process::id()
    ));
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

/// Each `(expression, expected)` as a hardware output and a testbench local
/// of `ty`, with `a, b, c, d` given; returns the failures.
#[cfg(feature = "llvm")]
fn check(name: &str, ty: &str, inputs: [&str; 4], cases: &[(&str, i128)]) {
    let family = ty.split('[').next().unwrap();
    let outputs = (0..cases.len())
        .map(|i| format!("o{i}: {ty} out"))
        .collect::<Vec<_>>()
        .join(", ");
    let drivers = cases
        .iter()
        .enumerate()
        .map(|(i, (e, _))| format!("o{i} = {e};"))
        .collect::<String>();
    let locals = cases
        .iter()
        .enumerate()
        .map(|(i, (e, _))| format!("let t{i}: {ty} = {e};"))
        .collect::<String>();
    let prints = (0..cases.len())
        .map(|i| format!("print!(\"|{i} {{}} {{}}\\n\", dut.o{i}, t{i});"))
        .collect::<String>();
    let [a, b, c, d] = inputs;
    let source = format!(
        "module m;\nuse std::bits::{family};\n\
         entity D {{ a: {ty} in, b: {ty} in, c: {ty} in, d: {ty} in, {outputs} }}\n\
         impl D {{ {drivers} }}\n\
         #[test] entity T {{}}\n\
         impl T {{\n\
             let a: {ty} = {a}; let b: {ty} = {b}; let c: {ty} = {c}; let d: {ty} = {d};\n\
             let dut: D = {{ .a = a, .b = b, .c = c, .d = d }};\n\
             check: process {{ {locals} await 1ns; {prints} }}\n\
         }}\n"
    );
    let report = run(name, &source);
    let failures = cases
        .iter()
        .enumerate()
        .filter(|(i, (_, expected))| !report.contains(&format!("|{i} {expected} {expected}\n")))
        .map(|(_, (e, expected))| format!("`{e}` expected {expected} in both engines"))
        .collect::<Vec<_>>();
    assert!(failures.is_empty(), "{failures:#?}\n{report}");
}

#[cfg(feature = "llvm")]
#[test]
fn unsigned_operators_with_literals_and_nesting() {
    // a = 77, b = 200, c = 45, d = 9
    check(
        "unsigned",
        "unsigned[8]",
        ["77", "200", "45", "9"],
        &[
            ("5 xor d", 12),
            ("250 xor d", 243),
            ("3 xnor d", 245),
            ("7 nor d", 240),
            ("(b xnor 123) nand ((not c) nand c)", 179),
            ("(b xnor 123) nand c", 243),
            ("(c xor a) nor c", 146),
            ("(c * d) / b", 0),
            ("(c * d) % b", 149),
            ("(c * d) >> 4", 9),
            ("((b and 93) xor (c * d)) / b", 1),
        ],
    );
}

#[cfg(feature = "llvm")]
#[test]
fn signed_operators_with_literals() {
    // a = -99, b = 55, c = -7, d = 3
    check(
        "signed",
        "signed[8]",
        ["0 - 99", "55", "0 - 7", "3"],
        &[
            ("102 / c", -14),
            ("102 % c", 4),
            ("102 / (c and c)", -14),
            ("102 % (c and c)", 4),
            ("102 / (d nand 98)", -34),
            ("a / (c or 24)", 14),
            ("a % (c and 24)", -3),
            ("5 xor c", -4),
            ("if 100 < c { 1 } else { 0 }", 0),
            ("if 100 > c { 1 } else { 0 }", 1),
            ("if c > 100 { 1 } else { 0 }", 0),
            ("if c <= 100 { 1 } else { 0 }", 1),
            ("if 0 >= c { 1 } else { 0 }", 1),
            // The most negative dividend: its magnitude does not fit signed.
            ("(0 - 128) % 87", -41),
            ("(b << 7) % 87", -41),
            ("(b << 7) / 87", -1),
        ],
    );
}

#[cfg(feature = "llvm")]
#[test]
fn literal_arithmetic_is_exact_in_its_vector() {
    let report = run(
        "fold",
        "module m;\nuse std::bits::{signed, unsigned};\n#[test] entity T {}\n\
         impl T { check: process {\n\
             let k: signed[65] = 0 - 17262465143367426459;\n\
             let l: signed[128] = 0 - 52603218775442905902131428009360323762;\n\
             let m: unsigned[70] = 18446744073709551616 + 1;\n\
             let n: signed[8] = (0 - 7) / 2;\n\
             print!(\"|{} {} {} {}\\n\", k, l, m, n);\n\
         } }\n",
    );
    assert!(
        report.contains(
            "|-17262465143367426459 -52603218775442905902131428009360323762 18446744073709551617 -3"
        ),
        "{report}"
    );
}

/// Declared at the test's top level, a vector initialized from literal
/// arithmetic was stored as the initializer's `integer`, at 64 bits: as
/// testbench state, and as a signal driving a DUT port.
#[cfg(feature = "llvm")]
#[test]
fn top_level_declarations_keep_their_vector_type() {
    let report = run(
        "top_level",
        "module m;
use std::bits::signed;
         entity E { c: signed[65] in, y: signed[65] out, }
impl E { y = c; }
         #[test] entity T {}
         impl T {
             let c: signed[65] = 0 - 17262465143367426459;
             let k: signed[65] = 0 - 17262465143367426459;
             let e: E = { .c = c };
             check: process { await 1ns; print!(\"|{} {} {}\\n\", c, k, e.y); }
         }
",
    );
    assert!(
        report.contains("|-17262465143367426459 -17262465143367426459 -17262465143367426459"),
        "{report}"
    );
}

/// std's signed `/`, `%` and order tested the sign through the 64-bit kernel
/// `integer`: wrong in hardware at 64 bits (the shift left -1, not 1) and
/// lost above 64 bits.
#[cfg(feature = "llvm")]
#[test]
fn wide_signed_division_and_order() {
    for ty in ["signed[64]", "signed[65]", "signed[128]"] {
        check(
            &ty.replace(['[', ']'], "_"),
            ty,
            ["0 - 100", "14", "3", "0 - 128"],
            &[
                ("a / b", -7),
                ("a % b", -2),
                ("60 or (a / b)", -3),
                ("if a < c { 1 } else { 0 }", 1),
                ("if c > a { 1 } else { 0 }", 1),
                // A literal divisor, and both operands negative.
                ("a / (0 - 2)", 50),
                ("d / (0 - 2)", 64),
                ("a % (0 - 3)", -1),
                ("(0 - 2) >> 1", -1),
            ],
        );
    }
}

/// A signed `>>` by a run-time amount had no lowering for the testbench's
/// metavalue companion; a constant compared with an operator result in
/// hardware kept all 64 bits of a negative constant (`(a + 0) == (0 - 5)`
/// was false).
#[cfg(feature = "llvm")]
#[test]
fn variable_signed_shifts_and_negative_constants() {
    for ty in ["signed[8]", "signed[65]"] {
        check(
            &format!("shift_{}", ty.replace(['[', ']'], "_")),
            ty,
            ["0 - 100", "3", "0", "100"],
            &[
                ("a >> b", -13),
                ("a >> c", -100),
                ("a >> d", -1),
                ("d >> b", 12),
                ("if (a >> 1) == (0 - 50) { 1 } else { 0 }", 1),
                ("if (0 - 50) == (a >> 1) { 1 } else { 0 }", 1),
                ("if (a + 0) != (0 - 100) { 1 } else { 0 }", 0),
            ],
        );
    }
}

#[cfg(feature = "llvm")]
fn diagnostics(name: &str, source: &str) -> String {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let path = std::env::temp_dir().join(format!("siox_operator_literals_{name}.siox"));
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, source),
            Emit::Metadata,
        ))
        .render_diagnostics()
}

/// Writes the checker accepted and nothing could lower.
#[cfg(feature = "llvm")]
#[test]
fn constants_and_loop_variables_are_not_assignable_out_of_range() {
    let test = |body: &str| {
        format!(
            "module m;\nuse std::bits::unsigned;\nconst K: integer = 3;\n\
             #[test] entity T {{}}\nimpl T {{ check: process {{ {body} }} }}\n"
        )
    };
    for (name, body, expected) in [
        ("fit", "let k: unsigned[8] = 300;", "does not fit"),
        (
            "fit_assign",
            "let k: unsigned[8] = 0; k = 256;",
            "does not fit",
        ),
        (
            "fit_shift",
            "let k: unsigned[8] = 1; let j: unsigned[8] = k << 300;",
            "does not fit",
        ),
        ("const", "K = 4;", "which is a `const`"),
        (
            "loop",
            "for i in 0..4 { i = i + 1; }",
            "which is a `for` loop variable",
        ),
    ] {
        let rendered = diagnostics(name, &test(body));
        assert!(rendered.contains(expected), "{name}: {rendered}");
    }
    let shadowed = diagnostics(
        "shadowed",
        &test("for i in 0..4 { let i: integer = 0; i = 1; print!(\"{}\", i); }"),
    );
    assert!(!shadowed.contains("error"), "{shadowed}");
}
