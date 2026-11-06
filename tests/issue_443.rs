//! Regression for sioxc #443: an overlapping slice copy within one array
//! (a struct field's array, a descending range, a backward source) reads no
//! element after writing it, and a cyclic move (a reversal) that no
//! element order can keep is reported instead of corrupting the array.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i443_{name}_{}", std::process::id()));
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

#[cfg(feature = "llvm")]
#[test]
fn overlapping_copies_read_before_they_write() {
    let (ok, text, binary) = compile("overlap", OVERLAP);
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
fn a_cyclic_move_is_reported() {
    let (ok, text, _) = compile("cycle", CYCLE);
    assert!(!ok, "{text}");
    assert!(text.contains("within itself in a cycle"), "{text}");
}

const OVERLAP: &str = r#"module p443;

struct Container {
    pub arr: integer[4],
}

#[test]
entity Tb {}

impl Tb {
    check: process {
        let c: Container = Container { .arr = [1, 2, 3, 4] };
        c.arr[1..2] = c.arr[0..1];
        let desc: integer[3..0] = [10, 20, 30, 40];
        desc[1..0] = desc[2..1];
        let back: integer[4] = [10, 20, 30, 40];
        back[0..1] = back[1..2];
        assert!(c.arr[0] == 1 and c.arr[1] == 1 and c.arr[2] == 2 and c.arr[3] == 4, "field {} {} {} {}", c.arr[0], c.arr[1], c.arr[2], c.arr[3]);
        assert!(desc[3] == 10 and desc[2] == 20 and desc[1] == 20 and desc[0] == 30, "desc {} {} {} {}", desc[3], desc[2], desc[1], desc[0]);
        assert!(back[0] == 20 and back[1] == 30 and back[2] == 30 and back[3] == 40, "back {} {} {} {}", back[0], back[1], back[2], back[3]);
    }
}
"#;

const CYCLE: &str = r#"module p443_cycle;

#[test]
entity Tb {}

impl Tb {
    check: process {
        let rev: integer[4] = [10, 20, 30, 40];
        rev[1..2] = rev[2..1];
    }
}
"#;
