//! Regression for sioxc #546: a builtin named through the std module that
//! provides it (`bits::resize`, `std::bits::resize`,
//! `std::primitive::integer`) is the builtin, in hardware and in a
//! testbench; lowering recognized only the bare name and crashed.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn qualified_builtins_lower() {
    let dir = std::env::temp_dir().join(format!("siox_i546_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("qualified.siox");
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

const SOURCE: &str = r#"module p546;

use std::bits;
use std::bits::unsigned;

entity Top {
    x: unsigned[8] in,
    y: unsigned[16] out,
}

impl Top {
    y = bits::resize(x, 16);
}

#[test]
entity Tb {}

impl Tb {
    let x: unsigned[8];
    let y: unsigned[16];
    let top: Top = { .x = x, .y = y };

    process {
        x = 42;
        await 1ns;
        assert!(y == 42, "hw resize");
        let w: unsigned[16] = std::bits::resize(x, 16);
        let n: integer = std::primitive::integer(x);
        let r: real = std::primitive::real(3);
        assert!(w == 42 and n == 42 and r == 3.0, "tb");
        print!("done\n");
    }
}
"#;
