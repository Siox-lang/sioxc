//! Regressions for sioxc #609 and #643: tuples in checks. A match over a
//! nested tuple (`(a, (b, c))`) is checked by its leaves, so an exhaustive
//! one passes and a missing case is named; the nested position was a value
//! only `_` covered (#609). And a position of a newtype over a tuple
//! (`pair.0`) has its element's type, so writes, methods and initializers
//! through it are checked; it typed as an error and passed (#643).

use std::process::Command;

fn compile(name: &str, source: &str, test: bool) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i609_{name}_{}", std::process::id()));
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
fn an_exhaustive_nested_tuple_match_passes() {
    let (ok, text, _) = compile("exhaustive", EXHAUSTIVE, false);
    assert!(ok, "{text}");
}

#[test]
fn a_missing_nested_tuple_case_is_named() {
    let (ok, text, _) = compile("missing", MISSING, true);
    assert!(!ok, "{text}");
    assert!(text.contains("`('1', '0', '1')` is not covered"), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_wildcarded_nested_tuple_match_runs() {
    let (ok, text, binary) = compile("wild", WILD, true);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[test]
fn tuple_newtype_positions_are_typed() {
    let (ok, text, _) = compile("positions", POSITIONS, true);
    assert!(!ok, "{text}");
    for expected in [
        "cannot assign Char[5] to integer",
        "`integer` has no method `non_existent_method`",
        "cannot initialize Other with integer",
    ] {
        assert!(text.contains(expected), "{text}");
    }
}

const EXHAUSTIVE: &str = r#"module test_nested_tuple;

entity Top {
    a: Bit in,
    b: Bit in,
    c: Bit in,
    q: Bit out
}

impl Top {
    q = match (a, (b, c)) {
        ('0', ('0', '0')) => '0',
        ('0', ('0', '1')) => '0',
        ('0', ('1', '0')) => '0',
        ('0', ('1', '1')) => '0',
        ('1', ('0', '0')) => '1',
        ('1', ('0', '1')) => '1',
        ('1', ('1', '0')) => '1',
        ('1', ('1', '1')) => '1',
    };
}
"#;

const MISSING: &str = r#"module p609b;

#[test]
entity TB {}
impl TB {
    run: process {
        let a: Bit = '1';
        let b: Bit = '0';
        let c: Bit = '1';
        let q: Bit = match (a, (b, c)) {
            ('0', _) => '0',
            ('1', ('0', '0')) => '0',
        };
    }
}
"#;

const WILD: &str = r#"module p609;

#[test]
entity TB {}
impl TB {
    run: process {
        let a: Bit = '1';
        let b: Bit = '0';
        let c: Bit = '1';
        let q: Bit = match (a, (b, c)) {
            ('0', _) => '0',
            ('1', ('0', '0')) => '0',
            ('1', ('0', '1')) => '1',
            ('1', ('1', _)) => '1',
        };
        assert!(q == '1', "q");
        print!("done\n");
    }
}
"#;

const POSITIONS: &str = r#"module repro;

pub struct Pair((integer, integer));

struct Other {
    pub a: integer,
    pub b: integer,
}

#[test]
entity TB {}
impl TB {
    run: process {
        let p: Pair = Pair((10, 20));
        // Bug 1: Incompatible assignment completely bypasses Stage 4 type check
        p.0 = "hello";

        // Bug 2: Non-existent method call completely bypasses Stage 4 type check
        p.0.non_existent_method();

        // Bug 3: Incompatible place assignment compiles and crashes runtime
        let o: Other = p.0;
    }
}
"#;
