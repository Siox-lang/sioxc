//! Regressions for sioxc #409 (a `match` on `unsigned[127]` panicked
//! computing its domain, then missed its top value) and #410 (literals from
//! `2^63` to `u64::MAX` skipped every width check).

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i409_{name}_{}", std::process::id()));
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

#[cfg(feature = "llvm")]
#[test]
fn a_match_on_a_127_bit_vector_checks_and_runs() {
    let (ok, text, binary) = compile("match127", MATCH127);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[test]
fn a_literal_past_i64_is_width_checked() {
    let (ok, text, _) = compile("wide", WIDE);
    assert!(!ok, "{text}");
    assert_eq!(
        text.matches("`9223372036854775808` does not fit in a 8-bit vector")
            .count(),
        2,
        "{text}"
    );
    assert!(!text.contains("18446744073709551615"), "{text}");
}

const MATCH127: &str = r#"module p409;

#[test]
entity Tb {}

impl Tb {
    process {
        let v: unsigned[127] = 3;
        let r: integer = match v {
            0 => 0,
            3 => 1,
            _ => 2,
        };
        assert!(r == 1, "r={}", r);
    }
}
"#;

const WIDE: &str = r#"module p410;

#[test]
entity Tb {}

impl Tb {
    process {
        let s: unsigned[8] = 0;
        let x: unsigned[8] = 0x8000_0000_0000_0000;
        assert!(s != 0x8000_0000_0000_0000, "compared");
        let ok: unsigned[64] = 0xFFFF_FFFF_FFFF_FFFF;
    }
}
"#;
