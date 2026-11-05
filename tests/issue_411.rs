//! Regression for sioxc #411: a local declared with a type alias to a
//! generic struct (`type BytePacket = Packet<8>;`) takes the aliased
//! instance's layout.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn an_alias_to_a_generic_struct_lowers() {
    let dir = std::env::temp_dir().join(format!("siox_i411_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("alias.siox");
    std::fs::write(&file, SOURCE).unwrap();
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
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const SOURCE: &str = r#"module test_type_alias_generic;

struct Packet<N: integer> {
    pub data: unsigned[N],
}

type BytePacket = Packet<8>;

#[test]
entity Tb {}

impl Tb {
    process {
        let p: BytePacket = Packet { .data = 42 };
        print!("data = {}\n", p.data);
        assert!(p.data == 42);
    }
}
"#;
