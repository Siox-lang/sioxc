//! Regression for sioxc #564 (hardware): a generic integer that only sizes
//! a function's result (`fn slice_lo<W: integer>(v: unsigned[64]) ->
//! unsigned[W]`) takes the width its call is assigned to, so
//! `lo = slice_lo(v)` into an `unsigned[16]` binds `W = 16`. It was a free
//! name in the inlined body ("no value named `W`").

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_return_only_generic_takes_the_target_width_in_hardware() {
    let dir = std::env::temp_dir().join(format!("siox_i564_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("ret.siox");
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

const SOURCE: &str = r#"module p564;

use std::bits::unsigned;

fn slice_lo<W: integer>(v: unsigned[64]) -> unsigned[W] {
    return unsigned[W](v);
}

entity Top {
    v: unsigned[64] in,
    lo: unsigned[16] out,
    nib: unsigned[4] out,
}

impl Top {
    lo = slice_lo(v);
    nib = slice_lo(v);
}

#[test]
entity TB {}
impl TB {
    let v: unsigned[64];
    let lo: unsigned[16];
    let nib: unsigned[4];
    let dut: Top = { .v = v, .lo = lo, .nib = nib };
    process {
        v = 0x123456789ABCDEF0;
        await 1ns;
        assert!(lo == 0xDEF0 and nib == 0, "hardware {} {}", lo, nib);
        print!("done\n");
    }
}
"#;
