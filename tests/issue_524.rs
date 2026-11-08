//! Regression for sioxc #524: a qualified runtime call (`rand::randint`,
//! `std::rand::uniform`, `fs::exists`) is the same testbench-only call as
//! the bare one, so hardware that reaches it is reported; a testbench may
//! still make it.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i524_{name}_{}", std::process::id()));
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
fn qualified_runtime_calls_in_hardware_are_reported() {
    let (ok, text, _) = compile("direct", DIRECT);
    assert!(!ok, "{text}");
    for name in ["randint", "uniform", "exists"] {
        assert!(
            text.contains(&format!("`{name}` is only available in a testbench")),
            "{name}: {text}"
        );
    }
    let (ok, text, _) = compile("through", THROUGH);
    assert!(!ok, "{text}");
    assert!(
        text.contains("reaches `rand`, which only a testbench can run"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_testbench_may_make_qualified_runtime_calls() {
    let (ok, text, binary) = compile("testbench", TESTBENCH);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const DIRECT: &str = r#"module test_repro;

use std::rand;
use std::fs;

entity Top {
    r1: integer out,
    r2: real out,
    ok: Bool out,
}
impl Top {
    // Calling qualified runtime intrinsics in hardware continuous assignments:
    r1 = rand::randint(1, 100);
    r2 = std::rand::uniform();
    ok = fs::exists("/tmp");
}
"#;

const THROUGH: &str = r#"module test_fn;

use std::rand;

fn random_value() -> integer {
    return rand::rand();
}

entity Top {
    outp: integer out,
}
impl Top {
    outp = random_value();
}
"#;

const TESTBENCH: &str = r#"module p524_ok;

use std::rand;
use std::fs;

#[test]
entity Tb {}
impl Tb {
    check: process {
        let r: integer = rand::randint(1, 5);
        let e: Bool = std::fs::exists("/tmp");
        assert!(r >= 1 and r <= 5, "r={}", r);
        print!("e={}\n", e);
    }
}
"#;
