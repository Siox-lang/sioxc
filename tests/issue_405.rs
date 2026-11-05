//! Regressions for sioxc #405: `#[derive(Eq)]` on a newtype over a plain
//! array (`struct Byte(Bit[8])`) compares element by element instead of
//! calling the array type as a conversion, and an array of bit characters
//! builds the newtype as the bit string it spells.

use std::process::Command;

fn passes(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i405_{name}_{}", std::process::id()));
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
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[cfg(feature = "llvm")]
#[test]
fn an_array_newtype_derives_eq_by_element() {
    passes("eq", EQ);
}

#[cfg(feature = "llvm")]
#[test]
fn an_array_of_bit_characters_builds_the_newtype() {
    passes("literal", LITERAL);
}

const EQ: &str = r#"module t405_ord;

#[derive(Eq)]
pub struct Byte(Bit[8]);

#[test]
entity Tb {}

impl Tb {
    process {
        let a: Byte = Byte("00000001");
        let b: Byte = Byte("00000010");
        let c: Byte = Byte(['0', '0', '0', '0', '0', '0', '0', '1']);
        assert!(a == c, "a == c");
        assert!(a != b, "a != b");
    }
}
"#;

const LITERAL: &str = r#"module test_byte_array_literal;
use std::sim::*;

pub struct Byte(Bit[8]);

#[test]
entity Tb {}

impl Tb {
    process {
        let b1: Byte = Byte(['0', '1', '0', '1', '0', '1', '0', '1']);
    }
}
"#;
