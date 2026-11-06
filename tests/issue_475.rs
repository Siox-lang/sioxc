//! Regression for sioxc #475: an enum discriminant literal no 64-bit value
//! holds (`0x1_0000_0000_0000_0000`) is reported, while `u64::MAX` is
//! accepted.

use std::process::Command;

#[test]
fn a_discriminant_literal_past_64_bits_is_reported() {
    let dir = std::env::temp_dir().join(format!("siox_i475_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("huge.siox");
    std::fs::write(&file, SOURCE).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--emit",
            "ir",
        ])
        .arg(&file)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "{text}");
    assert_eq!(
        text.matches("past the 64 bits a discriminant holds")
            .count(),
        1,
        "{text}"
    );
    assert!(text.contains("`Huge::Past`"), "{text}");
}

const SOURCE: &str = r#"module p475;

enum Huge {
    Zero = 0,
    Past = 0x1_0000_0000_0000_0000,
}

enum Fine {
    Zero = 0,
    Top = 0xFFFF_FFFF_FFFF_FFFF,
}
"#;
