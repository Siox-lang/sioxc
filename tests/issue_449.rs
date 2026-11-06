//! Regression for sioxc #449: an array or vector field of a generic struct
//! keeps its declared labels once its parameters are substituted
//! (`data: integer[5..N]`, a `T = unsigned[7..4]` field), so a valid label
//! reads and one outside them is reported.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i449_{name}_{}", std::process::id()));
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
fn an_array_field_reads_its_declared_labels() {
    runs("array", ARRAY);
}

#[cfg(feature = "llvm")]
#[test]
fn a_vector_field_reads_its_declared_labels() {
    runs("vector", VECTOR);
}

#[test]
fn a_label_outside_the_field_is_reported() {
    let (ok, text, _) = compile("outside", OUTSIDE);
    assert!(!ok, "{text}");
    assert!(text.contains("index 0 is outside `5..6`"), "{text}");
}

const ARRAY: &str = r#"module test_generic_struct_bounds;

struct Buffer<N: integer> {
    pub data: integer[5..N],
}

#[test]
entity Top {}
impl Top {
    check: process {
        let b: Buffer<6> = Buffer { .data = [10, 20] };
        // Index 5 is the first valid element, but compiler rejects it:
        print!("b.data[5] = {}\n", b.data[5]);
        assert!(b.data[5] == 10);
    }
}
"#;

const VECTOR: &str = r#"module test_generic_packed_bounds;

struct Reg<T> {
    pub bits: T,
}

#[test]
entity Top {}
impl Top {
    check: process {
        let r: Reg<unsigned[7..4]> = Reg { .bits = 0 };
        // Index 7 is the MSB of unsigned[7..4], but compiler rejects it:
        print!("r.bits[7] = {}\n", r.bits[7]);
    }
}
"#;

const OUTSIDE: &str = r#"module test_generic_struct_bounds;

struct Buffer<N: integer> {
    pub data: integer[5..N],
}

#[test]
entity Top {}
impl Top {
    check: process {
        let b: Buffer<6> = Buffer { .data = [10, 20] };
        // Index 5 is the first valid element, but compiler rejects it:
        print!("{}", b.data[0]);
        assert!(b.data[5] == 10);
    }
}
"#;
