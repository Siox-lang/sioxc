//! Regression for sioxc #386: a type-generic function's body is checked
//! only at its calls, and what that check recorded (an impl attribute read,
//! a tuple match's types) was discarded, so the body did not lower. It is
//! kept; a read two calls expand differently (`x'length` with `x: T`) is
//! left to lowering, which gives each call its own.

use std::process::Command;

fn run(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i386t_{name}_{}", std::process::id()));
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
    if name == "tables" {
        assert!(stdout.contains("bits=12 d=4"), "{stdout}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn generic_bodies_keep_their_attribute_reads_and_tuple_matches() {
    run("tables", TABLES);
}

#[cfg(feature = "llvm")]
#[test]
fn a_parameter_dependent_read_differs_per_call() {
    run("widths", WIDTHS);
}

const TABLES: &str = r#"module t386_2;
use std::sim::*;

struct Pair {
    pub a: unsigned[4],
    pub b: unsigned[8],
}

impl Pair {
    pub attr total_bits: integer = 12;
}

fn read_attr_in_generic<T>(dummy: T, p: Pair) -> integer {
    return p'total_bits;
}

fn decode_pair_in_generic<T>(dummy: T, sel: (Bit, Bit)) -> unsigned[8] {
    let m: unsigned[8] = 99;
    match sel {
        ('0', '0') => { m = 1; }
        ('0', '1') => { m = 2; }
        ('1', '0') => { m = 3; }
        ('1', '1') => { m = 4; }
    }
    return m;
}

#[test]
entity Tb {}

impl Tb {
    process {
        let p: Pair = { .a = 3, .b = 10 };
        let bits: integer = read_attr_in_generic(unsigned[8](0), p);
        let d: unsigned[8] = decode_pair_in_generic(unsigned[8](0), ('1', '1'));
        print!("bits={} d={}", bits, d);
        stop();
    }
}
"#;

const WIDTHS: &str = r#"module g386f;

fn width<T>(x: T) -> integer {
    return x'length;
}

#[test]
entity Tb {}
impl Tb {
    process {
        let a: unsigned[8] = 1;
        let b: unsigned[16] = 2;
        assert!(width(a) == 8 and width(b) == 16, "{} {}", width(a), width(b));
        stop();
    }
}
"#;
