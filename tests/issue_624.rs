//! Regression for sioxc #624: a generate `if` on a 64-bit constant with its
//! high bit set (`if MASK > 0` for `MASK = 0x8000_0000_0000_0000`) builds
//! the branch the value says. Elaboration read it as a negative `i64` and
//! built the other branch, and lowering, unable to fold it, wired the
//! instance elaboration never built: "`l.val` has no hardware form".

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_wide_unsigned_generate_condition_takes_its_branch() {
    let dir = std::env::temp_dir().join(format!("siox_i624_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("mask.siox");
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
    assert!(
        run.status.success() && stdout.contains("out_val=42"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module test_mask_elab;

use std::bits::unsigned;

pub const MASK: unsigned[64] = 0x8000_0000_0000_0000;

entity Leaf {
    val: unsigned[64] out,
}
impl Leaf {
    val = 42;
}

entity Top {
    out_val: unsigned[64] out,
}
impl Top {
    if MASK > 0 {
        let l: Leaf = {};
        out_val = l.val;
    } else {
        out_val = 0;
    }
}

#[test] entity T {}
impl T {
    let out_val: unsigned[64];
    let top: Top = { .out_val = out_val };
    process {
        await 1ns;
        print!("out_val={}\n", out_val);
        assert!(out_val == 42);
    }
}
"#;
