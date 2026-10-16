//! `await 5;` was accepted as an always-true condition and waited for
//! nothing, though `if 5` is rejected: a number is neither a time (it has no
//! unit) nor a condition. And a `time` literal that overflows the native
//! femtosecond timeline outside an `await` (`let t: time = ..ns;`) built,
//! then failed when the test ran.

use std::process::Command;

fn compile(name: &str, statement: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("siox_await_forms_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    let source = format!(
        "module m;\n#[test] entity T {{}}\nimpl T {{ let clk: Bit = '0'; clk = not clk after 5ns; \
         check: process {{ let n: integer = 2; let t: time = 3ns; {statement} }} }}\n"
    );
    std::fs::write(&file, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(["--emit", "metadata"])
        .arg(&file)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text)
}

#[test]
fn await_rejects_a_bare_number() {
    for statement in ["await 5;", "await n;"] {
        let (ok, text) = compile("number", statement);
        assert!(!ok && text.contains("not a number"), "{statement}: {text}");
    }
}

#[test]
fn await_accepts_times_edges_and_conditions() {
    for statement in [
        "await 5ns;",
        "await t;",
        "await clk.rising();",
        "await clk'event;",
        "await n == 2;",
    ] {
        let (ok, text) = compile("form", statement);
        assert!(ok, "{statement}: {text}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn an_overflowing_time_literal_is_a_build_error() {
    let dir = std::env::temp_dir().join(format!("siox_await_forms_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("overflow.siox");
    std::fs::write(
        &file,
        "module m;\n#[test] entity T {}\n\
         impl T { check: process { let t: time = 18446744073709551615ns; await t; } }\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .arg("--test")
        .arg(&file)
        .arg("-o")
        .arg(dir.join("overflow"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && text.contains("exceeds the native 64-bit femtosecond timeline"),
        "{text}"
    );
}
