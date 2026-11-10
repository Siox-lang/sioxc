//! Regression for sioxc #568: a vector constructor converts a `real` by
//! value in hardware and in a testbench alike (`unsigned[8](100.75)` is
//! 100), and `core::intrinsics::to_bits` reads a real's IEEE-754 bit
//! pattern in both. A testbench reinterpreted the bits, and std's float
//! encoder relied on that, so `float<32, 23>(1.5)` was 0 in hardware.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn real_to_vector_is_numeric_everywhere() {
    let dir = std::env::temp_dir().join(format!("siox_i568_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("real.siox");
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

const SOURCE: &str = r#"module p568;

use std::bits::{unsigned, signed};
use std::float::float;
use core::intrinsics::to_bits;

entity Dut {
    r: real in,
    u8: unsigned[8] out,
    s16: signed[16] out,
    bits: natural out,
    f: float<32, 23> out,
}

impl Dut {
    u8 = unsigned[8](r);
    s16 = signed[16](0.0 - r);
    bits = to_bits(r);
    f = float<32, 23>(1.5);
}

#[test]
entity TB {}
impl TB {
    let r: real;
    let u8: unsigned[8];
    let s16: signed[16];
    let bits: natural;
    let f: float<32, 23>;
    let dut: Dut = { .r = r, .u8 = u8, .s16 = s16, .bits = bits, .f = f };
    process {
        r = 100.75;
        await 1ns;
        let tb_u8: unsigned[8] = unsigned[8](r);
        let tb_s16: signed[16] = signed[16](0.0 - r);
        let tb_bits: natural = to_bits(r);
        let tb_f: float<32, 23> = float<32, 23>(1.5);
        assert!(u8 == 100 and tb_u8 == 100, "u8 {} {}", u8, tb_u8);
        assert!(s16 == -100 and tb_s16 == -100, "s16 {} {}", s16, tb_s16);
        assert!(bits == tb_bits and tb_bits == 0x4059300000000000, "bits {} {}", bits, tb_bits);
        assert!(f == tb_f and real(tb_f) == 1.5, "float {} {}", f, tb_f);
        print!("done\n");
    }
}
"#;
