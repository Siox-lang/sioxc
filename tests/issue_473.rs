//! Regression for sioxc #473: `after` on a variable of the process or
//! function body is reported, as it has no update to delay; on a signal it
//! still schedules the update.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i473_{name}_{}", std::process::id()));
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
fn after_on_a_local_is_reported() {
    let (ok, text, _) = compile("local", LOCAL);
    assert!(!ok, "{text}");
    assert!(text.contains("`x` is a variable of this body"), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn after_on_a_signal_still_schedules() {
    let (ok, text, binary) = compile("signal", SIGNAL);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const LOCAL: &str = r#"module test_local_after;

#[test]
entity TestLocalAfter {}
impl TestLocalAfter {
    check: process {
        let x: integer = 0;
        // Assigning to a process local variable with 'after':
        x = 50 after 1ns;
        await 2ns;
    }
}
"#;

const SIGNAL: &str = r#"module p473_ok;

#[test]
entity Tb {}
impl Tb {
    let s: integer = 0;
    check: process {
        s = 50 after 1ns;
        await 2ns;
        assert!(s == 50, "s={}", s);
    }
}
"#;
