//! Regression for sioxc #404: a generic struct named with named generic
//! arguments (`Packet<N = 3>`, `Reg<W = 8>`) binds each by name and lowers
//! with its layout.

use std::process::Command;

fn passes(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i404_{name}_{}", std::process::id()));
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
fn a_named_length_argument_binds() {
    passes("array", ARRAY);
}

#[cfg(feature = "llvm")]
#[test]
fn a_named_width_argument_binds() {
    passes("width", WIDTH);
}

const ARRAY: &str = r#"module test_generic_array_struct;
use std::sim::*;

struct Packet<N: integer> {
    pub id: unsigned[8],
    pub payload: unsigned[8][N],
}

#[test]
entity Tb {}

impl Tb {
    process {
        // Named generic argument: builds cleanly, crashes at runtime with:
        // "direct Process IR lowering is incomplete for process 0 block 0"
        let p: Packet<N = 3> = Packet { .id = 1, .payload = [10, 20, 30] };
        print!("id = {} payload[1] = {}\n", p.id, p.payload[1]);
        assert!(p.id == 1 and p.payload[1] == 20);
    }
}
"#;

const WIDTH: &str = r#"module test_generic_vec_struct;
use std::sim::*;

struct Reg<W: integer> {
    pub val: unsigned[W],
}

#[test]
entity Tb {}

impl Tb {
    process {
        let r: Reg<W = 8> = Reg { .val = 42 };
        print!("val = {}\n", r.val);
        assert!(r.val == 42);
    }
}
"#;
