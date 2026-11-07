//! Regression for sioxc #493: a sized vector type argument of `Index` or
//! `IndexAssign` (`unsigned[8]`) is recorded, so a value of another type is
//! rejected on write and the read is typed; it recorded nothing and accepted
//! any value.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i493_{name}_{}", std::process::id()));
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

#[test]
fn sized_index_contracts_are_checked() {
    let (ok, text, _) = compile("write", WRITE);
    assert!(!ok, "{text}");
    assert!(
        text.contains("needs `impl IndexAssign<integer, real>"),
        "{text}"
    );
    let (ok, text, _) = compile("read", READ);
    assert!(!ok, "{text}");
    assert!(text.contains("cannot assign unsigned to real"), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn sized_index_contracts_still_run() {
    let (ok, text, binary) = compile("valid", VALID);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const WRITE: &str = r#"module test_index_assign_bypass;

use std::ops::IndexAssign;

struct MyMem {
    pub data: unsigned[8][4],
}

impl IndexAssign<integer, unsigned[8]> for MyMem {
    fn index_assign(idx: integer, val: unsigned[8]) {
        data[idx] = val;
    }
}

#[test]
entity Top {}
impl Top {
    check: process {
        let m: MyMem = { .data = [0, 0, 0, 0] };
        // val is declared unsigned[8], but 3.14 is a real float!
        // Stage 4 typechecking passes with 0 diagnostics!
        m[0] = 3.14;
        // Prints: data[0] = 31 (raw IEEE-754 float bits 0x1f!)
        print!("data[0] = {}\n", m.data[0]);
    }
}
"#;

const READ: &str = r#"module test_index_read_bypass;

use std::ops::Index;

struct MyMem {
    pub data: unsigned[8][4],
}

impl Index<integer, unsigned[8]> for MyMem {
    fn index(idx: integer) -> unsigned[8] {
        return data[idx];
    }
}

entity Top {
    outp: real out,
}
impl Top {
    let m: MyMem = { .data = [10, 20, 30, 40] };
    // m[0] is unsigned[8], but outp is real!
    // Stage 4 typechecking passes with 0 diagnostics!
    outp = m[0];
}
"#;

const VALID: &str = r#"module p493_ok;

use std::ops::Index;
use std::ops::IndexAssign;

struct MyMem {
    pub data: unsigned[8][4],
}

impl Index<integer, unsigned[8]> for MyMem {
    fn index(idx: integer) -> unsigned[8] {
        return data[idx];
    }
}

impl IndexAssign<integer, unsigned[8]> for MyMem {
    fn index_assign(idx: integer, val: unsigned[8]) {
        data[idx] = val;
    }
}

#[test]
entity Top {}
impl Top {
    check: process {
        let m: MyMem = { .data = [0, 0, 0, 0] };
        let v: unsigned[8] = 7;
        m[1] = v;
        let r: unsigned[8] = m[1];
        assert!(r == 7 and m.data[1] == 7, "index contract");
    }
}
"#;
