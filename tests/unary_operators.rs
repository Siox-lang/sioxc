//! Custom prefix operators, `UnaryOperator<"op", Out>` (binary-and-unary-
//! operators proposal, owner rulings of 2026-11-27): declared precedence
//! groups the operand, a symbol may be both prefix and infix, `"op"(..)`
//! calls an operator by name, and an operator word is reserved.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn prefix_operators_group_by_precedence_and_run() {
    let dir = std::env::temp_dir().join(format!("siox_unary_ops_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("u.siox");
    std::fs::write(&file, SOURCE).unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
        ])
        .arg(&file)
        .arg("-o")
        .arg(dir.join("t.bin"))
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "{stdout}");
    assert!(
        stdout.contains("p=16 q=9 r=4 s=5 t=102 u=5 bit='1'"),
        "{stdout}"
    );
}

fn diagnostics(name: &str, source: &str) -> String {
    let path = std::env::temp_dir().join(format!("siox_unary_ops_{name}.siox"));
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, source),
            Emit::Metadata,
        ))
        .render_diagnostics()
}

/// Parse errors: a reserved operator word, and a string-name call whose
/// operand count no operator takes.
#[test]
fn operator_words_are_reserved() {
    let text = diagnostics("words", BAD);
    for message in [
        "`flip` is a custom operator here, so it cannot also name a function, variable or parameter",
        "no custom operator `flip` takes 2 operand(s)",
    ] {
        assert!(text.contains(message), "missing `{message}`:\n{text}");
    }
}

/// A checker error: a prefix operator with no impl for its operand's type.
#[test]
fn a_prefix_operator_needs_an_impl() {
    let text = diagnostics("impl", MISSING);
    assert!(
        text.contains("`flip` needs an `impl UnaryOperator<\"flip\", Out> for unsigned`"),
        "{text}"
    );
}

const SOURCE: &str = r#"module uop2;
// precedence 85: the operand takes `*` (90), not `+` (80)
impl UnaryOperator<"twice", unsigned[8]> for unsigned {
    attr precedence = 85;
    fn apply() -> unsigned[8] { return unsigned[8](self + self); }
}
// precedence 100: binds tighter than `*`
impl UnaryOperator<"inc", unsigned[8]> for unsigned {
    attr precedence = 100;
    fn apply() -> unsigned[8] { return unsigned[8](self + 1); }
}
// punctuation, and the same word both unary and binary
impl UnaryOperator<"~~", Bit> for Bit {
    attr precedence = 100;
    fn apply() -> Bit { return self; }
}
impl UnaryOperator<"fold", unsigned[8]> for unsigned {
    attr precedence = 100;
    fn apply() -> unsigned[8] { return unsigned[8](self + 100); }
}
impl BinaryOperator<"fold", unsigned, unsigned[8]> for unsigned {
    attr precedence = 70;
    fn apply(rhs: unsigned) -> unsigned[8] { return unsigned[8](self + rhs); }
}
#[test] entity Tb {}
impl Tb {
    let a: unsigned[8] = 2; let b: unsigned[8] = 3; let c: unsigned[8] = 4;
    let bit: Bit = '1';
    await 1ns;
    let p: unsigned[8] = twice a * b + c;   // twice(2*3) + 4 = 16
    let q: unsigned[8] = inc a * b;         // (inc 2) * 3 = 9
    let r: unsigned[8] = "twice"(a);        // 4
    let s: unsigned[8] = "fold"(a, b);      // 5
    let t: unsigned[8] = fold a;            // 102
    let u: unsigned[8] = a fold b;          // 5
    print!("p={} q={} r={} s={} t={} u={} bit={}\n", p, q, r, s, t, u, ~~bit);
}
"#;

const BAD: &str = r#"module uop3;
impl UnaryOperator<"flip", Bit> for Bit {
    attr precedence = 100;
    fn apply() -> Bit { return self; }
}
fn flip(x: Bit) -> Bit { return x; }
#[test] entity Tb {}
impl Tb {
    let a: unsigned[8] = 1;
    let y: unsigned[8] = flip a;
    let z: Bit = "flip"('1', '0');
}
"#;

const MISSING: &str = r#"module uop4;
impl UnaryOperator<"flip", Bit> for Bit {
    attr precedence = 100;
    fn apply() -> Bit { return self; }
}
#[test] entity Tb {}
impl Tb {
    let a: unsigned[8] = 1;
    let y: unsigned[8] = flip a;
}
"#;
