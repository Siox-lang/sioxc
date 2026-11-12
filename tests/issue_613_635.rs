//! Regressions for sioxc #613 and #635: derives over arrays. A newtype over
//! an array of an imported or path-named enum (`Overflow[4]`) derives `Eq`
//! element by element, as a local enum's does; it fell back to a
//! conversion a plain array lacks (#613). A long array derives too: no
//! 256-element cap with that same fallback for `Eq`, and `Ord` past 32
//! elements as flat statements rather than one expression that passed the
//! parser's nesting limit at 64 (#635).

use std::process::Command;

fn compile(name: &str, source: &str, test: bool) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i613_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    command.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")]);
    if test {
        command.arg("--test");
    } else {
        command.args(["--emit", "ir"]);
    }
    let out = command
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
fn an_imported_enum_array_derives_eq() {
    let (ok, text, _) = compile("imported", IMPORTED, false);
    assert!(ok, "{text}");
}

#[test]
fn a_long_enum_array_derives_ord() {
    let (ok, text, _) = compile("ranks", RANKS, false);
    assert!(ok, "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn long_bit_arrays_compare() {
    let (ok, text, binary) = compile("long", LONG, true);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success() && stdout.contains("done"), "{stdout}");
}

const IMPORTED: &str = r#"module test_derive_hw;

use std::numeric::Overflow;

#[derive(Eq)]
struct Flags(Overflow[4]);

entity Top {
    a: Flags in,
    b: Flags in,
    eq: Bool out,
}

impl Top {
    eq = (a == b);
}
"#;

const RANKS: &str = r#"module p635b;

enum Level { Low, High }

impl Ord<Level> for Level {
    fn lt(rhs: Level) -> Bool { return self == Level::Low and rhs == Level::High; }
    fn le(rhs: Level) -> Bool { return self == rhs or (self == Level::Low and rhs == Level::High); }
}

#[derive(Eq, Ord)]
struct Ranks(Level[70]);

#[test]
entity TB {}
impl TB {
    run: process {
        let a: Ranks;
        let b: Ranks;
        assert!(a == b and a <= b and not (a < b), "equal");
        b[69] = Level::High;
        assert!(a < b and not (b < a) and not (b <= a), "ordered");
        print!("done\n");
    }
}
"#;

const LONG: &str = r#"module p635;

#[derive(Eq)]
struct Big(Bit[257]);

#[derive(Eq)]
struct Wide(Logic[40]);

#[test]
entity TB {}
impl TB {
    run: process {
        let a: Big;
        let b: Big;
        assert!(a == b, "big equal");
        b[200] = '1';
        assert!(not (a == b), "big differ");
        let w: Wide;
        let v: Wide;
        assert!(w == v, "wide equal");
        v[39] = '1';
        assert!(not (w == v), "wide differ");
        print!("done\n");
    }
}
"#;
