//! The kernel `natural`: the unsigned counterpart of `integer`, a mathematical
//! number from 0 to 2^64 - 1. Leaving that range or dividing by zero is a
//! run-time error, and it mixes with `integer` only through a conversion.

use std::process::Command;

/// Build `source` (`--test` and run it when `run`, else `--emit metadata`)
/// and return everything the compiler and the binary printed.
fn compile(name: &str, source: &str, run: bool) -> String {
    let dir = std::env::temp_dir().join(format!("siox_natural_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let binary = dir.join(name);
    let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    command.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")]);
    if run {
        command.arg("--test").arg("-o").arg(&binary);
    } else {
        command.args(["--emit", "metadata"]);
    }
    let output = command.arg(&file).output().unwrap();
    let mut text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    if run && output.status.success() {
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
fn natural_is_an_unsigned_64_bit_number() {
    let out = compile(
        "values",
        &testbench(
            "let a: natural = 18446744073709551615; let b: natural = 5;\n\
             let i: integer = -3;\n\
             print!(\"{} {} {} {}\\n\", a - b, 1 + b * 2, a / b, a % b);\n\
             print!(\"{} {} {}\\n\", integer(b) + i, natural(i + 10), natural(12.7));\n\
             assert!(a > b and b != 0 and real(a) > 18000000000000000000.0, \"compares unsigned\");",
        ),
        true,
    );
    assert!(
        out.contains("18446744073709551610 11 3689348814741910323 0")
            && out.contains("2 7 12")
            && out.contains("1 passed"),
        "{out}"
    );
}

#[test]
fn leaving_natural_is_an_error() {
    const MAX: &str = "18446744073709551615";
    for (name, body, message) in [
        (
            "add",
            format!("let a: natural = {MAX}; let b: natural = a + 1;"),
            "outside 0 to 2^64 - 1",
        ),
        (
            "sub",
            "let a: natural = 3; let b: natural = a - 5;".to_string(),
            "outside 0 to 2^64 - 1",
        ),
        (
            "mul",
            "let a: natural = 4294967296; let b: natural = a * a;".to_string(),
            "outside 0 to 2^64 - 1",
        ),
        (
            "div",
            "let z: natural = 0; let b: natural = 7 / z;".to_string(),
            "division by zero",
        ),
        (
            "from_integer",
            "let i: integer = -3; let n: natural = natural(i);".to_string(),
            "outside 0 to 2^64 - 1",
        ),
        (
            "from_real",
            "let r: real = -1.0; let n: natural = natural(r);".to_string(),
            "outside 0 to 2^64 - 1",
        ),
        (
            "to_integer",
            format!("let a: natural = {MAX}; let i: integer = integer(a);"),
            "does not fit in 64 bits",
        ),
    ] {
        let out = compile(name, &testbench(&body), true);
        assert!(
            out.contains("FAILED") && out.contains(message),
            "{name}: {out}"
        );
    }
}

#[test]
fn natural_in_hardware() {
    let out = compile(
        "hardware",
        "module m;\nentity E { a: natural in, b: natural in, i: integer in, y: natural out, n: natural out, }\n\
         impl E { y = a - b; n = natural(i); }\n\
         #[test] entity T {}\nimpl T { let a: natural = 5; let b: natural = 3; let i: integer = 4;\n\
         let y: natural; let n: natural; let e: E = { .a = a, .b = b, .i = i, .y = y, .n = n };\n\
         process { await 1ns; print!(\"hw {} {}\\n\", y, n); b = 9; await 1ns; } }\n",
        true,
    );
    assert!(
        out.contains("hw 2 4") && out.contains("outside 0 to 2^64 - 1"),
        "{out}"
    );
}

#[test]
fn natural_and_integer_mix_only_by_conversion() {
    let out = compile(
        "mixing",
        &testbench(
            "let b: natural = 5; let i: integer = 3;\n\
             let x: natural = b + i; let y: natural = -1; let z: integer = b;",
        ),
        false,
    );
    for message in [
        "`natural` and `integer` do not mix",
        "cannot initialize natural with integer",
        "cannot initialize integer with natural",
    ] {
        assert!(out.contains(message), "missing `{message}`: {out}");
    }
}
