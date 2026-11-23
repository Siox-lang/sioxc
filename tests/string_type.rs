//! `std::string::String`, growable text (`docs/proposals/alloc.md`, step 1):
//! a `Char[N]` assigns into it, never the reverse, and it has no hardware
//! form.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_string_takes_fixed_text_in_a_testbench() {
    let dir = std::env::temp_dir().join(format!("siox_string_type_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("s.siox");
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
        "s=hello len=5 empty=false c=e",
        "t=hi there len=8",
        "e=[] empty=true",
        "g=abc",
        "eq=true ne=true",
    ] {
        assert!(stdout.contains(line), "{stdout}");
    }
}

fn diagnostics(name: &str, source: &str) -> String {
    let path = std::env::temp_dir().join(name);
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, source),
            Emit::Metadata,
        ))
        .render_diagnostics()
}

#[test]
fn a_string_never_becomes_fixed_text() {
    let text = diagnostics("siox_string_type_back.siox", BACK);
    assert!(
        text.contains("cannot initialize Char[3] with String"),
        "{text}"
    );
}

#[test]
fn a_string_has_no_hardware_form() {
    let text = diagnostics("siox_string_type_hw.siox", HARDWARE);
    assert!(
        text.contains("a `String` grows at run time, so it has no hardware form"),
        "{text}"
    );
}

const TESTBENCH: &str = r#"module s2;
fn same(s: String) -> String {
    return s;
}
#[test] entity Tb {}
impl Tb {
    process {
        let s: String = "hello";
        print!("s={} len={} empty={} c={}\n", s, s.len(), s.is_empty(), s[1]);
        let t: String = same("hi there");
        print!("t={} len={}\n", t, t.len());
        let e: String;
        print!("e=[{}] empty={}\n", e, e.is_empty());
        let fixed: Char[3] = "abc";
        let g: String = fixed;
        print!("g={}\n", g);
        let h: String = "hello";
        print!("eq={} ne={}\n", s == h, s != t);
    }
}
"#;

const BACK: &str = r#"module s3;
#[test] entity Tb {}
impl Tb {
    process {
        let s: String = "abc";
        let c: Char[3] = s;
    }
}
"#;

const HARDWARE: &str = r#"module sh;
entity Top { o: integer out }
impl Top {
    let s: String;
    o = s.len();
}
"#;
