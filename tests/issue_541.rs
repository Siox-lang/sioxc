//! Regression for sioxc #541: a custom operator whose body uses `xor`
//! (itself a std custom operator) lowers in a hardware continuous
//! assignment; inside an operator body the operands are kernel words, `xor`
//! had no kernel form, and the assignment lowered to nothing.

use std::process::Command;

fn run(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i541_{name}_{}", std::process::id()));
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
    assert!(
        run.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_vector_custom_operator_lowers_in_hardware() {
    run("vector", VECTOR);
}

#[cfg(feature = "llvm")]
#[test]
fn a_bit_custom_operator_lowers_in_hardware() {
    run("bit", BIT);
}

const VECTOR: &str = r#"module c541;

use std::bits::unsigned;

impl CustomOperator<"^^", unsigned[8], unsigned[8]> for unsigned[8] {
    attr precedence = 40;
    fn apply(rhs: unsigned[8]) -> unsigned[8] {
        return self xor rhs;
    }
}

entity Foo {
    a: unsigned[8] in,
    b: unsigned[8] in,
    c: unsigned[8] out,
}

impl Foo {
    c = a ^^ b;
}

#[test]
entity Tb {}
impl Tb {
    let a: unsigned[8] = 12;
    let b: unsigned[8] = 10;
    let c: unsigned[8];
    let f: Foo = { .a = a, .b = b, .c = c };
    check: process {
        await 1ns;
        assert!(c == 6, "c={}", c);
    }
}
"#;

const BIT: &str = r#"module test_custom_hw;

impl CustomOperator<"^^", Bit, Bit> for Bit {
    attr precedence = 40;
    fn apply(rhs: Bit) -> Bit {
        return self xor rhs;
    }
}

entity Foo {
    a: Bit in,
    b: Bit in,
    c: Bit out,
}

impl Foo {
    c = a ^^ b;
}

#[test]
entity Tb {}
impl Tb {
    let a: Bit = '1';
    let b: Bit = '0';
    let c: Bit;
    let f: Foo = { .a = a, .b = b, .c = c };
    check: process {
        await 1ns;
        assert!(c == '1', "c");
    }
}
"#;
