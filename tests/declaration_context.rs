#![cfg(feature = "cli")]

use std::process::{Command, Output};

const SOURCE: &str = r#"module declarations;
use std::bits::unsigned;
const OFFSET: integer = 6;
fn width(n: integer) -> integer { return n + OFFSET; }
fn packed() -> unsigned[16] { return 400; }
entity Cell<W: integer> { q: unsigned[W] out }
impl<W: integer> Cell<W> { q = 5; }
entity Outer<OFFSET: integer> { q: unsigned[8] out }
impl<OFFSET: integer> Outer<OFFSET> {
    let inner: Cell<W = width(2)> = { .q = q };
}
#[test] entity Top {}
impl Top {
    let outer: Outer<OFFSET = 90> = {};
    await 1ns;
    assert!(outer.q == 5);
    assert!(outer.q'length == 8);
    let narrowed: unsigned[8] = unsigned[8](packed());
    assert!(narrowed == 144, "packed results keep explicit narrowing semantics");
}
"#;

fn compiler(file: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(args)
        .arg(file)
        .output()
        .unwrap()
}

#[test]
fn resolved_function_parameters_agree_in_tree_and_native_output() {
    let dir = std::env::temp_dir().join(format!("siox_declaration_tree_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("declarations.siox");
    std::fs::write(&file, SOURCE).unwrap();
    let tree = compiler(&file, &["--emit", "tree"]);
    let diagnostics = String::from_utf8_lossy(&tree.stderr);
    assert!(tree.status.success(), "{diagnostics}");
    let tree = String::from_utf8_lossy(&tree.stdout);
    assert!(tree.contains("outer: Outer<OFFSET=90>"), "{tree}");
    assert!(tree.contains("inner: Cell<W=8>"), "{tree}");

    let binary = dir.join("native");
    let built = compiler(&file, &["--test", "--out", binary.to_str().unwrap()]);
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let ran = Command::new(&binary)
        .args(["-o"])
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
fn resolved_function_width_mismatch_is_rejected_before_ir() {
    let dir =
        std::env::temp_dir().join(format!("siox_declaration_mismatch_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("declarations.siox");
    std::fs::write(&file, SOURCE.replace("q: unsigned[8]", "q: unsigned[9]")).unwrap();
    let checked = compiler(&file, &["--emit", "tree"]);
    let diagnostics = String::from_utf8_lossy(&checked.stderr);
    assert!(
        !checked.status.success(),
        "a known mismatch passed elaboration"
    );
    assert!(diagnostics.contains("error[E-P003]"), "{diagnostics}");
    assert!(diagnostics.contains("width mismatch"), "{diagnostics}");
    std::fs::remove_dir_all(dir).unwrap();
}
