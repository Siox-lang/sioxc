//! Regressions for a batch of front-end gaps:
//! - sioxc #603: a second `impl Trait for Type` (same trait arguments) is
//!   reported; each call took whichever impl lowering met last.
//! - sioxc #620: a program's own module named `fs`, `bits`, `rand` or
//!   `primitive` keeps its qualified calls (`fs::read(42)`).
//! - sioxc #623: an integer match open at both ends still has its gaps
//!   reported (`..0`, `100..` miss `1..99`).
//! - sioxc #642, #645: a string ending in an escaped quote keeps it, and a
//!   character literal takes escapes (`'\''`, `'\n'`, `'\\'`).
//! - sioxc #647: a character enum of multibyte symbols (`'←'`) is one, by
//!   characters rather than bytes.

use std::process::Command;

fn compile(
    dir_name: &str,
    files: &[(&str, &str)],
    main: &str,
) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i603_{dir_name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, source) in files {
        std::fs::write(dir.join(name), source).unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
        ])
        .arg(dir.join(main))
        .arg("-o")
        .arg(dir.join("t.bin"))
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text, dir.join("t.bin"))
}

#[cfg(feature = "llvm")]
fn runs(dir_name: &str, files: &[(&str, &str)], main: &str) -> String {
    let (ok, text, binary) = compile(dir_name, files, main);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout).to_string();
    assert!(run.status.success(), "{stdout}");
    stdout
}

#[test]
fn a_duplicate_trait_impl_is_reported() {
    let (ok, text, _) = compile("dup", &[("dup.siox", DUPLICATE)], "dup.siox");
    assert!(!ok, "{text}");
    assert!(
        text.contains("duplicate implementation of `Foo` for `MyStruct`"),
        "{text}"
    );
}

#[test]
fn an_open_integer_match_reports_its_gap() {
    let (ok, text, _) = compile("gap", &[("gap.siox", GAP)], "gap.siox");
    assert!(!ok, "{text}");
    assert!(text.contains("`1..99` is not covered"), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_covered_open_integer_match_runs() {
    assert!(runs("covered", &[("covered.siox", COVERED)], "covered.siox").contains("done"));
}

#[cfg(feature = "llvm")]
#[test]
fn an_own_module_named_fs_keeps_its_calls() {
    runs(
        "fs",
        &[("fs.siox", FS), ("main.siox", FS_MAIN)],
        "main.siox",
    );
}

#[cfg(feature = "llvm")]
#[test]
fn escapes_and_multibyte_symbols_read_whole() {
    let out = runs("escapes", &[("escapes.siox", ESCAPES)], "escapes.siox");
    assert!(out.contains("u: ←→ q: '"), "{out}");
    let quote = runs("quote", &[("quote.siox", QUOTE)], "quote.siox");
    assert!(quote.contains("s1 length: 1"), "{quote}");
}

const DUPLICATE: &str = r#"module m;

struct MyStruct {
    pub x: integer,
}

trait Foo {
    fn bar() -> integer;
}

impl Foo for MyStruct {
    fn bar() -> integer {
        return 1;
    }
}

impl Foo for MyStruct {
    fn bar() -> integer {
        return 2;
    }
}

entity Dut {
    s: MyStruct in,
    y: integer out,
}

impl Dut {
    y = s.bar();
}

#[test]
entity Tb {}
impl Tb {
    let s: MyStruct = { .x = 10 };
    let d: Dut = { .s = s };
    print!("s.bar() in tb = {}\n", s.bar());
    print!("d.y = {}\n", d.y);
    assert!(s.bar() == d.y);
}
"#;

const GAP: &str = r#"module test_match_gap;

#[test] entity T {}
impl T {
    process {
        let x: integer = 50;
        let y: integer = match x {
            ..0 => 1,
            100.. => 2,
        };
        print!("x=50 -> y={}\n", y);
        // 50 is not in `..0` and not in `100..`, yet y evaluates to 2!
        assert!(y == 2);
    }
}
"#;

const COVERED: &str = r#"module p623;

#[test]
entity TB {}
impl TB {
    run: process {
        let x: integer = 50;
        let y: integer = match x {
            ..0 => 1,
            1..99 => 3,
            100.. => 2,
        };
        assert!(y == 3, "y {}", y);
        print!("done\n");
    }
}
"#;

const FS: &str = r#"module fs;

pub fn read(addr: integer) -> integer {
    return addr + 1;
}
"#;

const FS_MAIN: &str = r#"module main;
use fs::read;

#[test] entity T {}
impl T {
    process {
        let x: integer = fs::read(42);
        assert!(x == 43);
    }
}
"#;

const ESCAPES: &str = r#"module p645;

pub enum Arrow { '←', '→' }

#[test]
entity TB {}
impl TB {
    run: process {
        let q: Char = '\'';
        let n: Char = '\n';
        let b: Char = '\\';
        let s: string = "say \"hi\"";
        assert!(q == '\'' and b == '\\', "escapes");
        assert!(s'length == 8, "string {}", s'length);
        let u: Arrow[2] = "←→";
        print!("u: {} q: {}\n", u, q);
        print!("done\n");
    }
}
"#;

const QUOTE: &str = r#"module test_str_quote;

#[test]
entity TB {}
impl TB {
    process {
        let s1: string = "\"";
        print!("s1 length: {}\n", s1'length);
        print!("s1 character: {:?}\n", s1[0]); // Prints '\\' instead of '"'
        assert!(s1[0] == '"'); // FAILS

        let s2: string = "a\"\"";
        print!("s2[0] = {:?}, s2[1] = {:?}, s2[2] = {:?}\n", s2[0], s2[1], s2[2]); // Prints 'a', '"', '\\'
        assert!(s2[2] == '"'); // FAILS
    }
}
"#;
