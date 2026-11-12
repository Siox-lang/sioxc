//! Regression for sioxc #638: an unsized format constructor (`ufixed(42)`)
//! of a declared format builds that format, in a testbench `let` and a
//! hardware assignment alike. Rewritten only after checking (#532), a
//! testbench built it from the unsized call's types and did not lower, and
//! hardware read the integer's bits (3 as 0.1875).

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn an_unsized_format_constructor_builds_its_format() {
    let dir = std::env::temp_dir().join(format!("siox_i638_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("format.siox");
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

const SOURCE: &str = r#"module p638;

use std::fixed::ufixed;

entity Top {
    y: ufixed<8, 4> out,
}

impl Top {
    y = ufixed(3);
}

#[test]
entity Tb {}
impl Tb {
    let y: ufixed<8, 4>;
    let dut: Top = { .y = y };
    process {
        await 1ns;
        let a: ufixed<8, 4> = ufixed(42);
        let b: ufixed<8, 4> = ufixed(3);
        assert!(real(a) == 15.9375 and real(b) == 3.0 and real(y) == 3.0, "a {} b {} y {}", a, b, y);
        print!("done\n");
    }
}
"#;
