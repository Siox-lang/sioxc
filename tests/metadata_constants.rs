#![cfg(feature = "cli")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use std::path::Path;
use std::process::{Command, Output};

const PROVIDER: &str = r#"module provider;
const BASE: integer = 6;
fn amount(n: integer) -> integer { return n * BASE; }
pub attr score: integer for entity, let, port = BASE + 1;
pub entity Cell { q: integer out }
impl Cell { attr score = amount(7); q = Cell'score; }
pub entity Plain { q: integer out }
impl Plain { q = Plain'score; }
"#;

const CLIENT: &str = r#"module metadata;
use provider::{Cell, Plain, score};
const BASE: integer = 90;
fn amount(n: integer) -> integer { return n * BASE; }
#[test] entity Top {}
impl Top {
    attr score = (1 + 2) * 3;
    let inherited: Cell = {};
    let defaulted: Plain = {};
    attr score for overridden = amount(2);
    let overridden: Cell = {};
    attr score for negative = -(4 + 2);
    let negative: Plain = {};
    attr score for minimum = -0x8000_0000_0000_0000;
    let minimum: Plain = {};
    await 1ns;
    assert!(Top'score == 9);
    assert!(inherited'score == 42 and inherited.q == 42);
    assert!(defaulted'score == 7 and defaulted.q == 7);
    assert!(overridden'score == 180 and overridden.q == 42);
    assert!(negative'score == -6);
    let labelled: integer[-6..-3] = [11, 22, 33, 44];
    assert!(labelled[negative'score] == 11 and labelled[-3] == 44);
    assert!(minimum'score < 0);
    let low: integer = -0x8000_0000_0000_0000;
    let high: integer = 0x7fff_ffff_ffff_ffff;
    assert!(low < 0 and -0x8000_0000_0000_0000 < 0);
    let wrapped: integer = high + 1;
    assert!(wrapped < 0, "a stored kernel result retains its own width");
}
"#;

fn compiler(file: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(args)
        .arg(file)
        .output()
        .unwrap()
}

#[test]
fn metadata_constants_agree_in_tree_and_native_output() {
    let dir = std::env::temp_dir().join(format!("siox_metadata_constants_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("provider.siox"), PROVIDER).unwrap();
    let file = dir.join("metadata.siox");
    std::fs::write(&file, CLIENT).unwrap();
    let tree = compiler(&file, &["--emit", "tree"]);
    assert!(
        tree.status.success(),
        "{}",
        String::from_utf8_lossy(&tree.stderr)
    );
    let printed = String::from_utf8_lossy(&tree.stdout);
    assert!(printed.contains("score = 180"), "{printed}");
    assert!(printed.contains("score = -6"), "{printed}");
    assert!(!printed.contains("amount("), "unfolded metadata: {printed}");
    let binary = dir.join("native");
    let built = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(SourceInput::path(&file), Emit::TestExecutable).with_output(&binary),
    );
    assert!(
        built.succeeded(),
        "{}{:?}",
        built.render_diagnostics(),
        built.failure
    );
    let ran = Command::new(&binary)
        .arg("-o")
        .arg(dir.join("out.vcd"))
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn nonconstant_and_overflowing_integer_metadata_is_rejected() {
    let dir = std::env::temp_dir().join(format!("siox_metadata_invalid_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("invalid.siox");
    for value in ["1 / 0", "0x7fff_ffff_ffff_ffff + 1", "x"] {
        let source = format!(
            "module invalid;
            attr score: integer for entity;
            entity E {{ x: integer in }}
            impl E {{ attr score = {value}; }}"
        );
        std::fs::write(&file, source).unwrap();
        let out = compiler(&file, &["--emit", "metadata"]);
        let diagnostics = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "accepted {value}");
        assert!(
            diagnostics.contains("error[E-P007]"),
            "{value}: {diagnostics}"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
