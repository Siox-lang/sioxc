//! Regression for sioxc #537: `time` and `frequency` have their
//! arithmetic in std (sum, difference, scaling, ratio, negation), as VHDL's
//! physical types do.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn time_and_frequency_arithmetic() {
    let dir = std::env::temp_dir().join(format!("siox_i537_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sim_ops.siox");
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

const SOURCE: &str = r#"module p537;

use std::sim::{time, frequency};

#[test]
entity TB {}
impl TB {
    run: process {
        let t1: time = 10ns;
        let t2: time = 20ns;
        assert!(t1 + t2 == 30ns, "add");
        assert!(t2 - t1 == 10ns, "sub");
        assert!(t1 * 3 == 30ns and 3 * t1 == 30ns, "mul");
        assert!(t2 / 2 == 10ns, "div by integer");
        assert!(t2 / t1 == 2, "ratio");
        assert!(-t1 + t2 == 10ns, "neg");
        assert!(1s == 1000ms, "seconds");
        let f1: frequency = 100MHz;
        let f2: frequency = 200MHz;
        assert!(f1 + f2 == 300MHz, "freq add");
        assert!(f2 - f1 == 100MHz, "freq sub");
        assert!(f2 / f1 == 2.0, "freq ratio");
        assert!(f1 * 2.0 == 200MHz, "freq mul");
        await t1 + t2;
        print!("done\n");
    }
}
"#;
