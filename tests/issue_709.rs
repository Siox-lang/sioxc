//! Regression for sioxc #709 (part): a metavalue vector literal returned
//! directly (`return "0UX1";`) lowers as one bound by a `let` does; it did
//! not lower. (`s.saturating_sub(0) == c`, an unsized call, still compares
//! value planes only: #629's exception.)

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn metavalue_calls_compare_as_hardware_marks_them() {
    let dir = std::env::temp_dir().join(format!("siox_i709_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("meta.siox");
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
        run.status.success() && stdout.contains("false true true"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module p709;

use std::bits::unsigned;

fn get_direct() -> unsigned[4] {
    return "0UX1";
}

fn get_clean() -> unsigned[4] {
    return "0101";
}

#[test]
entity Tb {}

impl Tb {
    process {
        let u: unsigned[4] = "0101";
        print!("{} {} {}\n",
            get_direct() == u,
            get_clean() == u,
            get_clean() == get_clean());
        assert!(not (get_direct() == u) and get_clean() == u);
    }
}
"#;
