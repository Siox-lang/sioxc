//! Regression for sioxc #438: a conversion width given as a named constant
//! or a constant expression (`unsigned[W](100)`, `resize(v, W)`) is checked
//! as a literal width is: the value must fit, and the width be at least 1.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i438_{name}_{}", std::process::id()));
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
fn a_constant_width_checks_the_value_fits() {
    let (ok, text, _) = compile("fit", FIT);
    assert!(!ok, "{text}");
    assert!(
        text.contains("`100` does not fit in a 4-bit vector"),
        "{text}"
    );
}

#[test]
fn a_constant_width_must_be_positive() {
    for (name, source, width) in [("zero", ZERO, "0"), ("negative", NEGATIVE, "-5")] {
        let (ok, text, _) = compile(name, source);
        assert!(!ok, "{text}");
        assert!(
            text.contains(&format!("a width must be at least 1, not {width}")),
            "{text}"
        );
    }
}

#[cfg(feature = "llvm")]
#[test]
fn valid_constant_widths_still_build() {
    let (ok, text, binary) = compile("ok", OK);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const FIT: &str = r#"module m;

const W: integer = 4;

#[test]
entity Top {}
impl Top {
    check: process {
        // While literal unsigned[4](100) properly triggers:
        // error[E-P003]: `100` does not fit in a 4-bit vector
        // using named const W bypasses check_fits_width and silently truncates 100 to 4:
        let v: unsigned[W] = unsigned[W](100);
        print!("v = {}\\n", v); // prints "v = 4"
    }
}
"#;

const ZERO: &str = r#"module m;

const W: integer = 0; // or const W: integer = -5;

#[test]
entity Top {}
impl Top {
    check: process {
        let v: unsigned[16] = 0x1234;
        // Bypasses `a width must be at least 1, not 0`
        let r: unsigned[8] = resize(v, W);
        print!("r = {}\\n", r);
    }
}
"#;

const NEGATIVE: &str = r#"module p438_neg;

const W: integer = -5;

#[test]
entity Top {}
impl Top {
    check: process {
        let v: unsigned[16] = 0x1234;
        let r: unsigned[8] = unsigned[W](v);
    }
}
"#;

const OK: &str = r#"module p438_ok;

const W: integer = 4;

#[test]
entity Top {}
impl Top {
    check: process {
        let v: unsigned[W] = unsigned[W](9);
        let r: unsigned[8] = resize(v, W + 4);
        assert!(v == 9 and r == 9, "v={} r={}", v, r);
    }
}
"#;
