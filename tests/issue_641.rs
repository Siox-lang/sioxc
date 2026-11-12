//! Regression for sioxc #641: a `signed` shift right copies the leftmost
//! element into the vacated places, a metavalue included, as numeric_std's
//! `shift_right(signed, n)` does. std filled them arithmetically from the
//! sign's value bit, so an `'X'` sign came back a clean bit, in a testbench
//! and in hardware alike.

use std::process::Command;

fn runs(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_i641_{name}_{}", std::process::id()));
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
    let stdout = String::from_utf8_lossy(&run.stdout).to_string();
    assert!(run.status.success(), "{name}: {stdout}");
    stdout
}

#[cfg(feature = "llvm")]
#[test]
fn a_testbench_signed_shift_keeps_metavalues() {
    runs("testbench", TESTBENCH);
}

#[cfg(feature = "llvm")]
#[test]
fn a_hardware_signed_shift_sign_extends_a_metavalue() {
    runs("hardware", HARDWARE);
}

#[cfg(feature = "llvm")]
#[test]
fn wide_signed_shifts_keep_their_values() {
    let out = runs("wide", WIDE);
    for line in [
        "k0 hw -1000 -1000 tb -1000 -1000",
        "k3 hw -125 -125 tb -125 -125",
        "k63 hw -1 -1 tb -1 -1",
        "pos hw 125 125 tb 125 125",
    ] {
        assert!(out.contains(line), "{line}: {out}");
    }
}

const TESTBENCH: &str = r#"module test_meta_signed_shift;

#[test]
entity TB {}
impl TB {
    process {
        // 1. Unsigned right shift preserves 'X':
        let u: unsigned[8] = "0000X100";
        let res_u: unsigned[8] = u >> 1;
        print!("res_u[2] = {:?}\n", res_u[2]); // Prints 'X' (correct)

        // 2. Signed right shift drops metavalues:
        let s: signed[8] = "0000X100";
        let res_s: signed[8] = s >> 1;
        print!("res_s[2] = {:?}\n", res_s[2]); // Prints '0' (BUG: should be 'X')
        assert!(res_s[2] == 'X');

        // 3. Sign-extending 'X' drops metavalues:
        let s2: signed[8] = "X0000100";
        let res_s2: signed[8] = s2 >> 1;
        print!("res_s2[7] = {:?}, res_s2[6] = {:?}\n", res_s2[7], res_s2[6]); // Prints '0', '0' (BUG: should be 'X', 'X')
        assert!(res_s2[7] == 'X');
    }
}
"#;

const HARDWARE: &str = r#"module p641;

use std::bits::signed;

entity Top {
    s: signed[8] in,
    r: signed[8] out,
}

impl Top {
    r = s >> 1;
}

#[test]
entity TB {}
impl TB {
    let s: signed[8] = "X0000100";
    let r: signed[8];
    let dut: Top = { .s = s, .r = r };
    process {
        await 1ns;
        print!("r[7] = {:?}, r[6] = {:?}, r[1] = {:?}\n", r[7], r[6], r[1]);
        assert!(r[7] == 'X' and r[6] == 'X', "sign");
        print!("done\n");
    }
}
"#;

const WIDE: &str = r#"module w641;

use std::bits::signed;
use std::bits::unsigned;

entity Top {
    s: signed[64] in,
    t: signed[40] in,
    k: unsigned[7] in,
    a: signed[64] out,
    b: signed[40] out,
}

impl Top {
    a = s >> k;
    b = t >> k;
}

#[test]
entity TB {}
impl TB {
    let s: signed[64] = 0 - 1000;
    let t: signed[40] = 0 - 1000;
    let k: unsigned[7] = 0;
    let a: signed[64];
    let b: signed[40];
    let dut: Top = { .s = s, .t = t, .k = k, .a = a, .b = b };
    process {
        await 1ns;
        print!("k0 hw {} {} tb {} {}\n", a, b, s >> k, t >> k);
        k = 3;
        await 1ns;
        print!("k3 hw {} {} tb {} {}\n", a, b, s >> k, t >> k);
        k = 63;
        await 1ns;
        print!("k63 hw {} {} tb {} {}\n", a, b, s >> k, t >> k);
        s = 1000; t = 1000; k = 3;
        await 1ns;
        print!("pos hw {} {} tb {} {}\n", a, b, s >> k, t >> k);
    }
}
"#;
