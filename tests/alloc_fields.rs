//! A struct holds a `Vec` or `String` field (`docs/proposals/alloc.md`): the
//! field is its runtime handle, and copying the struct copies the vector, so
//! structs stay values.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_struct_holds_its_own_vec_and_string() {
    let dir = std::env::temp_dir().join(format!("siox_alloc_fields_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("f.siox");
    std::fs::write(&file, TESTBENCH).unwrap();
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
    assert!(run.status.success(), "{stdout}");
    for line in ["front=5 len=2", "q=2 r=3", "r=3 s=4", "w=2 v=3", "ada 36 3"] {
        assert!(stdout.contains(line), "{stdout}");
    }
}

const TESTBENCH: &str = r#"module f;
use std::vec::Vec;
pub struct Q { pub items: Vec<integer>, pub head: integer }
impl Q {
    pub fn push_back(x: integer) { self.items.push(x); }
    pub fn front() -> integer { return self.items[self.head]; }
    pub fn len() -> integer { return self.items.len() - self.head; }
}
pub struct W { pub q: Q, pub tag: integer }
pub struct P { pub name: String, pub age: integer }
#[test] entity Tb {}
impl Tb {
    process {
        let q: Q;
        q.push_back(5);
        q.push_back(6);
        print!("front={} len={}\n", q.front(), q.len());
        let r: Q = q;
        r.push_back(7);
        print!("q={} r={}\n", q.len(), r.len());
        let s: Q;
        s = r;
        s.push_back(8);
        print!("r={} s={}\n", r.len(), s.len());
        let w: W;
        w.q = q;
        let v: W = w;
        v.q.push_back(9);
        print!("w={} v={}\n", w.q.len(), v.q.len());
        let p: P;
        p.name = "ada";
        p.age = 36;
        print!("{} {} {}\n", p.name, p.age, p.name.len());
    }
}
"#;
