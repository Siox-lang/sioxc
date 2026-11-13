//! Regression for sioxc #413: a process local declared through a view over
//! a generic struct (`let s: Stream<unsigned[8]> Source;`) is laid out as
//! the struct it views; it had no layout and did not build.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn view_over_generic_struct_local_runs() {
    let dir = std::env::temp_dir().join(format!("siox_i413_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("view.siox");
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
        run.status.success() && stdout.contains("data = 42"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module test_view_generic_struct_local;

struct Stream<T> {
    pub data: T,
    pub valid: Bit,
    pub ready: Bit,
}

view Source for Stream {
    data out,
    valid out,
    ready in,
}

impl<T> Stream<T> Source {
    pub fn send(val: T) {
        self.data = val;
        self.valid = '1';
    }
}

#[test]
entity Tb {}

impl Tb {
    process {
        let s: Stream<unsigned[8]> Source;
        s.send(42);
        print!("data = {}\n", s.data);
        assert!(s.data == 42);
    }
}
"#;
