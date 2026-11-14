//! Regression for sioxc #538: fixed and float formats past the 64-bit kernel
//! word. `float<80, 64>(1.5)` read NaN and `float<128, 112>(1.5)` read 0;
//! the wide words are now built, read and computed over `unsigned[128]` and
//! `unsigned[256]`, every binary64 round-trips exactly, and arithmetic runs
//! at the format's own precision. A binary64 near its smallest normal also
//! read 0 back.

use std::process::Command;

fn prints(name: &str, source: &str, expect: &[&str], absent: &[&str]) {
    let dir = std::env::temp_dir().join(format!("siox_i538_{name}_{}", std::process::id()));
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
    assert!(run.status.success(), "{name}: {stdout}");
    for line in expect {
        assert!(stdout.contains(line), "{name}: {stdout}");
    }
    for line in absent {
        assert!(!stdout.contains(line), "{name}: {stdout}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn wide_formats_convert_with_real() {
    prints(
        "issue",
        ISSUE,
        &["ufx: 1.5", "sfx: 1.5", "fl80: 1.5", "fl128: 1.5"],
        &[],
    );
    prints("wide", WIDE, &["inf=inf nan=true"], &["false"]);
    prints("binary64", BINARY64, &["inf=inf nan=true"], &["false"]);
}

#[cfg(feature = "llvm")]
#[test]
fn wide_formats_compute() {
    // `float<128, 112>`: + - * / sqrt; `float<80, 64>`: * + neg /; order and
    // equality; and quad precision itself: `(1/3) * 3` rounds back to 1.
    prints(
        "arithmetic",
        ARITHMETIC,
        &[
            "3.75 -0.75 3.375 1.5 1.5",
            "-1.5 -2.5 3 -6",
            "true false true",
            "true true",
        ],
        &[],
    );
}

const ISSUE: &str = r#"module test_wide_numeric;

use std::fixed::{ufixed, sfixed};
use std::float::float;

#[test]
entity TB {}
impl TB {
    run: process {
        let ufx: ufixed<72, 36> = ufixed<72, 36>(1.5);
        let sfx: sfixed<72, 36> = sfixed<72, 36>(1.5);
        let fl80: float<80, 64> = float<80, 64>(1.5);
        let fl128: float<128, 112> = float<128, 112>(1.5);

        let ru: real = real(ufx);
        let rs: real = real(sfx);
        let r80: real = real(fl80);
        let r128: real = real(fl128);

        print!("ufx: {}", ru);
        print!("sfx: {}", rs);
        print!("fl80: {}", r80);
        print!("fl128: {}", r128);





    }
}
"#;

const WIDE: &str = r#"module rt538;
use std::float::float;
#[test]
entity Tb {}
impl Tb {
    process {
        let tiny: real = 1.0;
        let huge: real = 1.0;
        for k in 0..99 {
            tiny = tiny * 0.001;
            huge = huge * 1000.0;
        }
        let xs: real[6] = [3.141592653589793, 0.0 - tiny, huge, 0.1, 0.0 - 123456.789, 0.0];
        for i in 0..5 {
            let x: real = xs[i];
            let a: float<80, 64> = float<80, 64>(x);
            let b: float<128, 112> = float<128, 112>(x);
            print!("{} {}\n", real(a) == x, real(b) == x);
        }
        let inf: float<80, 64> = float<80, 64>(1.0 / 0.0);
        let nan: float<80, 64> = float<80, 64>(0.0 / 0.0);
        print!("inf={} nan={}\n", real(inf), real(nan) != real(nan));
    }
}
"#;

const BINARY64: &str = r#"module rt64;
use std::float::float;
#[test]
entity Tb {}
impl Tb {
    process {
        let tiny: real = 1.0;
        let huge: real = 1.0;
        for k in 0..99 {
            tiny = tiny * 0.001;
            huge = huge * 1000.0;
        }
        let xs: real[6] = [3.141592653589793, 0.0 - tiny, huge, 0.1, 0.0 - 123456.789, 0.0];
        for i in 0..5 {
            let x: real = xs[i];
            let a: float<64, 52> = float<64, 52>(x);
            let b: float<64, 52> = float<64, 52>(x);
            print!("{} {}\n", real(a) == x, real(b) == x);
        }
        let inf: float<64, 52> = float<64, 52>(1.0 / 0.0);
        let nan: float<64, 52> = float<64, 52>(0.0 / 0.0);
        print!("inf={} nan={}\n", real(inf), real(nan) != real(nan));
    }
}
"#;

const ARITHMETIC: &str = r#"module q538;
use std::float::float;
#[test]
entity Tb {}
impl Tb {
    process {
        let a: float<128, 112> = float<128, 112>(1.5);
        let b: float<128, 112> = float<128, 112>(2.25);
        let c: float<80, 64> = float<80, 64>(0.0 - 3.0);
        let d: float<80, 64> = float<80, 64>(0.5);
        print!("{} {} {} {} {}\n", real(a + b), real(a - b), real(a * b), real(b / a), real(b.sqrt()));
        print!("{} {} {} {}\n", real(c * d), real(c + d), real(-c), real(c / d));
        print!("{} {} {}\n", a < b, b < a, a == float<128, 112>(1.5));
        let third: float<128, 112> = float<128, 112>(1.0) / float<128, 112>(3.0);
        print!("{} {}\n", real(third * float<128, 112>(3.0)) == 1.0, real(third) == 1.0 / 3.0);
    }
}
"#;
