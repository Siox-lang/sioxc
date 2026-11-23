//! `std::collections::VecDeque<T>` (`docs/proposals/alloc.md`): a
//! double-ended queue written in std over a `Vec`, and the generic-body
//! lowering it needs: a `Vec` method called on a field (`self.buf.clear()`,
//! `self.buf.pop()`) in `impl<T> ..`.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_vec_deque_pushes_and_pops_at_both_ends() {
    let dir = std::env::temp_dir().join(format!("siox_vec_deque_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("dq.siox");
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
        "len=3 front=1 back=3 get1=2",
        "pop_front=1 len=2",
        "front=9 pop_back=3 len=2",
        "pop=9 pop=2 empty=true",
        "cleared=true",
        "field clear=0 pop=2 len=1",
    ] {
        assert!(stdout.contains(line), "{stdout}");
    }
}

const TESTBENCH: &str = r#"module dq;
use std::collections::VecDeque;
use std::vec::Vec;
pub struct D<T> { buf: Vec<T> }
impl<T> D<T> {
    pub fn add(x: T) { self.buf.push(x); }
    pub fn clr() { self.buf.clear(); }
    pub fn take() -> T { return self.buf.pop(); }
    pub fn n() -> integer { return self.buf.len(); }
}
#[test] entity Tb {}
impl Tb {
    process {
        let q: VecDeque<integer>;
        q.push_back(2);
        q.push_back(3);
        q.push_front(1);
        print!("len={} front={} back={} get1={}\n", q.len(), q.front(), q.back(), q.get(1));
        print!("pop_front={} len={}\n", q.pop_front(), q.len());
        q.push_front(9);
        print!("front={} pop_back={} len={}\n", q.front(), q.pop_back(), q.len());
        print!("pop={} pop={} empty={}\n", q.pop_front(), q.pop_front(), q.is_empty());
        q.push_back(7);
        q.clear();
        print!("cleared={}\n", q.is_empty());
        let d: D<integer>;
        d.add(1);
        d.clr();
        let cleared: integer = d.n();
        d.add(1);
        d.add(2);
        print!("field clear={} pop={} len={}\n", cleared, d.take(), d.n());
    }
}
"#;
