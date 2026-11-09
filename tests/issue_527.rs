//! Regression for sioxc #527: `std::math::mod` takes its remainder once, so
//! nesting it (`mod(mod(mod(mod(s, 2), 2), 2), 2)`) compiles in well under a
//! second; written four times, each call copied its operands and every level
//! cost four times the last (32 s at depth three, minutes at four).

use std::process::Command;

const SOURCE: &str = r#"module nested_mod;

use std::bits::signed;
use std::math::mod;

#[test]
entity TB {}
impl TB {
    run: process {
        let s: signed[8] = -7;
        let m: signed[8] = mod(mod(mod(mod(s, 2), 2), 2), 2);
        assert!(m == 1, "m={}", m);
    }
}
"#;

#[cfg(feature = "llvm")]
#[test]
fn nested_mod_compiles_quickly_and_correctly() {
    let dir = std::env::temp_dir().join(format!("siox_i527_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("nested.siox");
    std::fs::write(&file, SOURCE).unwrap();
    let started = std::time::Instant::now();
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
    let elapsed = started.elapsed();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(
        elapsed.as_secs() < 30,
        "compiling nested mod took {elapsed:?}"
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}
