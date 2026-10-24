//! `extern "C"` functions with no result: C `void`, called for their effect
//! from a testbench, and refused where hardware would have to schedule them.

use std::process::Command;

/// Build `source` as a test binary and run it, or return the compiler's
/// output when it does not build.
fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_foreign_void_{}", std::process::id()));
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

#[test]
fn a_void_c_function_runs_as_a_testbench_statement() {
    // Reseeding libc's generator with the same value repeats its sequence,
    // which only holds if each `srand` call ran, in order.
    let out = run(
        "srand",
        r#"module m;
extern "C" { fn srand(seed: integer); fn rand() -> integer; }
fn reseed(s: integer) { srand(s); }
#[test] entity T {}
impl T {
    process {
        srand(7);
        let a: integer = rand();
        let b: integer = rand();
        reseed(7);
        assert!(a == rand(), "reseeding repeats the sequence");
        assert!(a != b, "and it is a sequence");
        print!("reseeded\n");
    }
}
"#,
    );
    assert!(
        out.contains("reseeded") && out.contains("1 passed"),
        "{out}"
    );
}

#[test]
fn hardware_cannot_schedule_a_testbench_only_call() {
    for (name, source, message) in [
        (
            "direct",
            "module m;\nextern \"C\" { fn srand(seed: integer); }\n\
             entity E { a: integer in, }\nimpl E { process { srand(a); } }\n",
            "`srand` returns nothing, so it can only be called in a testbench",
        ),
        (
            "value",
            "module m;\nextern \"C\" { fn srand(seed: integer); }\n\
             #[test] entity T {}\nimpl T { process { let x: integer = srand(1); } }\n",
            "cannot initialize integer with no value",
        ),
        (
            "through_a_function",
            "module m;\nextern \"C\" { fn srand(seed: integer); }\n\
             fn seed(s: integer) -> integer { srand(s); return s; }\n\
             fn outer(s: integer) -> integer { return seed(s) + 1; }\n\
             entity E { a: integer in, y: integer out, }\nimpl E { y = outer(a); }\n",
            "`outer` reaches `srand`, which only a testbench can run",
        ),
        (
            "rand_through_a_function",
            "module m;\nfn pick() -> integer { return rand(); }\n\
             entity E { y: integer out, }\nimpl E { y = pick(); }\n",
            "`pick` reaches `rand`, which only a testbench can run",
        ),
    ] {
        let out = run(name, source);
        assert!(out.contains(message), "{name}: {out}");
    }
}
