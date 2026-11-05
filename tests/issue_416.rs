//! Regression for sioxc #416: a conditional recursive call with a vector
//! argument no longer leaves an unterminated `pv.effect.join` block (an
//! invalid LLVM module) when its guarded emission is abandoned.

use std::process::Command;

fn passes(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i416_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
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

#[cfg(feature = "llvm")]
#[test]
fn a_conditional_recursive_vector_call_builds() {
    passes("issue", ISSUE);
}

const ISSUE: &str = r#"module probe_recursive_u8;

fn count(x: unsigned[8]) -> integer {
    if x == 0 {
        return 0;
    }
    return 1 + count(x);
}

#[test]
entity Tb {}

impl Tb {
    process {
        let a: unsigned[8] = 0;
        let c: integer = count(a);
        assert!(c == 0);
    }
}
"#;
