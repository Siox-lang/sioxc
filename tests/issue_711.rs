//! Regression for sioxc #711: a newtype of a sized newtype (`struct
//! Outer(Inner)` of `struct Inner(unsigned[8])`) has its base's length and
//! labels, in the checker and in testbench lowering. It read as unsized: a
//! 3-bit literal and `o[99]` passed, `OuterWord`'s attributes did not
//! lower, and an alias of `unsigned` slipped past the unsized check.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use std::process::Command;

fn diagnostics(name: &str, source: &str) -> String {
    let path = std::env::temp_dir().join(format!("siox_i711_{name}.siox"));
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, source),
            Emit::Metadata,
        ))
        .render_diagnostics()
}

#[test]
fn chained_newtypes_are_sized() {
    let text = diagnostics("short", SHORT);
    assert!(
        text.contains("cannot initialize Outer[8] with Char[3]"),
        "{text}"
    );
    let text = diagnostics("index", INDEX);
    assert!(text.contains("bit 99 is outside `0..7`"), "{text}");
    let text = diagnostics("alias", ALIAS);
    assert!(text.contains("unconstrained vector type for `u`"), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn chained_newtypes_lower() {
    let dir = std::env::temp_dir().join(format!("siox_i711_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("ok.siox");
    std::fs::write(&file, VALID).unwrap();
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
    assert!(
        run.status.success() && stdout.contains("b='1' low=8 high=15 len=8 olen=8"),
        "{stdout}"
    );
}

const SHORT: &str = r#"module m;

use std::bits::unsigned;

struct Inner(unsigned[8]);
struct Outer(Inner);

struct Word(Logic[15..8]);
struct OuterWord(Word);

type UnsizedAlias = unsigned;

#[test]
entity Tb {}

impl Tb {
    process {
        let o_short: Outer = "101";
        await 1ns;
    }
}
"#;

const INDEX: &str = r#"module m;

use std::bits::unsigned;

struct Inner(unsigned[8]);
struct Outer(Inner);

struct Word(Logic[15..8]);
struct OuterWord(Word);

type UnsizedAlias = unsigned;

#[test]
entity Tb {}

impl Tb {
    process {
        let o: Outer;
        let b: Logic = o[99];
        await 1ns;
    }
}
"#;

const ALIAS: &str = r#"module m;

use std::bits::unsigned;

struct Inner(unsigned[8]);
struct Outer(Inner);

struct Word(Logic[15..8]);
struct OuterWord(Word);

type UnsizedAlias = unsigned;

#[test]
entity Tb {}

impl Tb {
    process {
        let u: UnsizedAlias;
        await 1ns;
    }
}
"#;

const VALID: &str = r#"module m;

use std::bits::unsigned;

struct Inner(unsigned[8]);
struct Outer(Inner);

struct Word(Logic[15..8]);
struct OuterWord(Word);

type UnsizedAlias = unsigned;

#[test]
entity Tb {}

impl Tb {
    process {
        let o: Outer = "10100101";
        let b: Logic = o[7];
        let ow: OuterWord;
        print!("b={} low={} high={} len={} olen={}\n", b, ow'low, ow'high, ow'length, o'length);
        await 1ns;
    }
}
"#;
