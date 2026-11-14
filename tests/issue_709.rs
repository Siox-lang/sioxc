//! Regression for sioxc #709: a metavalue vector literal returned directly
//! (`return "0UX1";`) lowers as one bound by a `let` does, and a call with
//! an unsized family result of known width (`s.saturating_sub(0)`) takes
//! the metavalue comparison rule (#629). The first did not lower; the
//! second compared value planes only and said `true`.

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
        run.status.success() && stdout.contains("false false false true true"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module p709;

use std::bits::unsigned;
use std::bits::signed;

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
        let s: signed[4] = "0UX1";
        let c: signed[4] = "0001";
        let t: signed[4] = "0011";
        let two: signed[4] = "0010";
        let u: unsigned[4] = "0101";
        print!("{} {} {} {} {}\n",
            s.saturating_sub(0) == c,
            s.saturating_sub(0) == s,
            get_direct() == u,
            t.saturating_sub(1) == two,
            get_clean() == u);
        assert!(not (s.saturating_sub(0) == c) and t.saturating_sub(1) == two and get_clean() == u);
    }
}
"#;
