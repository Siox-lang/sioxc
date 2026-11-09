//! Regression for sioxc #582: `frequency` scales by a scalar from either
//! side and by an integer, as `time` does: `2.0 * 100MHz`, `100MHz * 2`,
//! `2 * 100MHz` and `400MHz / 2` were rejected. Kernel arithmetic keeps its
//! built-in operators. (Hardware stores of `frequency` arithmetic: #593.)

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn frequency_scales_from_either_side() {
    let dir = std::env::temp_dir().join(format!("siox_i582_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("scale.siox");
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

const SOURCE: &str = r#"module p582;

use std::sim::frequency;

#[test]
entity TB {}
impl TB {
    process {
        let f1: frequency = 100MHz * 2.0;
        let f2: frequency = 2.0 * 100MHz;
        let f3: frequency = 100MHz * 2;
        let f4: frequency = 2 * 100MHz;
        let f5: frequency = 400MHz / 2;
        assert!(f1 == 200MHz and f2 == 200MHz and f3 == 200MHz and f4 == 200MHz and f5 == 200MHz, "scaling");
        let n: integer = 3 * 4;
        let r: real = 2.0 * 1.5;
        assert!(n == 12 and r == 3.0, "kernel");
        print!("done\n");
    }
}
"#;
