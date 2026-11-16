//! Proof of concept for `docs/proposals/vec.md`: `std::vec::Vec<T>` names
//! the simulator's growable store, its methods are written in siox, and a
//! hardware `Vec` is reported where it is declared.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_vec_grows_and_shrinks_in_a_testbench() {
    let dir = std::env::temp_dir().join(format!("siox_vec_poc_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("vec.siox");
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
    // `0..4` is inclusive: five elements.
    for line in [
        "len=5 last=40",
        "pop=40 len=4 last=30",
        "sum=60",
        "empty=true",
    ] {
        assert!(stdout.contains(line), "{stdout}");
    }
}

#[test]
fn a_vec_has_no_hardware_form() {
    let path = std::env::temp_dir().join("siox_vec_poc_hw.siox");
    let text = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, HARDWARE),
            Emit::Metadata,
        ))
        .render_diagnostics();
    assert!(
        text.contains("a `Vec` grows at run time, so it has no hardware form"),
        "{text}"
    );
}

const TESTBENCH: &str = r#"module v5;
use std::vec::Vec;
#[test] entity Tb {}
impl Tb {
    process {
        let v: Vec<integer>;
        for i in 0..4 {
            v.push(i * 10);
        }
        print!("len={} last={}\n", v.len(), v.last());
        let top: integer = v.pop();
        print!("pop={} len={} last={}\n", top, v.len(), v.last());
        let sum: integer = 0;
        for x in v {
            sum = sum + x;
        }
        print!("sum={}\n", sum);
        v.clear();
        print!("empty={}\n", v.is_empty());
    }
}
"#;

const HARDWARE: &str = r#"module vh;
use std::vec::Vec;
entity Top { o: integer out }
impl Top {
    let v: Vec<integer>;
    o = v.len();
}
"#;
