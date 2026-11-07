//! Regression for sioxc #497: a newtype over a scalar (`time`,
//! `struct Meter(integer)`) prints as its scalar when it has no `Display`,
//! or under `{:?}`; it aborted the testbench as unlowered.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn scalar_newtypes_print_as_their_scalar() {
    let dir = std::env::temp_dir().join(format!("siox_i497_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("newtypes.siox");
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
    assert!(run.status.success(), "{stdout}");
    assert!(
        stdout.contains("t=10000000 m=42 r=1.5 m?=42 s=7m s?=7"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module p497;

use std::sim::time;

struct Meter(integer);
struct Ratio(real);

struct Shown(integer);

impl Display for Shown {
    fn fmt(f: Formatter) {
        write!(f, "{}m", integer(self));
    }
}

#[test]
entity TestBench {}
impl TestBench {
    check: process {
        let t: time = 10ns;
        let m: Meter = Meter(42);
        let r: Ratio = Ratio(1.5);
        let s: Shown = Shown(7);
        print!("t={} m={} r={} m?={:?} s={} s?={:?}\n", t, m, r, m, s, s);
    }
}
"#;
