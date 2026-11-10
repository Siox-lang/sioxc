//! Regression for sioxc #578: a type attribute defined through itself
//! (`attr a = self'a + 1`, or `a` and `b` naming each other) is reported
//! at its declaration. Its expansion stopped at a depth limit and lowering
//! failed on the bare attribute. An attribute built from another runs.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i578_{name}_{}", std::process::id()));
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
fn a_cyclic_type_attribute_is_reported() {
    let (ok, text, _) = compile("self", SELF);
    assert!(!ok, "{text}");
    assert!(
        text.contains("type attribute `a` is defined in terms of itself"),
        "{text}"
    );
    let (ok, text, _) = compile("mutual", MUTUAL);
    assert!(!ok, "{text}");
    assert!(
        text.contains("type attribute `a` is defined in terms of itself"),
        "{text}"
    );
    assert!(
        text.contains("type attribute `b` is defined in terms of itself"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn an_attribute_built_from_another_runs() {
    let (ok, text, binary) = compile("chain", CHAIN);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const SELF: &str = r#"module m;

struct Q(integer);

impl Q {
    pub attr a: integer = self'a + 1;
}

entity Dut {
    q: Q in,
    y: integer out,
}

impl Dut {
    y = q'a;
}
"#;

const MUTUAL: &str = r#"module m;

struct Q(integer);

impl Q {
    pub attr a: integer = self'b;
    pub attr b: integer = self'a;
}

entity Dut {
    q: Q in,
    y: integer out,
}

impl Dut {
    y = q'a;
}
"#;

const CHAIN: &str = r#"module p578;

struct Q(integer);

impl Q {
    pub attr base: integer = 4;
    pub attr twice: integer = self'base * 2;
}

#[test]
entity TB {}
impl TB {
    process {
        let q: Q = Q(1);
        assert!(q'twice == 8, "twice {}", q'twice);
        print!("done\n");
    }
}
"#;
