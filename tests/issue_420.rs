//! Regression for sioxc #420: `std::fixed` arithmetic past the 64-bit
//! kernel word. `ufixed<48, 24>`'s product (96 bits), a Q32.32 product,
//! division and square root, and a 64-bit format's range all wrapped or
//! saturated to -1. They are taken over `unsigned[128]`/`signed[128]` past
//! the kernel word, and a 64-bit format's extremes cap at what an `integer`
//! word holds.

use std::process::Command;

fn prints(name: &str, source: &str, expect: &[&str]) {
    let dir = std::env::temp_dir().join(format!("siox_i420_{name}_{}", std::process::id()));
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
    for line in expect {
        assert!(
            run.status.success() && stdout.contains(line),
            "{name}: {stdout}"
        );
    }
}

#[cfg(feature = "llvm")]
#[test]
fn wide_fixed_point_arithmetic() {
    prints(
        "unsigned",
        UNSIGNED,
        &[
            "1000 * 1000 in ufixed<48, 24> = 1000000",
            "2 * 3 in ufixed<64, 32> = 6",
            "from real: 2  div48: 125",
            "sqrt(4) in ufixed<64, 32> = 2",
        ],
    );
    prints(
        "signed",
        SIGNED,
        &["mul=-250500 div=-125 ndiv=-3.5 sqrt=1.5", "narrow=-3 -0.75"],
    );
}

const UNSIGNED: &str = r#"module m;
use std::fixed::ufixed;

#[test]
entity Top {}
impl Top {
    check: process {
        let a: ufixed<48, 24> = ufixed<48, 24>(1000);
        let b: ufixed<48, 24> = ufixed<48, 24>(1000);
        let mul48: ufixed<48, 24> = a * b;
        print!("1000 * 1000 in ufixed<48, 24> = {}\n", real(mul48));


        let x: ufixed<64, 32> = ufixed<64, 32>(2);
        let y: ufixed<64, 32> = ufixed<64, 32>(3);
        let mul64: ufixed<64, 32> = x * y;
        print!("2 * 3 in ufixed<64, 32> = {}\n", real(mul64));


        let s: ufixed<64, 32> = ufixed<64, 32>(4).sqrt();
        print!("from real: {}  div48: {}\n", real(ufixed<64, 32>(2.0)), real(ufixed<48, 24>(1000) / ufixed<48, 24>(8)));
        print!("sqrt(4) in ufixed<64, 32> = {}\n", real(s));

    }
}
"#;

const SIGNED: &str = r#"module sfw;
use std::fixed::{sfixed, ufixed};
#[test]
entity Tb {}
impl Tb {
    process {
        let a: sfixed<48, 24> = sfixed<48, 24>(0.0 - 1000.0);
        let b: sfixed<48, 24> = sfixed<48, 24>(250.5);
        let c: sfixed<48, 24> = sfixed<48, 24>(0.0 - 7.0);
        print!("mul={} div={} ndiv={} sqrt={}\n", real(a * b), real(a / sfixed<48, 24>(8.0)), real(c / sfixed<48, 24>(2.0)), real(sfixed<48, 24>(2.25).sqrt()));
        let n8: sfixed<8, 4> = sfixed<8, 4>(0.0 - 1.5);
        let m8: sfixed<8, 4> = sfixed<8, 4>(2.0);
        print!("narrow={} {}\n", real(n8 * m8), real(n8 / m8));
    }
}
"#;
