//! Regressions for three checks that passed the checker and failed in
//! native lowering:
//! - sioxc #566: a positional literal shorter than its struct (`{ 42 }`)
//!   is warned like a named one and leaves the rest at the default;
//! - sioxc #570: a width-less vector with no initializer (`let u: unsigned;`)
//!   is reported, while one sized by its initializer still runs;
//! - sioxc #571: `'length` and the other range attributes on a struct or a
//!   tuple are reported.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i566_{name}_{}", std::process::id()));
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

#[cfg(feature = "llvm")]
fn runs(name: &str, source: &str) -> String {
    let (ok, text, binary) = compile(name, source);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout).to_string();
    assert!(run.status.success(), "{stdout}");
    text
}

#[cfg(feature = "llvm")]
#[test]
fn a_short_positional_literal_defaults_the_rest() {
    let text = runs("short", SHORT);
    assert!(text.contains("leaves `y` at the default value"), "{text}");
}

#[test]
fn a_width_less_vector_needs_a_width_or_an_initializer() {
    let (ok, text, _) = compile("unsized", UNSIZED);
    assert!(!ok, "{text}");
    assert!(text.contains("unconstrained vector type for `u`"), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_width_less_vector_takes_its_initializers_width() {
    runs("sized", SIZED);
}

#[test]
fn range_attributes_on_a_struct_or_tuple_are_reported() {
    let (ok, text, _) = compile("attrs", ATTRS);
    assert!(!ok, "{text}");
    assert!(
        text.contains("`'length` reads an array's range, and `p571::Point` has fields"),
        "{text}"
    );
    assert!(
        text.contains("`'high` reads an array's range, and `(integer, integer)` has fields"),
        "{text}"
    );
    assert_eq!(text.matches("error[").count(), 2, "{text}");
}

const SHORT: &str = r#"module p566;

use std::bits::unsigned;

struct Point {
    pub x: unsigned[8],
    pub y: unsigned[8],
}

entity Dut {
    clk: Bit in,
    p_cont: Point out,
    p_clk: Point out,
}

impl Dut {
    p_cont = { 42 };

    process {
        if clk.rising() {
            p_clk = { 7 };
        }
    }
}

#[test]
entity TB {}
impl TB {
    let clk: Bit;
    let a: Point;
    let b: Point;
    let dut: Dut = { .clk = clk, .p_cont = a, .p_clk = b };
    run: process {
        await 1ns;
        clk = '1';
        await 1ns;
        assert!(a.x == 42 and a.y == 0, "cont {} {}", a.x, a.y);
        assert!(b.x == 7 and b.y == 0, "clk {} {}", b.x, b.y);
        let p: Point = { 5 };
        assert!(p.x == 5 and p.y == 0, "local");
        print!("done\n");
    }
}
"#;

const UNSIZED: &str = r#"module test_hw;
use std::bits::unsigned;

entity Top {
    val: integer out,
}

impl Top {
    let u: unsigned; // Unconstrained vector signal without length
    process {
        u = unsigned[8](100);
        val = integer(u);
    }
}

#[test] entity TB {}
impl TB {
    let val: integer;
    let dut: Top = { .val = val };
    process {
        await 1ns;
        print!("HW val: {}\n", val);
    }
}
"#;

const SIZED: &str = r#"module p570;
use std::bits::unsigned;
#[test] entity TB {}
impl TB {
    process {
        let u: unsigned = unsigned[4](3);
        assert!(u == 3, "sized by the initializer");
        print!("done\n");
    }
}
"#;

const ATTRS: &str = r#"module p571;
use std::bits::unsigned;
struct Point { pub x: integer, pub y: integer }
struct Byte(Bit[8]);
#[test] entity TB {}
impl TB {
    run: process {
        let p: Point = { .x = 1, .y = 2 };
        let t: (integer, integer) = (1, 2);
        let v: unsigned[8] = 3;
        let b: Byte = Byte(v);
        print!("{} {} {} {}\n", p'length, t'high, v'length, b'length);
    }
}
"#;
