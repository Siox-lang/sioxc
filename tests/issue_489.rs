//! Regression for sioxc #489: a constant in a struct or tuple literal is
//! checked against the field or element it initializes, as one assigned to
//! a vector directly is, in hardware, in a process and when nested.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i489_{name}_{}", std::process::id()));
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
fn literal_fields_and_elements_must_fit() {
    for (name, source, expected) in [
        ("struct", STRUCT, "`20` does not fit in a 4-bit vector"),
        ("tuple", TUPLE, "`20` does not fit in a 4-bit vector"),
        ("hardware", HARDWARE, "`20` does not fit in a 4-bit vector"),
        ("nested", NESTED, "`30` does not fit in a 4-bit vector"),
        ("nested", NESTED, "`7` does not fit in a 2-bit vector"),
    ] {
        let (ok, text, _) = compile(name, source);
        assert!(!ok, "{name}: {text}");
        assert!(text.contains(expected), "{name}: {text}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn fitting_literals_still_run() {
    let (ok, text, binary) = compile("fits", FITS);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const STRUCT: &str = r#"module test_struct_literal_overflow;

struct Point {
    pub x: unsigned[4],
    pub y: unsigned[4],
}

#[test]
entity Top {}
impl Top {
    check: process {
        let p: Point = { .x = 1, .y = 20 };
        print!("p.x = {}, p.y = {}\n", p.x, p.y);
        assert!(p.y == 20, "overflow");
    }
}
"#;

const TUPLE: &str = r#"module test_tuple_literal_overflow;

#[test]
entity Top {}
impl Top {
    check: process {
        let t: (unsigned[4], unsigned[4]) = (1, 20);
        print!("t.0 = {}, t.1 = {}\n", t.0, t.1);
        assert!(t.1 == 20, "overflow");
    }
}
"#;

const HARDWARE: &str = r#"module test_struct_literal_overflow_hw;

struct Point {
    pub x: unsigned[4],
    pub y: unsigned[4],
}

entity Top {
    p: Point out,
}

impl Top {
    p = { .x = 1, .y = 20 };
}
"#;

const NESTED: &str = r#"module i489_nested;

struct Point {
    pub x: unsigned[4],
    pub y: unsigned[4],
}

struct Line {
    pub a: Point,
    pub b: Point,
}

#[test]
entity Top {}
impl Top {
    check: process {
        let l: Line = { .a = { .x = 2, .y = 30 }, .b = { .x = 1, .y = 1 } };
        let n: (Point, unsigned[2]) = ({ .x = 1, .y = 1 }, 7);
        print!("{}\n", l.a.y);
    }
}
"#;

const FITS: &str = r#"module p489_ok;

struct Point {
    pub x: unsigned[4],
    pub y: unsigned[4],
}

struct Line {
    pub a: Point,
    pub b: Point,
}

#[test]
entity Top {}
impl Top {
    check: process {
        let p: Point = { .x = 1, .y = 15 };
        let t: (unsigned[4], integer) = (15, 300);
        let l: Line = { .a = { .x = 2, .y = 3 }, .b = p };
        assert!(p.y == 15 and t.0 == 15 and t.1 == 300 and l.a.y == 3, "fits");
    }
}
"#;
