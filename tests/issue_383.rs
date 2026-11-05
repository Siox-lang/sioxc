//! Regressions for sioxc #383: `#[derive(Eq, Ord)]` on a newtype over a
//! tuple compares its positions, which the newtype exposes as fields; a tuple
//! literal built straight into the newtype takes its positions' widths; and a
//! newtype over a struct keeps its fields through `Pair(w)`, in a testbench
//! and in hardware.

use std::process::Command;

fn passes(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i383_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
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
    let text = String::from_utf8_lossy(&run.stdout).to_string();
    assert!(run.status.success(), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_tuple_newtype_derives_by_position() {
    passes("tuple", TUPLE);
}

#[cfg(feature = "llvm")]
#[test]
fn a_struct_newtype_converts_with_its_fields() {
    passes("struct", STRUCT);
}

#[cfg(feature = "llvm")]
#[test]
fn a_struct_newtype_converts_in_hardware() {
    passes("hardware", HARDWARE);
}

const TUPLE: &str = r#"module test_derive_tuple_more;

#[derive(Eq, Ord)]
pub struct Pair((unsigned[8], unsigned[8]));

#[test]
entity Tb {}
impl Tb {
    check: process {
        let a: Pair = Pair((1, 2));
        let b: Pair = Pair((1, 3));
        let c: Pair = Pair((1, 2));
        assert!(a != b, "a != b");
        assert!(a == c, "a == c");
        assert!(a < b and a <= c and not (b < a), "ordered by position");
    }
}
"#;

const STRUCT: &str = r#"module test_struct_newtype_probe;

#[derive(Eq, Ord)]
pub struct Pair {
    pub a: unsigned[8],
    pub b: unsigned[8],
}

#[derive(Eq, Ord)]
pub struct WrappedPair(Pair);

fn sum_pair(p: Pair) -> unsigned[8] {
    return p.a + p.b;
}

#[test]
entity Tb1 {}
impl Tb1 {
    check: process {
        let w1: WrappedPair = WrappedPair(Pair { .a = 1, .b = 2 });
        assert!(sum_pair(Pair(w1)) == 3, "sum_pair(Pair(w1))");
    }
}

#[test]
entity Tb2 {}
impl Tb2 {
    check: process {
        let w1: WrappedPair = WrappedPair(Pair { .a = 1, .b = 2 });
        let w2: WrappedPair = WrappedPair(Pair { .a = 1, .b = 3 });
        assert!(w1 != w2, "w1 != w2");
        assert!(w1 < w2, "w1 < w2");
    }
}
"#;

const HARDWARE: &str = r#"module test_struct_newtype_hw;
use std::sim::*;

pub struct Pair {
    pub a: unsigned[8],
    pub b: unsigned[8],
}

pub struct WrappedPair(Pair);

fn sum_pair(p: Pair) -> unsigned[8] {
    return p.a + p.b;
}

entity Top {
    w: WrappedPair in,
    y: unsigned[8] out,
}

impl Top {
    process {
        y = sum_pair(Pair(w)) + Pair(w).a;
    }
}

#[test]
entity Tb {}

impl Tb {
    let w: WrappedPair = WrappedPair(Pair { .a = 4, .b = 5 });
    let y: unsigned[8];
    let dut: Top = { .w = w, .y = y };

    process {
        await 1ns;
        assert!(y == 13, "hardware sum");
    }
}
"#;
