//! Regression for sioxc #544: `std::math::rem` is `%`, so a `real`
//! remainder is the true one (`rem(5.5, 2.0)` is 1.5, `mod(-5.5, 2.0)` is
//! 0.5) rather than 0.0, and integer and vector remainders are unchanged.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn real_remainders_are_not_zero() {
    let dir = std::env::temp_dir().join(format!("siox_i544_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("rem.siox");
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
    assert!(run.status.success() && stdout.contains("done"), "{stdout}");
}

const SOURCE: &str = r#"module p544;

use std::math::{rem, mod};
use std::bits::{signed, unsigned};

#[test]
entity Tb {}
impl Tb {
    check: process {
        let a: real = 5.5;
        let n: real = 0.0 - 5.5;
        assert!(rem(a, 2.0) == 1.5 and rem(n, 2.0) == 0.0 - 1.5, "real rem");
        assert!(mod(a, 2.0) == 1.5 and mod(n, 2.0) == 0.5, "real mod");
        assert!(rem(0 - 7, 2) == 0 - 1 and mod(0 - 7, 2) == 1, "integer");
        let s: signed[8] = 0 - 7;
        let two: signed[8] = 2;
        assert!(rem(s, two) == 0 - 1 and mod(s, two) == 1, "signed");
        let u: unsigned[8] = 7;
        let three: unsigned[8] = 3;
        assert!(rem(u, three) == 1 and mod(u, three) == 1, "unsigned");
        print!("done\n");
    }
}
"#;
