//! Regression for sioxc #405: `Bit` (and `Logic`, `ULogic`, `Bool`) has no
//! ordering, so `#[derive(Ord)]` over one is reported at the derive; it
//! generated an impl comparing with an operator `Bit` does not have.
//! `#[derive(Eq)]` on the same newtype compares element by element.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i405_{name}_{}", std::process::id()));
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
fn ord_over_an_unordered_element_is_reported() {
    let (ok, text, _) = compile("ord", ORD);
    assert!(!ok, "{text}");
    assert!(
        text.contains("cannot derive `Ord` for `Byte`: `Bit` has no ordering"),
        "{text}"
    );
    assert!(
        text.contains("cannot derive `Ord` for `Pair`: `Logic` has no ordering"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn eq_over_bits_still_runs() {
    let (ok, text, binary) = compile("eq", EQ);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const ORD: &str = r#"module i405_ord;

#[derive(Eq, Ord)]
struct Byte(Bit[8]);

struct Flags { pub ready: Bit, pub count: integer }

#[derive(Ord)]
struct Pair { pub a: integer, pub flag: Logic }
"#;

const EQ: &str = r#"module p405;

#[derive(Eq)]
struct Byte(Bit[8]);

#[test]
entity TB {}
impl TB {
    run: process {
        let a: Byte = Byte(['1', '0', '1', '1', '0', '0', '0', '1']);
        let b: Byte = Byte(['1', '0', '1', '1', '0', '0', '0', '1']);
        assert!(a == b, "eq");
        print!("done\n");
    }
}
"#;
