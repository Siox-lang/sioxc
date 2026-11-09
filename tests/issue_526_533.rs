//! Regression for sioxc #526 and #533: a type's index that is not a
//! constant is checked (an unknown name, a division by a constant zero), and
//! a value-generic type is checked with its arguments (`MyVec<0>` of
//! `unsigned[N]` is a zero width), reported at the use.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i526_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
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
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text, dir.join("t.bin"))
}

#[test]
fn type_index_expressions_are_checked() {
    for (name, source, expected) in [
        ("zero", ZERO, "division by a constant zero"),
        (
            "unknown",
            UNKNOWN,
            "cannot find value `nonexistent_identifier` in this scope",
        ),
        (
            "port",
            PORT,
            "cannot find value `nonexistent_identifier` in this scope",
        ),
        ("generic", GENERIC, "a type width must be at least 1"),
    ] {
        let (ok, text, _) = compile(name, source);
        assert!(!ok, "{name}: {text}");
        assert!(text.contains(expected), "{name}: {text}");
    }
}

#[test]
fn a_valid_generic_width_is_accepted() {
    let (_, text, _) = compile("valid_generic", VALID_GENERIC);
    assert!(!text.contains("error["), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn constant_width_expressions_still_run() {
    let (ok, text, binary) = compile("valid", VALID);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const ZERO: &str = r#"module repro_divzero_tb;

#[test]
entity TB {}
impl TB {
    run: process {
        let x: unsigned[10 / 0];
    }
}
"#;

const UNKNOWN: &str = r#"module repro_unknown_tb;

#[test]
entity TB {}
impl TB {
    run: process {
        let x: unsigned[nonexistent_identifier];
    }
}
"#;

const PORT: &str = r#"module repro_hw_port;

entity Top {
    clk: Bit in,
    data: unsigned[nonexistent_identifier] in
}
"#;

const GENERIC: &str = r#"module test_generic_hw;

use std::bits::unsigned;

struct MyVec<N: integer>(unsigned[N]);

entity Top {
    p_in: MyVec<0> in,
    p_out: MyVec<0> out,
}
impl Top {
    p: process {
        p_out = p_in;
    }
}
"#;

const VALID_GENERIC: &str = r#"module p526_ok;

use std::bits::unsigned;

const W: integer = 8;
struct MyVec<N: integer>(unsigned[N]);

fn width_of<N: integer>(v: unsigned[N]) -> integer {
    return N;
}

#[test]
entity TB {}
impl TB {
    run: process {
        let x: unsigned[W * 2 / 4];
        let v: MyVec<4>;
        let u: unsigned[8] = 3;
        assert!(x'length == 4 and width_of(u) == 8, "widths");
        print!("done\n");
    }
}
"#;

const VALID: &str = r#"module p526_a;

use std::bits::unsigned;

const W: integer = 8;
struct MyVec<N: integer>(unsigned[N]);

fn width_of<N: integer>(v: unsigned[N]) -> integer {
    return N;
}

#[test]
entity TB {}
impl TB {
    run: process {
        let x: unsigned[W * 2 / 4];

        let u: unsigned[8] = 3;
        assert!(x'length == 4 and width_of(u) == 8, "widths");
        print!("done\n");
    }
}
"#;
