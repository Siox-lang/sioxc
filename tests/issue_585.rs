//! Regression for sioxc #585: `exists("path")` in a signal initializer is
//! compile-time I/O, relative to the source, like a `read` initializer.
//! The checker allowed it there, and lowering reported "the initializer
//! ... is not a constant".

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn exists_folds_in_an_initializer() {
    let dir = std::env::temp_dir().join(format!("siox_i585_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.bin"), "hi").unwrap();
    let file = dir.join("exists.siox");
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

const SOURCE: &str = r#"module p585;
use std::fs::*;

entity Dut {
    here: Bool out,
    gone: Bool out,
}

impl Dut {
    let has_cfg: Bool = exists("config.bin");
    let has_other: Bool = exists("missing.bin");
    here = has_cfg;
    gone = has_other;
}

#[test]
entity TB {}
impl TB {
    let here: Bool;
    let gone: Bool;
    let dut: Dut = { .here = here, .gone = gone };
    process {
        await 1ns;
        assert!(here and not gone, "here {} gone {}", here, gone);
        print!("done\n");
    }
}
"#;
