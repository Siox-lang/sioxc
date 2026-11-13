//! Regression for sioxc #414: a generic struct literal returned from a
//! function (`return Packet { .a = 10, .b = p.b }` or `Packet { ..p, .b =
//! 30 }` from `-> Packet<8>`) is laid out as the declared result; it had
//! no layout and the test failed with "lowering is incomplete".

use std::process::Command;

fn runs(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i414_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
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
        "{name}: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains(" ok"),
        "{name}: {stdout}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn returned_generic_struct_literals_run() {
    runs("fields", FIELDS);
    runs("spread", SPREAD);
}

const FIELDS: &str = r#"module probe_spread_return;

struct Packet<N: integer> {
    pub a: unsigned[N],
    pub b: unsigned[N],
}

fn update(p: Packet<8>) -> Packet<8> {
    return Packet { .a = 10, .b = p.b };
}

#[test]
entity Tb {}

impl Tb {
    process {
        let p1: Packet<8> = Packet { .a = 10, .b = 20 };
        let p2: Packet<8> = update(p1);
        assert!(p2.a == 10 and p2.b == 20);
    }
}
"#;

const SPREAD: &str = r#"module probe_spread_update;

struct Packet<N: integer> {
    pub a: unsigned[N],
    pub b: unsigned[N],
}

fn update(p: Packet<8>) -> Packet<8> {
    return Packet { ..p, .b = 30 };
}

#[test]
entity Tb {}

impl Tb {
    process {
        let p1: Packet<8> = Packet { .a = 10, .b = 20 };
        let p2: Packet<8> = update(p1);
        assert!(p2.a == 10 and p2.b == 30);
    }
}
"#;
