//! Regression for sioxc #737 (part): a string literal passed through a
//! function's `string` parameter and returned (`echo("hello")`) is the
//! fixed `Char[5]` it is, converted when stored into a `string` local.
//! Typed as the callee's unsized result, it was stored as a runtime handle
//! and the test did not lower.

use std::process::Command;

fn runs(name: &str, source: &str, expect: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i737_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
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
    assert!(
        build.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains(expect),
        "{name}: {stdout}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_literal_through_a_string_call_is_stored() {
    runs("literal", LITERAL, "e=hello");
    runs("variable", VARIABLE, "b=hello c=hello");
}

const LITERAL: &str = r#"module s3;
fn echo(s: string) -> string {
    return s;
}
#[test] entity T {}
impl T {
    process {
        let e: string = echo("hello");
        print!("e={}\n", e);
    }
}
"#;

const VARIABLE: &str = r#"module s1;
fn echo(s: string) -> string {
    return s;
}
#[test] entity T {}
impl T {
    process {
        let orig: string = "hello";
        print!("a={}\n", orig);
        let e: string = echo(orig);
        print!("b={} c={}\n", e, orig);
    }
}
"#;
