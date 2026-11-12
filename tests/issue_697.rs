//! Regression for sioxc #697: a declared attribute read names another
//! module's entity (`m1::Worker'speed`) and sizes a type
//! (`let x: unsigned[w'speed]`). A qualified object was rejected (E-P029)
//! and a read inside a type was never folded, leaving an unresolved width.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn attribute_reads_take_qualified_entities_and_size_types() {
    let dir = std::env::temp_dir().join(format!("siox_i697_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m1.siox"), M1).unwrap();
    std::fs::write(dir.join("top.siox"), TOP).unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(&dir)
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
            "top.siox",
        ])
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
    assert!(run.status.success() && stdout.contains("50 7"), "{stdout}");
}

const M1: &str = r#"module m1;

pub attr speed: integer for entity, let = 10;

pub entity Worker {}
impl Worker {
    attr speed = 50;
}
"#;

const TOP: &str = r#"module top;

use std::bits::unsigned;
use m1::{speed, Worker};

entity TopQual {
    y: integer out,
}
impl TopQual {
    y = m1::Worker'speed;
}

entity TopAttrType {
    y: unsigned[50] out,
}
impl TopAttrType {
    let w: Worker = {};
    let x: unsigned[w'speed] = 7;
    y = x;
}

#[test] entity T {}
impl T {
    let q: integer;
    let t: unsigned[50];
    let a: TopQual = { .y = q };
    let b: TopAttrType = { .y = t };
    process {
        await 1ns;
        print!("{} {}\n", q, t);
        assert!(q == 50 and t == 7);
        print!("done\n");
    }
}
"#;
