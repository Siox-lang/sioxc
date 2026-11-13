//! Regression for sioxc #564: a value parameter bound only by a function's
//! result (`fn slice_lo<W: integer>(v) -> unsigned[W]`, called as
//! `let lo: unsigned[16] = slice_lo(v)`) takes the result's width inside
//! the body, `unsigned[W](v)` included. Hardware said "no value named W"
//! and a testbench did not lower.

use std::process::Command;

fn sioxc(dir: &std::path::Path, name: &str, source: &str, args: &[&str]) -> std::process::Output {
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(args)
        .arg(&file)
        .output()
        .unwrap()
}

#[cfg(feature = "llvm")]
#[test]
fn result_bound_width_parameters_lower() {
    let dir = std::env::temp_dir().join(format!("siox_i564_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let object = dir.join("hw.o");
    let hw = sioxc(
        &dir,
        "hw",
        HARDWARE,
        &["--top", "Top", "-o", object.to_str().unwrap()],
    );
    assert!(
        hw.status.success(),
        "{}",
        String::from_utf8_lossy(&hw.stderr)
    );
    let binary = dir.join("t.bin");
    let tb = sioxc(
        &dir,
        "tb",
        TESTBENCH,
        &["--test", "-o", binary.to_str().unwrap()],
    );
    assert!(
        tb.status.success(),
        "{}",
        String::from_utf8_lossy(&tb.stderr)
    );
    let run = Command::new(binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success() && stdout.contains(" ok"), "{stdout}");
}

const HARDWARE: &str = r#"module test_generic_ret_hw;

fn slice_lo<W: integer>(v: unsigned[64]) -> unsigned[W] {
    return unsigned[W](v);
}

entity Top {
    v: unsigned[64] in,
    lo: unsigned[16] out
}

impl Top {
    lo = slice_lo(v);
}
"#;

const TESTBENCH: &str = r#"module test_generic_return_width;

fn slice_lo<W: integer>(v: unsigned[64]) -> unsigned[W] {
    return unsigned[W](v);
}

#[test]
entity Tb {}
impl Tb {
    process {
        let v: unsigned[64] = 0x123456789ABCDEF0;
        let lo: unsigned[16] = slice_lo(v);
        assert!(lo == 0xDEF0);
    }
}
"#;
