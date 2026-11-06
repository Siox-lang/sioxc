//! Regression for sioxc #484: elaboration reads an integer literal from
//! `2^63` to `u64::MAX` (`0x8000_0000_0000_0000`) as its 64-bit pattern, so
//! a generic entity argument and a generate `if` condition written with one
//! are constants.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_wide_literal_is_an_elaboration_constant() {
    let dir = std::env::temp_dir().join(format!("siox_i484_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("wide.siox");
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
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const SOURCE: &str = r#"module p484;

entity Sub<MASK: integer> {
    val: unsigned[64] out,
}

impl<MASK: integer> Sub<MASK> {
    val = MASK;
}

entity Top {
    y: unsigned[64] out,
    z: unsigned[64] out,
}

impl Top {
    let s: Sub<0x8000_0000_0000_0000> = { .val = y };
    if 0x8000_0000_0000_0000 != 0 {
        z = 7;
    } else {
        z = 0;
    }
}

#[test]
entity Tb {}

impl Tb {
    let y: unsigned[64];
    let z: unsigned[64];
    let dut: Top = { .y = y, .z = z };
    check: process {
        await 1ns;
        assert!(y == 0x8000_0000_0000_0000, "y={}", y);
        assert!(z == 7, "z={}", z);
    }
}
"#;
