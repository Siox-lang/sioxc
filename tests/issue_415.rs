//! Regression for sioxc #415: a function's width parameter
//! (`fn width<W: integer>(x: unsigned[W])`) is bound from its argument when
//! the call inlines as a value, so `return W;` and `x[W - 1..0]` lower.

use std::process::Command;

fn passes(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i415_{name}_{}", std::process::id()));
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
fn a_width_parameter_reads_as_its_value() {
    passes("value", VALUE);
}

#[cfg(feature = "llvm")]
#[test]
fn a_width_parameter_bounds_a_slice() {
    passes("slice", SLICE);
}

const VALUE: &str = r#"module test_generic_param_value;

fn get_width<W: integer>(x: unsigned[W]) -> integer {
    return W;
}

#[test]
entity Tb {}

impl Tb {
    process {
        let a: unsigned[8] = 42;
        let w: integer = get_width(a);
        assert!(w == 8);
    }
}
"#;

const SLICE: &str = r#"module test_slice_expr;

fn extract<W: integer>(x: unsigned[W]) -> unsigned[W] {
    let res: unsigned[W] = x[W - 1..0];
    return res;
}

#[test]
entity Tb {}

impl Tb {
    process {
        let a: unsigned[4] = 10;
        let b: unsigned[4] = extract(a);
        assert!(b == 10);
    }
}
"#;
