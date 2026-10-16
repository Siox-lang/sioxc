//! `std::math::Complex` in both engines. The testbench failed what hardware
//! ran: `z + 2` bound `2` as a `Complex` (codegen rejected it), and `5i`
//! stayed an unlowered suffix. `-z` was accepted with no `Neg` impl, and
//! `2 - z` also reported a mistyped initializer. A DUT's aggregate port
//! printed directly (`print!("{}", dut.z)`) failed to lower, and a `Logic[4]`
//! one printed its raw word.

#[cfg(feature = "llvm")]
fn compile(name: &str, source: &str) -> (siox::compiler::Compilation, std::path::PathBuf) {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_complex_{name}_{}", std::process::id()));
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
    (compilation, binary)
}

#[cfg(feature = "llvm")]
#[test]
fn complex_arithmetic_agrees_in_both_engines() {
    let (compilation, binary) = compile(
        "arith",
        r#"module g;
use std::math::Complex;
use std::logic::Logic;
entity E { a: Complex in, b: Complex in, s: Complex out, i: Complex out, c: Complex out, l: Logic[4] in, o: Logic[4] out }
impl E { s = a + 2; i = 10 + 5i; c = (a + b) - (b + b) + 1; o = l; }
#[test] entity T {}
impl T {
    let a: Complex = { .re = 3.0, .im = 4.0 };
    let b: Complex = { .re = 1.0, .im = 0.0 - 2.0 };
    let l: Logic[4] = "01XZ";
    let e: E = { .a = a, .b = b, .l = l };
    check: process {
        let s: Complex = a + 2; let i: Complex = 10 + 5i; let c: Complex = (a + b) - (b + b) + 1;
        await 1ns;
        print!("|TB {} | {} | {} | {}", s, i, c, 2.5i);
        print!("|HW {} | {} | {} | {}", e.s, e.i, e.c, e.o);
    }
}
"#,
    );
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let output = std::process::Command::new(&binary).output().unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(
        report.contains("|TB 5 + 4i | 10 + 5i | 3 + 6i | 0 + 2.5i"),
        "{report}"
    );
    assert!(
        report.contains("|HW 5 + 4i | 10 + 5i | 3 + 6i | 01XZ"),
        "{report}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn missing_complex_operators_are_reported_once() {
    for (name, body, expected) in [
        ("neg", "let z: Complex = -a;", "no unary `-` for `Complex`"),
        (
            "int_sub",
            "let z: Complex = 2 - a;",
            "cannot apply `-` to integer and Complex",
        ),
    ] {
        let (compilation, _) = compile(
            name,
            &format!(
                "module g;\nuse std::math::Complex;\n#[test] entity T {{}}\n\
                 impl T {{ check: process {{ let a: Complex = {{ .re = 3.0, .im = 4.0 }}; {body} print!(\"{{}}\", z); }} }}\n"
            ),
        );
        let rendered = compilation.render_diagnostics();
        assert!(rendered.contains(expected), "{name}: {rendered}");
        assert_eq!(rendered.matches("error[").count(), 1, "{name}: {rendered}");
    }
}

/// A testbench literal of a struct with several suffix impls took whichever
/// `suffix` came first, not the one for its symbol.
#[cfg(feature = "llvm")]
#[test]
fn a_struct_suffix_is_chosen_by_its_symbol() {
    let (compilation, binary) = compile(
        "units",
        r#"module g;
use std::ops::Suffix;
struct L { pub mm: real }
impl Suffix<"mm", real> for L { fn suffix(v: real) -> L { return { .mm = v }; } }
impl Suffix<"cm", real> for L { fn suffix(v: real) -> L { return { .mm = v * 10.0 }; } }
impl Suffix<"km", real> for L { fn suffix(v: real) -> L { return { .mm = v * 1000000.0 }; } }
#[test] entity T {}
impl T { check: process { let a: L = 3.0cm; let b: L = 2.0km; let c: L = 7.0mm; print!("|{} {} {}", a.mm, b.mm, c.mm); } }
"#,
    );
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let output = std::process::Command::new(&binary).output().unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("|30 2000000 7"), "{report}");
}
