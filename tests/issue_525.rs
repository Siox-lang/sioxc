//! Regression for sioxc #525: std declares the seconds suffix, so `1s` is a
//! `time` (10^15 fs) and `await 1s` waits; it was a scaled integer that no
//! `time` took and `await` could not lower.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn seconds_are_a_time() {
    let dir = std::env::temp_dir().join(format!("siox_i525_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("seconds.siox");
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
        run.status.success() && stdout.contains("waited"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module p525;

#[test]
entity TB {}
impl TB {
    run: process {
        let t: time = 2s;
        assert!(t == 2000ms, "2s is 2000ms");
        await 1s;
        print!("waited\n");
    }
}
"#;
