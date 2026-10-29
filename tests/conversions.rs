//! Conversions (docs/proposals/conversions.md): `Default`, `From` and `Into`.

use std::process::Command;

/// Build `source` as a test binary and run it, returning everything printed.
fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_conversions_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let binary = dir.join(name);
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
            "-o",
        ])
        .arg(&binary)
        .arg(&file)
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        let ran = Command::new(&binary).output().unwrap();
        text += &String::from_utf8_lossy(&ran.stdout);
        text += &String::from_utf8_lossy(&ran.stderr);
    }
    text
}

/// `T::default()` and `T()` build the default, `new` is a type's own
/// constructor, and an undeclared `T::new()` still builds the default with
/// the `renamed_default` warning.
#[test]
fn default_replaces_new() {
    let out = run(
        "default",
        r#"module d;
enum Phase { Idle, Run, }
struct Pair { pub a: integer, pub b: integer, }
struct S { pub v: integer, }
impl S { pub fn new(x: integer) -> S { return S { .v = x }; } }
struct K { pub k: integer, }
impl Default for K { fn default() -> K { return K { .k = 7 }; } }
#[test] entity T {}
impl T {
    let l: Logic;
    process {
        let p: Phase = Phase::default();
        let q: Pair = Pair::default();
        let r: Pair = Pair();
        let s: S = S::new(42);
        let old: Pair = Pair::new();
        let k: K = K::default();
        print!("{:?} {} {} {} {} {}\n", p, q.a, r.b, s.v, old.a, l);
        assert!(p == Phase::Idle and q.a == 0 and s.v == 42 and l == 'U');
    }
}
"#,
    );
    assert!(out.contains("passed;") && !out.contains("FAILED"), "{out}");
    assert!(out.contains("Idle 0 0 42 0 'U'"), "{out}");
    assert!(
        out.contains("W-P020") && out.contains("`Pair::new()` is now `Pair::default()`"),
        "{out}"
    );
}
