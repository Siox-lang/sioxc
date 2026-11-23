//! `BTreeSet`, `BinaryHeap` and `LinkedList` from `std::collections`
//! (`docs/proposals/alloc.md`), written in std. A store method's result
//! declared at an open type in a generic body (`let dropped: T =
//! self.items.remove(at)`) takes the store's element.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn sets_heaps_and_lists_behave_as_rusts() {
    let dir = std::env::temp_dir().join(format!("siox_collections_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("c.siox");
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
        "new=true again=false",
        "len=3 first=1 last=9 has1=true has2=false",
        "removed=true len=2 missing=false",
        "peek=8 len=4",
        "pops=8 5 3 1 empty=true",
        "list front=1 back=3 len=3 pop=1 back=3",
    ] {
        assert!(stdout.contains(line), "{stdout}");
    }
}

const TESTBENCH: &str = r#"module c;
use std::collections::{BTreeSet, BinaryHeap, LinkedList};
#[test] entity Tb {}
impl Tb {
    process {
        let s: BTreeSet<integer>;
        let new: Bool = s.insert(5);
        let again: Bool = s.insert(5);
        print!("new={} again={}\n", new, again);
        let added: Bool = s.insert(1);
        added = s.insert(9);
        print!("len={} first={} last={} has1={} has2={}\n", s.len(), s.first(), s.last(), s.contains(1), s.contains(2));
        let removed: Bool = s.remove(5);
        let missing: Bool = s.remove(5);
        print!("removed={} len={} missing={}\n", removed, s.len(), missing);
        let h: BinaryHeap<integer>;
        h.push(3);
        h.push(8);
        h.push(1);
        h.push(5);
        print!("peek={} len={}\n", h.peek(), h.len());
        print!("pops={} {} {} {} empty={}\n", h.pop(), h.pop(), h.pop(), h.pop(), h.is_empty());
        let l: LinkedList<integer>;
        l.push_back(2);
        l.push_front(1);
        l.push_back(3);
        print!("list front={} back={} len={} pop={} back={}\n", l.front(), l.back(), l.len(), l.pop_front(), l.pop_back());
    }
}
"#;
