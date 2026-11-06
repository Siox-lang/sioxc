//! Regression for sioxc #453: a type width or range bound given as a named
//! constant or constant expression (`unsigned[ZERO]`, `unsigned[2 - 5]`,
//! `integer[NEG]`) is checked as a literal is.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i453_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
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
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text, dir.join("t.bin"))
}

#[test]
fn constant_widths_below_one_are_reported() {
    let (ok, text, _) = compile("bad", BAD);
    assert!(!ok, "{text}");
    assert_eq!(
        text.matches("a type width must be at least 1").count(),
        1,
        "{text}"
    );
    assert_eq!(
        text.matches("a type width cannot be negative").count(),
        2,
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn valid_constant_widths_and_ranges_build() {
    let (ok, text, binary) = compile("ok", OK);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const BAD: &str = r#"module p453;

use std::bits::unsigned;

const ZERO: integer = 0;
const NEG: integer = -5;
const FOUR: integer = 4;

#[test]
entity Tb {}
impl Tb {
    let a: unsigned[ZERO] = 1;
    let b: unsigned[2 - 5] = 1;
    let c: integer[NEG];
    let ok: unsigned[FOUR] = 9;
}
"#;

const OK: &str = r#"module p453_ok;

use std::bits::unsigned;

const FOUR: integer = 4;

#[test]
entity Tb {}
impl Tb {
    let ok: unsigned[FOUR] = 9;
    let r: integer[FOUR - 4..FOUR - 3] = [1, 2];
    check: process {
        assert!(ok == 9 and r[1] == 2, "ok={} r={}", ok, r[1]);
    }
}
"#;
