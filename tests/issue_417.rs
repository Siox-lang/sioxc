//! Regression for sioxc #417: binary64 (`float<64, 52>`) multiplies,
//! divides and takes square roots. Its significand product (106 bits),
//! dividend (107) and radicand (110) overflowed the 64-bit `integer` the
//! std bodies worked in, and each read 0. They are taken over
//! `unsigned[128]` past the kernel word, and round as IEEE `real` does.

use std::process::Command;

fn prints(name: &str, source: &str, expect: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i417_{name}_{}", std::process::id()));
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
        "{name}: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains(expect),
        "{name}: {stdout}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn binary64_arithmetic_is_ieee() {
    prints("issue", ISSUE, "mul = 4, div = 1, root = 2");
    // Each result equals the `real` (IEEE binary64) operation, rounding
    // included: `0.1 * 3` is `0.30000000000000004`.
    prints("rounding", ROUNDING, "true true true true true");
}

const ISSUE: &str = r#"module test_float64;

use std::float::float;

#[test]
entity Tb {}

impl Tb {
    process {
        let a: float<64, 52> = float<64, 52>(2.0);

        let mul: float<64, 52> = a * a;
        let div: float<64, 52> = a / a;
        let root: float<64, 52> = float<64, 52>(4.0).sqrt();

        print!("mul = {}, div = {}, root = {}\n", mul, div, root);
        assert!(mul == float<64, 52>(4.0));
        assert!(div == float<64, 52>(1.0));
        assert!(root == float<64, 52>(2.0));
    }
}
"#;

const ROUNDING: &str = r#"module r64;
use std::float::float;
#[test]
entity Tb {}
impl Tb {
    process {
        let one: float<64, 52> = float<64, 52>(1.0);
        let three: float<64, 52> = float<64, 52>(3.0);
        let tenth: float<64, 52> = float<64, 52>(0.1);
        let two: float<64, 52> = float<64, 52>(2.0);
        let x: float<64, 52> = float<64, 52>(1.7);
        let y: float<64, 52> = float<64, 52>(0.0 - 2.9);
        print!("{} {} {} {} {}\n",
            real(one / three) == 1.0 / 3.0,
            real(tenth * three) == 0.1 * 3.0,
            real(two.sqrt()) == 1.4142135623730951,
            real(x * y) == 1.7 * (0.0 - 2.9),
            real(y / x) == (0.0 - 2.9) / 1.7);
        print!("{} {} {}\n", real(one / three), real(tenth * three), real(two.sqrt()));
    }
}
"#;
