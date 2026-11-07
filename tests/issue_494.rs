//! Regression for sioxc #494: a bit pattern spells every bit it matches,
//! so one of another width than the vector is reported; a radix digit may
//! overhang the top by less than a digit when the overhang is zero.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i494_{name}_{}", std::process::id()));
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
fn a_pattern_of_another_width_is_reported() {
    for (name, source, expected) in [
        (
            "longer",
            LONGER,
            "this pattern spells 12 bits, but the vector has 8",
        ),
        (
            "shorter",
            SHORTER,
            "this pattern spells 4 bits, but the vector has 8",
        ),
        (
            "overhang",
            OVERHANG,
            "this pattern spells 8 bits, but the vector has 6",
        ),
        (
            "overhang",
            OVERHANG,
            "this pattern spells 2 bits, but the vector has 6",
        ),
    ] {
        let (ok, text, _) = compile(name, source);
        assert!(!ok, "{name}: {text}");
        assert!(text.contains(expected), "{name}: {text}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn patterns_of_the_vector_width_match() {
    let (ok, text, binary) = compile("fits", FITS);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const LONGER: &str = r#"module test_pattern_longer;

#[test]
entity Top {}
impl Top {
    check: process {
        let v: unsigned[8] = 0xFA;
        // x"1FA" is a 12-bit pattern (value 0x1FA = 506).
        // v is an 8-bit vector with value 0xFA = 250.
        // Stage 4 typechecking passes with 0 diagnostics!
        match v {
            x"1FA" => print!("BUG: 0xFA matched 12-bit pattern x\"1FA\"!\n"),
            _ => print!("correct: did not match\n"),
        }
    }
}
"#;

const SHORTER: &str = r#"module test_pattern_shorter;

#[test]
entity Top {}
impl Top {
    check: process {
        let v: unsigned[8] = 0xFA; // binary 1111_1010
        // "1010" is a 4-bit pattern.
        // v is an 8-bit vector with 0xFA.
        // Stage 4 typechecking passes with 0 diagnostics!
        match v {
            "1010" => print!("BUG: 0xFA matched 4-bit pattern \"1010\"!\n"),
            _ => print!("correct: did not match\n"),
        }
    }
}
"#;

const OVERHANG: &str = r#"module i494_more;

#[test]
entity Top {}
impl Top {
    check: process {
        let s: unsigned[6] = 0x3A;
        match s {
            x"7A" => print!("bad\n"),
            "10" => print!("bad\n"),
            _ => {}
        }
    }
}
"#;

const FITS: &str = r#"module p494_ok;

#[test]
entity Top {}
impl Top {
    check: process {
        let v: unsigned[8] = 0xFA;
        let hits: integer = 0;
        match v {
            x"FA" => { hits = hits + 1; }
            _ => {}
        }
        match v {
            "1111_1010" => { hits = hits + 1; }
            _ => {}
        }
        match v {
            x"F?" => { hits = hits + 1; }
            _ => {}
        }
        let s: unsigned[6] = 0x3A;
        match s {
            x"3A" => { hits = hits + 1; }
            _ => {}
        }
        assert!(hits == 4, "hits={}", hits);
    }
}
"#;
