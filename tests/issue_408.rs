//! Regression for sioxc #408: a tuple literal holding a generic struct
//! literal (`(Node { .data = [10, 20] }, ..)` of `(Node<unsigned[8], 2>,
//! ..)`) writes the inner literal field by field, as its declared type lays
//! it out; lowered whole, `[10, 20]` had no element type and the test did
//! not lower.

use std::process::Command;

fn runs(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i408_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std"), "--test"])
        .arg(&file)
        .arg("-o")
        .arg(dir.join("t.bin"))
        .output()
        .unwrap();
    assert!(build.status.success(), "{name}: {}", String::from_utf8_lossy(&build.stderr));
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success() && stdout.contains(" ok"), "{name}: {stdout}");
}

#[cfg(feature = "llvm")]
#[test]
fn tuples_of_generic_struct_literals_run() {
    runs("pair", PAIR);
    runs("mixed", MIXED);
}

const PAIR: &str = r#"module test_tuple_generic_struct;
use std::sim::*;

struct Node<T, N: integer> {
    pub data: T[N],
}

#[test]
entity Tb {}

impl Tb {
    process {
        let p: (Node<unsigned[8], 2>, Node<unsigned[8], 2>) = (
            Node { .data = [10, 20] },
            Node { .data = [30, 40] }
        );
        assert!(p.0.data[0] == 10 and p.1.data[1] == 40);
    }
}
"#;

const MIXED: &str = r#"module v408e;
struct Node<T, N: integer> {
    pub data: T[N],
}
#[test]
entity Tb {}
impl Tb {
    process {
        let n: (Node<unsigned[8], 2>, integer) = (Node { .data = [10, 20] }, 3);
        assert!(n.0.data[1] == 20);
    }
}
"#;
