//! Regressions for sioxc #421 (enum discriminants from `2^63` left the
//! enum one bit wide and collided with zero) and #422 (`randint` ordered
//! its `integer` bounds as unsigned and drew far outside a range crossing
//! zero).

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i421_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
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
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text, dir.join("t.bin"))
}

fn runs(name: &str, source: &str) {
    let (ok, text, binary) = compile(name, source);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[cfg(feature = "llvm")]
#[test]
fn wide_discriminants_keep_their_bits_through_a_port() {
    runs("wide", WIDE);
}

#[test]
fn a_discriminant_past_64_bits_is_an_error() {
    let (ok, text, _) = compile("past", PAST);
    assert!(!ok, "{text}");
    assert!(
        text.contains("past the 64 bits a discriminant holds"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn randint_stays_in_a_signed_range() {
    runs("randint", RANDINT);
}

const WIDE: &str = r#"module p421;

enum Foo {
    Zero = 0,
    Big = 0x8000_0000_0000_0000,
}

enum Bar {
    Zero = 0,
    B = 0x7fff_ffff_ffff_ffff,
    C,
}

entity Dut {
    f_in: Foo in,
    f_out: Foo out,
    b_in: Bar in,
    b_out: Bar out,
}
impl Dut {
    f_out = f_in;
    b_out = b_in;
}

#[test]
entity Tb {}
impl Tb {
    let f_in: Foo;
    let f_out: Foo;
    let b_in: Bar;
    let b_out: Bar;
    let d: Dut = { .f_in = f_in, .f_out = f_out, .b_in = b_in, .b_out = b_out };
    check: process {
        f_in = Foo::Big;
        b_in = Bar::C;
        await 1ns;
        assert!(f_out == Foo::Big and f_out != Foo::Zero, "Foo::Big");
        assert!(b_out == Bar::C and b_out != Bar::Zero, "Bar::C");
    }
}
"#;

const PAST: &str = r#"module p421_err;

enum Over {
    Top = 0xFFFF_FFFF_FFFF_FFFF,
    Past,
}
"#;

const RANDINT: &str = r#"module test_randint_signed;

#[test]
entity Tb {}

impl Tb {
    check: process {
        for i in 0..200 {
            let r: integer = randint(-5, 5);
            let s: integer = randint(5, -5);
            let t: integer = randint(-9, -7);
            assert!(r >= -5 and r <= 5, "r={}", r);
            assert!(s >= -5 and s <= 5, "s={}", s);
            assert!(t >= -9 and t <= -7, "t={}", t);
        }
    }
}
"#;
