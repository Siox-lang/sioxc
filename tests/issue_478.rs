//! Regression for sioxc #478: a `match` on an unbounded scalar (`integer`,
//! `Char`) without a catch-all arm is reported as non-exhaustive, where an
//! unmatched value used to take the last arm; with `_` it runs.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i478_{name}_{}", std::process::id()));
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
fn an_unbounded_match_without_a_catch_all_is_reported() {
    let (ok, text, _) = compile("missing", MISSING);
    assert!(!ok, "{text}");
    assert_eq!(
        text.matches("has values no arm covers").count(),
        3,
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_catch_all_arm_takes_the_rest() {
    let (ok, text, binary) = compile("catch_all", CATCH_ALL);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const MISSING: &str = r#"module test_match_unbounded;

#[test]
entity Top {}
impl Top {
    check: process {
        let x: integer = 5;
        let res: integer = match x {
            0 => 10,
            1 => 20,
        };
        // Prints: res = 20 (even though 5 != 0 and 5 != 1!)
        print!("res = {}\n", res);

        let c: Char = 'z';
        let res_c: integer = match c {
            'a' => 100,
            'b' => 200,
        };
        // Prints: res_c = 200 (even though 'z' != 'a' and 'z' != 'b'!)
        print!("res_c = {}\n", res_c);

        let only: integer = match x {
            0 => 42,
        };
        // Prints: only = 42 (unconditionally evaluates to 42 for all x!)
        print!("only = {}\n", only);
    }
}
"#;

const CATCH_ALL: &str = r#"module p478_ok;

#[test]
entity Top {}
impl Top {
    check: process {
        let x: integer = 5;
        let res: integer = match x {
            0 => 10,
            1 => 20,
            _ => 30,
        };
        assert!(res == 30, "res={}", res);
    }
}
"#;
