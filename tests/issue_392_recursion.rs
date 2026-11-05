//! Regression for sioxc #392 part 4: a recursive void procedure
//! (`sum_down_proc(n - 1, acc);`) runs with a frame, and its written
//! parameter reaches the caller's local.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_recursive_procedure_writes_back_through_its_frames() {
    let dir = std::env::temp_dir().join(format!("siox_i392_rec_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("rec.siox");
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

const SOURCE: &str = r#"module t392_rec;
use std::sim::*;

fn sum_down_proc(n: integer, acc: integer) {
    if n > 0 {
        acc = acc + n;
        sum_down_proc(n - 1, acc);
    }
}

#[test]
entity TbRecursiveProc {}

impl TbRecursiveProc {
    process {
        let acc: integer = 0;
        sum_down_proc(4, acc);
        assert!(acc == 10, "expected acc == 10");
    }
}
"#;
