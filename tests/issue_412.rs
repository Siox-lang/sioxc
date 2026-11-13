//! Regression for sioxc #412: a struct literal for a generic field
//! (`Outer { .inner = Packet { .data = 42 } }` of `Outer<Packet<8>>`) takes
//! the field's concrete layout; it had no packed width and aborted native
//! code generation.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn nested_generic_struct_literal_runs() {
    let dir = std::env::temp_dir().join(format!("siox_i412_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("nested.siox");
    std::fs::write(&file, SOURCE).unwrap();
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
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains("inner.data = 42"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module test_nested_generic;

struct Packet<N: integer> {
    pub data: unsigned[N],
}

struct Outer<T> {
    pub inner: T,
}

#[test]
entity Tb {}

impl Tb {
    process {
        let x: Outer<Packet<8>> = Outer {
            .inner = Packet { .data = 42 }
        };
        print!("inner.data = {}\n", x.inner.data);
        assert!(x.inner.data == 42);
    }
}
"#;
