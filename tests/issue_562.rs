//! Regression for sioxc #562: a process local of a value-generic newtype
//! (`let v: MyVec<4>;` of `struct MyVec<N: integer>(unsigned[N])`) is laid
//! out as its base with the arguments put in; it had no layout and the
//! test did not lower.

use std::process::Command;

fn runs(name: &str, source: &str, expect: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i562_{name}_{}", std::process::id()));
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
        "{name}: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains(expect),
        "{name}: {stdout}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn value_generic_newtype_locals_run() {
    runs("unset", UNSET, "done");
    runs("assigned", ASSIGNED, "v=9");
}

const UNSET: &str = r#"module generic_newtype_local;

use std::bits::unsigned;

struct MyVec<N: integer>(unsigned[N]);

#[test]
entity TB {}
impl TB {
    run: process {
        let v: MyVec<4>;
        print!("done\n");
    }
}
"#;

const ASSIGNED: &str = r#"module p562;

use std::bits::unsigned;

struct MyVec<N: integer>(unsigned[N]);

#[test]
entity TB {}
impl TB {
    run: process {
        let v: MyVec<4>;
        let w: MyVec<4> = MyVec(9);
        v = w;
        print!("v={}\n", unsigned[4](v));
        assert!(unsigned[4](v) == 9);
    }
}
"#;
