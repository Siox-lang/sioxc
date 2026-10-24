//! `integer` is a mathematical value, as VHDL's: a division by zero, or a
//! `real` beyond it converted to one, is a run-time error. `real` is
//! IEEE-754, as VHDL's (`1.0 / 0.0` is infinity), and the logic vectors
//! (`unsigned`, `signed`) keep their total arithmetic.

use std::process::Command;

/// Build `source` as a test binary and run it, returning everything printed.
fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_mathematical_{}", std::process::id()));
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

fn testbench(body: &str) -> String {
    format!("module m;\n#[test] entity T {{}}\nimpl T {{ process {{ {body} }} }}\n")
}

#[test]
fn leaving_the_type_is_an_error() {
    for (name, body, message) in [
        (
            "div",
            "let z: integer = 0; let b: integer = 7 / z;",
            "division by zero",
        ),
        (
            "rem",
            "let z: integer = 0; let b: integer = 7 % z;",
            "division by zero",
        ),
        (
            "to_integer",
            "let x: real = 1000000000000.0; let n: integer = integer(x * x * x);",
            "does not fit in 64 bits",
        ),
    ] {
        let out = run(name, &testbench(body));
        assert!(
            out.contains("FAILED") && out.contains(message),
            "{name}: {out}"
        );
    }
}

#[test]
fn real_is_ieee_754() {
    let out = run(
        "ieee",
        &testbench(
            "let z: real = 0.0; let one: real = 1.0;\n\
             print!(\"{} {} {}\\n\", one / z, (0.0 - one) / z, z / z);",
        ),
    );
    assert!(
        out.contains("inf -inf NaN") && out.contains("1 passed"),
        "{out}"
    );
}

#[test]
fn hardware_arithmetic_is_checked_too() {
    let out = run(
        "hardware",
        "module m;\nentity E { a: integer in, y: integer out, }\nimpl E { y = 100 / a; }\n\
         #[test] entity T {}\nimpl T { let a: integer = 4; let y: integer; let e: E = { .a = a, .y = y };\n\
         process { await 1ns; a = 0; await 1ns; } }\n",
    );
    assert!(out.contains("division by zero"), "{out}");
}

#[test]
fn an_unselected_branch_does_not_fail_and_vectors_wrap() {
    let out = run(
        "guarded",
        &testbench(
            "let z: integer = 0; let r: real = 0.0;\n\
             let q: integer = if z != 0 { 7 / z } else { -1 };\n\
             let f: real = if r != 0.0 { 1.0 / r + 1.0 } else { 2.0 };\n\
             let v: unsigned[8] = 255; let w: unsigned[8] = v + 1;\n\
             print!(\"ok {} {} {}\\n\", q, f, w);",
        ),
    );
    assert!(
        out.contains("ok -1 2 0") && out.contains("1 passed"),
        "{out}"
    );
}
