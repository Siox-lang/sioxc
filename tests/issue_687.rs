//! Regression for sioxc #687: a qualified static call forwarding the
//! caller's value parameter (`Packet<N>::width()` inside
//! `impl<N: integer> Packet<N>`, `Packet<M>::width()` inside
//! `fn helper<M: integer>`) lowers in hardware. It found no `N` or `M`.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn qualified_calls_forward_value_parameters_in_hardware() {
    let dir = std::env::temp_dir().join(format!("siox_i687_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("forward.siox");
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
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains("16 12 16"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module p687;

use std::bits::unsigned;

pub struct Packet<N: integer> {
    pub data: unsigned[N],
}

impl<N: integer> Packet<N> {
    pub static fn width() -> integer {
        return N;
    }

    pub static fn double_width_qualified() -> integer {
        return Packet<N>::width() * 2;
    }
}

fn helper<M: integer>(v: unsigned[M]) -> integer {
    return Packet<M>::width();
}

entity Top {
    y: integer out,
    z: integer out,
}

impl Top {
    let v: unsigned[12] = 0;
    y = Packet<8>::double_width_qualified();
    z = helper(v);
}

#[test]
entity Tb {}

impl Tb {
    let y: integer;
    let z: integer;
    let top: Top = { .y = y, .z = z };
    process {
        await 1ns;
        let w: integer = Packet<8>::double_width_qualified();
        print!("{} {} {}\n", y, z, w);
        assert!(y == 16 and z == 12 and w == 16);
        print!("done\n");
    }
}
"#;
