//! Regression for sioxc #450: a tuple assignment that reads what it writes
//! through array elements or struct fields (`(arr[0], arr[1]) = (arr[1],
//! arr[0])`) is staged, so it swaps instead of losing a value.

use std::process::Command;

fn passes(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i450_{name}_{}", std::process::id()));
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
fn array_elements_swap() {
    passes("elements", ELEMENTS);
}

#[cfg(feature = "llvm")]
#[test]
fn struct_fields_swap() {
    passes("fields", FIELDS);
}

#[cfg(feature = "llvm")]
#[test]
fn a_disjoint_pair_still_assigns() {
    passes("disjoint", DISJOINT);
}

const ELEMENTS: &str = r#"module test_tuple_arr;

#[test]
entity Top {}
impl Top {
    check: process {
        let arr: integer[2] = [10, 20];
        (arr[0], arr[1]) = (arr[1], arr[0]);
        print!("arr[0] = {}, arr[1] = {}\n", arr[0], arr[1]);
        assert!(arr[0] == 20);
        assert!(arr[1] == 10);
    }
}
"#;

const FIELDS: &str = r#"module test_tuple_struct;

struct S {
    pub f: integer,
}

#[test]
entity Top {}
impl Top {
    check: process {
        let s: S = S { .f = 42 };
        let y: S = S { .f = 0 };
        (s.f, y) = (99, s);
        print!("s.f = {}, y.f = {}\n", s.f, y.f);
        assert!(s.f == 99);
        assert!(y.f == 42);
    }
}
"#;

const DISJOINT: &str = r#"module p450_disjoint;

#[test]
entity Top {}
impl Top {
    check: process {
        let a: integer = 1;
        let b: integer = 2;
        let arr: integer[2] = [10, 20];
        (a, b) = (arr[0], arr[1]);
        assert!(a == 10 and b == 20, "a={} b={}", a, b);
    }
}
"#;
