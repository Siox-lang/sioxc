//! Regression for sioxc #492: a static function of a value-generic impl
//! called through its applied type (`Config<100>::get_limit()`) sees the
//! impl's value parameter (`N`) bound to the argument, in hardware and in a
//! testbench; `N` named nothing in scope.

use std::process::Command;

fn run(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i492_{name}_{}", std::process::id()));
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
    assert!(run.status.success(), "{name}: {stdout}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_qualifier_binds_value_parameters_in_hardware() {
    run("hardware", HARDWARE);
}

#[cfg(feature = "llvm")]
#[test]
fn a_qualifier_binds_value_parameters_in_a_testbench() {
    run("testbench", TESTBENCH);
}

const HARDWARE: &str = r#"module p492_hw;

struct Config<N: integer> {
    pub max_val: integer,
}

impl<N: integer> Config<N> {
    pub static fn get_limit() -> integer {
        return N;
    }
}

entity Top {
    outp: integer out,
}

impl Top {
    outp = Config<100>::get_limit();
}

#[test]
entity Tb {}
impl Tb {
    let o: integer;
    let top: Top = { .outp = o };
    check: process {
        await 1ns;
        assert!(o == 100, "o={}", o);
    }
}
"#;

const TESTBENCH: &str = r#"module test_struct_param_static_tb;

struct Config<N: integer> {
    pub max_val: integer,
}

impl<N: integer> Config<N> {
    pub static fn get_limit() -> integer {
        return N;
    }
}

#[test]
entity Top {}
impl Top {
    check: process {
        let x: integer = Config<100>::get_limit();
        assert!(x == 100, "x={}", x);
    }
}
"#;
