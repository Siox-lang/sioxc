//! Regressions for sioxc #612 and #636. A fractional literal of a suffix
//! whose impl takes an `integer` is its whole count in the base unit
//! (`1.5ns` is 1500000 fs), in hardware as in a constant; hardware read it
//! as 0. One that is no whole count (`1.5fs`) is reported (#612). And a
//! `natural`, `real` or `Bool` constant whose initializer call never folds
//! is reported at the constant, as an `integer` one already was (#636).

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i612_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
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
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text, dir.join("t.bin"))
}

#[test]
fn a_fraction_of_no_whole_count_is_reported() {
    let (ok, text, _) = compile("fraction", FRACTION);
    assert!(!ok, "{text}");
    assert!(
        text.contains("`1.5fs` is not a whole number of the unit `fs` counts in"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn whole_fractions_scale_in_hardware() {
    let (ok, text, binary) = compile("whole", WHOLE);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[test]
fn an_unfolded_scalar_constant_is_reported() {
    let dir = std::env::temp_dir().join(format!("siox_i636_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("unfolded.siox");
    std::fs::write(&file, UNFOLDED).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--emit",
            "ir",
        ])
        .arg(&file)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("the value of `X` could not be computed"),
        "{text}"
    );
    assert!(!text.contains("no value named `X`"), "{text}");
}

const FRACTION: &str = r#"module p612b;

use std::sim::time;

#[test]
entity TB {}
impl TB {
    run: process {
        let t: time = 1.5fs;
    }
}
"#;

const WHOLE: &str = r#"module p612;

use std::sim::{time, frequency};

entity Top {
    t1: time out,
}

impl Top {
    t1 = 1.5ns;
}

#[test]
entity TB {}
impl TB {
    let t1: time;
    let dut: Top = { .t1 = t1 };
    run: process {
        await 1ns;
        let t: time = 2.5ps;
        let g: frequency = 1.5MHz;
        assert!(t1 == 1500ps and t == 2500fs and g == 1500kHz, "fractions {} {}", integer(t1), integer(t));
        print!("done\n");
    }
}
"#;

const UNFOLDED: &str = r#"module test_natural;

fn loop_fn(n: natural) -> natural {
    return loop_fn(n + 1);
}

const X: natural = loop_fn(0);

entity Top {
    y: natural out,
}

impl Top {
    y = X;
}
"#;
