//! Regression for sioxc #407: an `unsigned` operand that is not a plain
//! name (`arr[0]`, `p.b`, `(x + 1)`, `f()`) widens to a wider `unsigned`
//! operand by the kernel's raw resize, as `unsigned[32](x)` does, since the
//! family has no `From` of its own.

use std::process::Command;

fn passes(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i407_{name}_{}", std::process::id()));
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
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[cfg(feature = "llvm")]
#[test]
fn an_array_element_widens() {
    passes("index", INDEX);
}

#[cfg(feature = "llvm")]
#[test]
fn a_struct_field_widens() {
    passes("field", FIELD);
}

#[cfg(feature = "llvm")]
#[test]
fn a_subexpression_and_a_call_widen() {
    passes("more", MORE);
}

const INDEX: &str = r#"module test_array_elem_widen;
use std::sim::*;

#[test]
entity Tb {}

impl Tb {
    process {
        let arr: unsigned[8][2] = [10, 20];
        let total: unsigned[32] = 0;
        let sum: unsigned[32] = total + arr[0];
        print!("sum = {}\n", sum);
        assert!(sum == 10);
    }
}
"#;

const FIELD: &str = r#"module test_struct_field_widen;
use std::sim::*;

struct P {
    pub b: unsigned[8],
}

#[test]
entity Tb {}

impl Tb {
    process {
        let p: P = P { .b = 10 };
        let total: unsigned[32] = 0;
        let sum: unsigned[32] = total + p.b;
        print!("sum = {}\n", sum);
        assert!(sum == 10);
    }
}
"#;

const MORE: &str = r#"module p407_more;

fn small() -> unsigned[8] {
    return 7;
}

#[test]
entity Tb {}

impl Tb {
    process {
        let x: unsigned[8] = 200;
        let total: unsigned[32] = 1000;
        let a: unsigned[32] = total + (x + 1);
        let b: unsigned[32] = total + small();
        assert!(a == 1201, "a={}", a);
        assert!(b == 1007, "b={}", b);
    }
}
"#;
