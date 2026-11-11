//! Regressions for sioxc #621 and #625: operator gaps. A `real` or an
//! `integer` divides a `Complex` (`25.0 / z`), as it already adds,
//! subtracts and multiplies one. And `xnor`, `nand` and `nor` in a custom
//! operator's body complement within the operands' width in hardware,
//! where only `xor` lowered and the others failed (E-P017).

use std::process::Command;

fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_i621_{name}_{}", std::process::id()));
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
    let stdout = String::from_utf8_lossy(&run.stdout).to_string();
    assert!(run.status.success(), "{name}: {stdout}");
    stdout
}

#[cfg(feature = "llvm")]
#[test]
fn a_scalar_divides_a_complex() {
    assert!(run("complex", COMPLEX).contains("done"));
}

#[cfg(feature = "llvm")]
#[test]
fn complemented_logic_operators_lower_in_operator_bodies() {
    assert!(run("logic", LOGIC).contains("done"));
}

const COMPLEX: &str = r#"module p621;

use std::math::Complex;

#[test]
entity TB {}
impl TB {
    process {
        let z: Complex = { .re = 3.0, .im = 4.0 };
        let a: Complex = 25.0 / z;
        let b: Complex = 25 / z;
        assert!(a.re == 3.0 and a.im == -4.0, "real {} {}", a.re, a.im);
        assert!(b.re == 3.0 and b.im == -4.0, "integer {} {}", b.re, b.im);
        print!("done\n");
    }
}
"#;

const LOGIC: &str = r#"module p625;

use std::bits::unsigned;

impl CustomOperator<"^~", unsigned[8], unsigned[8]> for unsigned[8] {
    attr precedence = 40;
    fn apply(rhs: unsigned[8]) -> unsigned[8] {
        return self xnor rhs;
    }
}

impl CustomOperator<"~&", unsigned[8], unsigned[8]> for unsigned[8] {
    attr precedence = 40;
    fn apply(rhs: unsigned[8]) -> unsigned[8] {
        return self nand rhs;
    }
}

impl CustomOperator<"~|", unsigned[8], unsigned[8]> for unsigned[8] {
    attr precedence = 40;
    fn apply(rhs: unsigned[8]) -> unsigned[8] {
        return self nor rhs;
    }
}

entity Foo {
    a: unsigned[8] in,
    b: unsigned[8] in,
    x: unsigned[8] out,
    n: unsigned[8] out,
    o: unsigned[8] out,
    same: Bool out,
}

impl Foo {
    x = a ^~ b;
    n = a ~& b;
    o = a ~| b;
    same = (a ^~ b) == 249;
}

#[test]
entity Tb {}
impl Tb {
    let a: unsigned[8] = 12;
    let b: unsigned[8] = 10;
    let x: unsigned[8];
    let n: unsigned[8];
    let o: unsigned[8];
    let same: Bool;
    let f: Foo = { .a = a, .b = b, .x = x, .n = n, .o = o, .same = same };
    process {
        await 1ns;
        assert!(x == 249 and n == 247 and o == 241 and same, "x {} n {} o {} same {}", x, n, o, same);
        assert!((a ^~ b) == 249, "testbench");
        print!("done\n");
    }
}
"#;
