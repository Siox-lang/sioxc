//! `std::collections::BTreeMap<K, V>` and `Vec`'s `insert`, `remove` and
//! `truncate` (`docs/proposals/alloc.md`), written in std. A map keeps its
//! keys in order, replaces a repeated key's value, and hands out copies.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_btree_map_keeps_its_keys_in_order() {
    let dir = std::env::temp_dir().join(format!("siox_btree_map_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("bm.siox");
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
    for line in [
        "v=0 1 2 3 4 len=5",
        "removed=1 len=4 v1=2",
        "truncated=2",
        "len=3 get20=22 has10=true has15=false",
        "keys=10,20,30",
        "removed=1 len=2 has10=false",
        "map keeps=2",
    ] {
        assert!(stdout.contains(line), "{stdout}");
    }
}

const TESTBENCH: &str = r#"module bm;
use std::collections::BTreeMap;
use std::vec::Vec;
#[test] entity Tb {}
impl Tb {
    process {
        let v: Vec<integer>;
        v.push(1);
        v.push(3);
        v.insert(1, 2);
        v.insert(0, 0);
        v.insert(4, 4);
        print!("v={} {} {} {} {} len={}\n", v[0], v[1], v[2], v[3], v[4], v.len());
        print!("removed={} len={} v1={}\n", v.remove(1), v.len(), v[1]);
        v.truncate(2);
        print!("truncated={}\n", v.len());
        let m: BTreeMap<integer, integer>;
        m.insert(30, 3);
        m.insert(10, 1);
        m.insert(20, 2);
        m.insert(20, 22);
        print!("len={} get20={} has10={} has15={}\n", m.len(), m.get(20), m.contains_key(10), m.contains_key(15));
        let keys: Vec<integer> = m.keys();
        print!("keys={},{},{}\n", keys[0], keys[1], keys[2]);
        print!("removed={} len={} has10={}\n", m.remove(10), m.len(), m.contains_key(10));
        keys.push(99);
        print!("map keeps={}\n", m.len());
    }
}
"#;
