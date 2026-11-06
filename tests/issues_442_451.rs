//! Regressions for sioxc #442 and #451: a slice is assigned from a struct
//! field, an outer array's row or a string literal, element by element; a
//! call, which a per-element read would run once per element, is reported.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i442_{name}_{}", std::process::id()));
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

fn runs(name: &str, source: &str) {
    let (ok, text, binary) = compile(name, source);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_field_and_a_row_fill_a_slice() {
    runs("places", PLACES);
}

#[cfg(feature = "llvm")]
#[test]
fn a_string_literal_fills_a_slice() {
    runs("string", STRING);
}

#[cfg(feature = "llvm")]
#[test]
fn the_issue_451_field_fills_a_slice() {
    runs("field", FIELD);
}

#[test]
fn a_call_into_a_slice_is_reported() {
    let (ok, text, _) = compile("call", CALL);
    assert!(!ok, "{text}");
    assert!(
        text.contains("a slice cannot be assigned the call `get_two()` directly"),
        "{text}"
    );
}

const PLACES: &str = r#"module p442;

struct Container {
    pub pair: integer[2],
}

#[test]
entity Top {}
impl Top {
    check: process {
        let arr: integer[4] = [1, 2, 3, 4];
        let c: Container = Container { .pair = [10, 20] };
        arr[0..1] = c.pair;
        assert!(arr[0] == 10 and arr[1] == 20, "field {} {}", arr[0], arr[1]);
        let matrix: integer[2][2] = [[30, 40], [50, 60]];
        arr[2..3] = matrix[1];
        assert!(arr[2] == 50 and arr[3] == 60, "row {} {}", arr[2], arr[3]);
    }
}
"#;

const STRING: &str = r#"module test_str_slice;

#[test]
entity Top {}
impl Top {
    check: process {
        let s: string = "hello world";
        s[0..4] = "HELLO";
        print!("s = {}\n", s);
        assert!(s == "HELLO world");
    }
}
"#;

const FIELD: &str = r#"module test_field_slice;

struct S {
    pub field: integer[2],
}

#[test]
entity Top {}
impl Top {
    check: process {
        let s: S = S { .field = [10, 20] };
        let arr: integer[4] = [1, 2, 3, 4];
        arr[0..1] = s.field;
        print!("arr[0] = {}, arr[1] = {}\n", arr[0], arr[1]);
        assert!(arr[0] == 10);
        assert!(arr[1] == 20);
    }
}
"#;

const CALL: &str = r#"module test_fn_slice;

fn get_two() -> integer[2] {
    return [10, 20];
}

#[test]
entity Top {}
impl Top {
    check: process {
        let arr: integer[4] = [1, 2, 3, 4];
        arr[0..1] = get_two();
        print!("arr[0] = {}, arr[1] = {}\n", arr[0], arr[1]);
        assert!(arr[0] == 10);
        assert!(arr[1] == 20);
    }
}
"#;
