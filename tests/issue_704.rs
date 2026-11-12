//! Regression for sioxc #704: a constant-folded function whose taken branch
//! does not fold (a `let` inside it) is not folded at all. One `None` stood
//! for both "fell through" and "does not fold", so the fold went past the
//! branch to a later `return`: `compute(5)` was 999 in hardware, not 15.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_branch_that_does_not_fold_is_not_a_fall_through() {
    let dir = std::env::temp_dir().join(format!("siox_i704_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("fold.siox");
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
        run.status.success() && stdout.contains("hw=15 tb=15"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module test_eval_const_if_fallthrough;

fn compute(x: integer) -> integer {
    if x > 0 {
        let y: integer = x + 10;
        return y;
    }
    return 999;
}

entity Top {
    o: integer out,
}
impl Top {
    o = compute(5);
}

#[test]
entity Tb {}
impl Tb {
    let o: integer;
    let d: Top = { .o = o };
    process {
        await 1ns;
        print!("hw={} tb={}\n", o, compute(5));
        assert!(o == 15 and compute(5) == 15);
        print!("done\n");
    }
}
"#;
