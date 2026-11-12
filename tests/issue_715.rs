//! Regression for sioxc #715: a constant whose value never folds is
//! reported at its declaration (E-P021), once, whatever its type, wherever
//! the call sits in its value, and for an impl's constant too. These were
//! dropped in silence and each read reported "no value named".

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn unfolded_constants_are_reported_where_they_are_declared() {
    let dir = std::env::temp_dir().join(format!("siox_i715_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("unfolded.siox");
    std::fs::write(&file, SOURCE).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .arg(&file)
        .arg("-o")
        .arg(dir.join("t.o"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{text}");
    for name in ["X_EXPR", "X_U8", "X_IMPL"] {
        assert!(
            text.contains(&format!("the value of `{name}` could not be computed")),
            "{name}: {text}"
        );
    }
    assert!(!text.contains("no value named"), "{text}");
}

const SOURCE: &str = r#"module repro;

use std::bits::unsigned;

fn loop_int(n: integer) -> integer {
    return loop_int(n + 1);
}

fn loop_u8(n: unsigned[8]) -> unsigned[8] {
    return loop_u8(n);
}

const X_EXPR: integer = loop_int(0) + 1;
const X_U8: unsigned[8] = loop_u8(0);

entity Top {
    y1: integer out,
    y2: integer out,
    y3: unsigned[8] out,
}

impl Top {
    const X_IMPL: integer = loop_int(0);

    y1 = X_EXPR;
    y2 = X_IMPL;
    y3 = X_U8;
}
"#;
