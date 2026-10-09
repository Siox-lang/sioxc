//! `print!` formatting: Rust-style specs, built-in struct/array/vector forms,
//! and `Display` impls, checked against the exact text a test executable
//! prints.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn print_formats_specs_composites_and_display_impls() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_print_format_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("formats");
    let source = r#"
module formats;
use std::float::float;
use std::fixed::{sfixed, ufixed};
use std::math::Complex;

enum Kind { Idle, Data }
struct Point { pub x: integer, pub y: integer }
struct Packet { pub kind: Kind, pub at: Point, pub payload: integer[3], pub flags: Logic[3..0] }
struct Pair { pub a: integer, pub b: integer }

impl Display for Pair {
    fn fmt(self, f: Formatter) {
        let total: integer = self.a + self.b;
        write!(f, "<{} and {} = {}>", self.a, self.b, total);
    }
}

#[test]
entity T {}

impl T {
    check: process {
        let t: real = 1.25;
        let big: real = 12345.678;
        let small: real = 0.000123;
        let word: integer = 31;
        let n: integer = 42;
        let neg: integer = 0 - 42;
        let u: unsigned[8] = 200;
        print!("t = {:.3} s", t);
        print!("{:.2e} {:e} {:E} {:.1e}", big, big, small, small);
        print!("0x{:x} {:#x} {:#X} {:b} {:#o}", word, word, word, word, word);
        print!("[{:>6}] [{:<6}] [{:^7}] [{:*^7}]", n, n, n, n);
        print!("[{:06}] [{:+}] [{:+06}] [{:#06x}]", neg, n, n, word);
        print!("{:e} {:.2e} {:x} {:08b}", 12345, 98765, neg, u);
        let p: Packet = Packet {
            .kind = Kind::Data,
            .at = Point { .x = 1, .y = 0 - 2 },
            .payload = [4, 5, 6],
            .flags = "10XZ",
        };
        let bits: Bit[7..0] = "10100101";
        print!("{}", p);
        print!("[{:<24}] bits = {}", p.at, bits);
        let y: float<32, 23> = float<32, 23>(0.0 - 2.25);
        let g: ufixed<8, 4> = ufixed<8, 4>(2.5);
        let z: Complex = Complex { .re = 1.0, .im = 2.5 };
        let pair: Pair = Pair { .a = 3, .b = 4 };
        print!("y = {}, {:.3}, {:e}; g = [{:>6}]", y, y, y, g);
        print!("z = {:.3}; pair = [{:^17}]", z, pair);
    }
}
"#;
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("formats.siox"), source),
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
    let output = Command::new(&binary)
        .output()
        .expect("test executable runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = [
        "t = 1.250 s",
        "1.23e4 1.2345678e4 1.23E-4 1.2e-4",
        "0x1f 0x1f 0x1F 11111 0o37",
        "[    42] [42    ] [  42   ] [**42***]",
        "[-00042] [+42] [+00042] [0x001f]",
        "1.2345e4 9.88e4 ffffffffffffffd6 11001000",
        "Packet { kind: Data, at: Point { x: 1, y: -2 }, payload: [4, 5, 6], flags: 10XZ }",
        "[Point { x: 1, y: -2 }   ] bits = 10100101",
        "y = -2.25, -2.250, -2.25e0; g = [   2.5]",
        "z = 1.000 + 2.500i; pair = [  <3 and 4 = 7>  ]",
    ];
    for line in expected {
        assert!(
            stdout.lines().any(|printed| printed == line),
            "missing `{line}` in:\n{stdout}"
        );
    }
    assert!(output.status.success(), "{stdout}");
}

/// A malformed spec, or one a type cannot take, is a compile error that says
/// what was wrong.
#[test]
fn malformed_and_mismatched_specs_are_reported() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let source = r#"
module specs;
#[test]
entity T {}
impl T {
    check: process {
        let r: real = 1.5;
        print!("{:q}", r);
        print!("{:x}", r);
        print!("{0}", r);
    }
}
"#;
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(SourceInput::memory("specs.siox", source), Emit::Metadata),
    );
    let diagnostics = compilation.render_diagnostics();
    assert!(
        diagnostics.contains("is not a format spec"),
        "{diagnostics}"
    );
    assert!(diagnostics.contains("writes integers"), "{diagnostics}");
    assert!(
        diagnostics.contains("arguments are taken in order"),
        "{diagnostics}"
    );
}

/// `{:?}` is the built-in form even for a type with a `Display` impl, and
/// quotes text the way Rust's `Debug` does; a real keeps its point.
#[cfg(feature = "llvm")]
#[test]
fn debug_placeholders_print_the_structural_form() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_print_debug_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("debug");
    let source = r#"
module debug;
use std::math::Complex;
enum Kind { Idle, Data }
struct Tag { pub name: string[2], pub c: Char, pub k: Kind }
struct Pair { pub a: integer, pub b: integer }
impl Display for Pair {
    fn fmt(self, f: Formatter) { write!(f, "<{} {}>", self.a, self.b); }
}
#[test] entity T {}
impl T {
    check: process {
        let p: Pair = Pair { .a = 1, .b = 2 };
        let t: Tag = Tag { .name = "hi", .c = 'z', .k = Kind::Data };
        let z: Complex = Complex { .re = 1.5, .im = 2.0 };
        print!("{} | {:?}", p, p);
        print!("{:?}", t);
        print!("{:?} {:?} {:?} {:?} [{:>21?}]", "hey", 'q', 42, z, p);
    }
}
"#;
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("debug.siox"), source),
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
    let output = Command::new(&binary)
        .output()
        .expect("test executable runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in [
        "<1 2> | Pair { a: 1, b: 2 }",
        "Tag { name: \"hi\", c: 'z', k: Data }",
        "\"hey\" 'q' 42 Complex { re: 1.5, im: 2.0 } [  Pair { a: 1, b: 2 }]",
    ] {
        assert!(
            stdout.lines().any(|printed| printed == line),
            "missing `{line}` in:\n{stdout}"
        );
    }
}
