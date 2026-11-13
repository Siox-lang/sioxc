//! Regression for sioxc #712: unwrapping a newtype over an enum
//! (`Mode(im)` of `struct InnerMode(Mode)`) is the enum's own value. In
//! hardware it was "no conversion from Mode to Mode"; in a testbench it
//! did not lower.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn newtype_over_enum_unwraps() {
    let dir = std::env::temp_dir().join(format!("siox_i712_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("unwrap.siox");
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
    assert!(run.status.success() && stdout.contains("done"), "{stdout}");
}

const SOURCE: &str = r#"module p712;

enum Mode {
    A,
    B,
}

struct InnerMode(Mode);

entity Top {
    im_in: InnerMode in,
    m_out: Mode out,
}

impl Top {
    m_out = Mode(im_in);
}

#[test]
entity Tb {}

impl Tb {
    let im_in: InnerMode = InnerMode(Mode::B);
    let m_out: Mode;
    let dut: Top = { .im_in = im_in, .m_out = m_out };

    process {
        await 1ns;
        let m: Mode = Mode(im_in);
        assert!(m_out == Mode::B and m == Mode::B);
        im_in = InnerMode(Mode::A);
        await 1ns;
        assert!(m_out == Mode::A and Mode(im_in) == Mode::A);
        print!("done\n");
    }
}
"#;
