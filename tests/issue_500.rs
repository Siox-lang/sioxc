//! Regression for sioxc #500: an array of packed vectors keeps its declared
//! labels (`unsigned[8][10..13]`), so a partial range (`arr[..12]`,
//! `arr[11..]`) is filled from them, in a process and in hardware.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn partial_ranges_use_the_declared_labels() {
    let dir = std::env::temp_dir().join(format!("siox_i500_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("labels.siox");
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
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const SOURCE: &str = r#"module p500;

entity Top {
    arr: unsigned[8][10..13] in,
    outp: unsigned[8][3] out,
}
impl Top {
    outp = arr[..12];
}

#[test]
entity TestBench {}
impl TestBench {
    let hw_in: unsigned[8][10..13] = [10, 20, 30, 40];
    let hw_out: unsigned[8][3];
    let top: Top = { .arr = hw_in, .outp = hw_out };
    run: process {
        let arr: unsigned[8][10..13] = [10, 20, 30, 40];
        let sub1: unsigned[8][3] = arr[..12];
        let sub2: unsigned[8][3] = arr[11..];
        assert!(sub1[0] == 10 and sub1[1] == 20 and sub1[2] == 30, "sub1");
        assert!(sub2[0] == 20 and sub2[1] == 30 and sub2[2] == 40, "sub2");
        assert!(arr[11] == 20, "index");
        await 1ns;
        assert!(hw_out[0] == 10 and hw_out[2] == 30, "hardware");
    }
}
"#;
