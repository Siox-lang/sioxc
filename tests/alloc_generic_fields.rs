//! Generic structs over stores (`docs/proposals/alloc.md`): `struct Q<T> {
//! items: Vec<T>, .. }` with methods in `impl<T> Q<T>`, the shape of every
//! collection in std::alloc. A field and a method's result are typed at the
//! receiver's arguments, and `push` in a generic body finds its element type.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_generic_struct_holds_vectors_of_its_parameters() {
    let dir = std::env::temp_dir().join(format!("siox_alloc_generic_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("g.siox");
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
    for line in ["front=5 len=2 first=5", "get=40 n=2", "copy q=2 r=3"] {
        assert!(stdout.contains(line), "{stdout}");
    }
}

const TESTBENCH: &str = r#"module g;
use std::vec::Vec;
pub struct Q<T> { pub items: Vec<T>, pub head: integer }
impl<T> Q<T> {
    pub fn push_back(x: T) { self.items.push(x); }
    pub fn front() -> T { return self.items[self.head]; }
    pub fn len() -> integer { return self.items.len() - self.head; }
}
pub struct M<K, V> { pub keys: Vec<K>, pub values: Vec<V> }
impl<K, V> M<K, V> {
    pub fn insert(k: K, v: V) { self.keys.push(k); self.values.push(v); }
    pub fn get(k: K) -> V {
        for i in 0 .. self.keys.len() - 1 {
            if self.keys[i] == k { return self.values[i]; }
        }
        return self.values[0];
    }
}
#[test] entity Tb {}
impl Tb {
    process {
        let q: Q<integer>;
        q.push_back(5);
        q.push_back(6);
        print!("front={} len={} first={}\n", q.front(), q.len(), q.items[0]);
        let m: M<integer, integer>;
        m.insert(3, 30);
        m.insert(4, 40);
        print!("get={} n={}\n", m.get(4), m.keys.len());
        let r: Q<integer> = q;
        r.push_back(7);
        print!("copy q={} r={}\n", q.len(), r.len());
    }
}
"#;
