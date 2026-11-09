//! Regression for sioxc #532: an unsized family constructor initializing a
//! declaration of a sized format (`let f: ufixed<8, 4> = ufixed(fl);`)
//! builds that format through its `From` impl, as `ufixed<8, 4>(fl)` does,
//! instead of reinterpreting the source bits.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn unsized_constructors_convert_like_sized_ones() {
    let dir = std::env::temp_dir().join(format!("siox_i532_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("unsized.siox");
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

const SOURCE: &str = r#"module p532;

use std::float::float;
use std::fixed::ufixed;

#[test]
entity TB {}
impl TB {
    run: process {
        let fl: float<32, 23> = float<32, 23>(1.5);
        let fx_unsized: ufixed<8, 4> = ufixed(fl);
        let fx_explicit: ufixed<8, 4> = ufixed<8, 4>(fl);
        assert!(integer(fx_unsized) == integer(fx_explicit), "ufixed {} {}", integer(fx_unsized), integer(fx_explicit));
        let r: real = 2.25;
        let a: float<32, 23> = float(r);
        let b: float<32, 23> = float<32, 23>(r);
        assert!(integer(a) == integer(b), "float");
        print!("done\n");
    }
}
"#;
