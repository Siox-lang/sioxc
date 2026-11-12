//! Regression for sioxc #629: a function result holding metavalues compares
//! as numeric_std says, false (`!=` true), directly as through a local. The
//! call folded its `let v = "0UX1"` to a bare string with no companion, so
//! its block did not lower; and a call operand skipped the metavalue rule.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_call_holding_metavalues_compares_false() {
    let dir = std::env::temp_dir().join(format!("siox_i629_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("calls.siox");
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
        run.status.success() && stdout.contains("false false true true false false"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module e629;

use std::bits::unsigned;

fn get_a() -> unsigned[4] {
    let v: unsigned[4] = "0UX1";
    return v;
}

fn get_c() -> unsigned[4] {
    let v: unsigned[4] = "0101";
    return v;
}

#[test] entity T {}
impl T {
    process {
        let a: unsigned[4] = "0UX1";
        let c: unsigned[4] = "0101";
        let x: unsigned[4] = get_a();
        print!("{} {} {} {} {} {}\n", get_a() == c, get_a() == a, get_a() != c, get_c() == c, x == a, get_a() == get_a());
    }
}
"#;
