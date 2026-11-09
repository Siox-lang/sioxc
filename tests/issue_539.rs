//! Regression for sioxc #539: a type's shape attributes (`integer'low`,
//! `natural'high`, `Byte'high` through an alias, `Logic[8]'length`,
//! `unsigned[7..0]'left`) are VHDL's type attributes, folded to constants;
//! they lowered to an invalid value. One a type lacks (`real'low`,
//! `integer'length`) is reported.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i539_{name}_{}", std::process::id()));
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
fn type_attributes_are_constants() {
    let (ok, text, binary) = compile("attrs", ATTRS);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success() && stdout.contains("done"), "{stdout}");
}

#[test]
fn an_attribute_a_type_lacks_is_reported() {
    let (ok, text, _) = compile("lacks", LACKS);
    assert!(!ok, "{text}");
    assert!(text.contains("`real'low` has no value"), "{text}");
    assert!(
        text.contains(
            "`integer'length` has no value: a type's `'length` is defined for array types"
        ),
        "{text}"
    );
}

const ATTRS: &str = r#"module p539;

use std::bits::unsigned;

type Byte = integer<0..255>;

const INT_MIN: integer = integer'low;

#[test]
entity TB {}
impl TB {
    run: process {
        assert!(INT_MIN == integer'low and integer'high == 9223372036854775807, "integer");
        assert!(integer'low + 1 < 0, "low is negative");
        assert!(Byte'low == 0 and Byte'high == 255, "ranged");
        let n: natural = natural'high;
        assert!(natural'low == natural(0) and n == natural'high, "natural");
        assert!(Logic[8]'length == 8 and Logic[8]'left == 0 and Logic[8]'high == 7, "width");
        assert!(unsigned[7..0]'left == 7 and unsigned[7..0]'right == 0, "range");
        assert!(unsigned[15..8]'low == 8 and unsigned[15..8]'length == 8, "labels");
        print!("done\n");
    }
}
"#;

const LACKS: &str = r#"module i539_bad;

#[test]
entity TB {}
impl TB {
    run: process {
        let r: integer = real'low;
        let l: integer = integer'length;
    }
}
"#;
