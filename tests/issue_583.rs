//! Regression for sioxc #583: a custom operator declared by an impl with
//! a long generic header (`impl<A: Marker, B: Marker, C: Marker, D: Marker>
//! CustomOperator<"^^", Bit, Bit> for Bit`) is in the operator table. The
//! pre-parse scan read only 20 tokens past `impl`, so `x ^^ x` was "no
//! operator `^^` is declared".

use std::process::Command;

#[test]
fn a_long_impl_header_declares_its_operator() {
    let dir = std::env::temp_dir().join(format!("siox_i583_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("op.siox");
    std::fs::write(&file, SOURCE).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--emit",
            "ast",
        ])
        .arg(&file)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{text}");
    assert!(!text.contains("no operator"), "{text}");
}

const SOURCE: &str = r#"module p583;

trait Marker {}

impl Marker for Bit {}

impl<A: Marker, B: Marker, C: Marker, D: Marker> CustomOperator<"^^", Bit, Bit> for Bit {
    attr precedence = 40;
    fn apply(rhs: Bit) -> Bit { return self; }
}

entity Dut {
    x: Bit in,
    y: Bit out,
}

impl Dut {
    y = x ^^ x;
}
"#;
