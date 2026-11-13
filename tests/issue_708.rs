//! Regression for sioxc #708 (part): a hardware signal of a newtype with
//! its own `impl Default` (`let sc: Score;`, `Score(42)`) powers on at that
//! default, as a testbench local of it does; it started at 0.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn newtype_signals_take_their_default() {
    let dir = std::env::temp_dir().join(format!("siox_i708_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("default.siox");
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
        run.status.success() && stdout.contains("hw=42 tb=42"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module p708;

struct Score(unsigned[8]);

impl Default for Score {
    static fn default() -> Score {
        return Score(42);
    }
}

entity Hw {
    score_out: Score out,
}

impl Hw {
    let sc: Score;
    score_out = sc;
}

#[test]
entity Tb {}

impl Tb {
    let hw_score: Score;
    let dut: Hw = { .score_out = hw_score };

    process {
        await 1ns;
        let tb_sc: Score;
        print!("hw={} tb={}\n", unsigned[8](hw_score), unsigned[8](tb_sc));
        assert!(unsigned[8](hw_score) == 42 and unsigned[8](tb_sc) == 42);
    }
}
"#;
