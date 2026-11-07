//! Regression for sioxc #458: `x op= e` is parsed as `x = x op e`, so a
//! call inside the target ran twice, doubling its side effects and possibly
//! reading and writing different places. Such a target is reported; a bound
//! index, constant indices and an explicit `a[f()] = a[f()] + 1` still run.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i458_{name}_{}", std::process::id()));
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
fn a_call_in_a_compound_target_is_reported() {
    for (name, source) in [("random", RANDOM), ("counter", COUNTER)] {
        let (ok, text, _) = compile(name, source);
        assert!(!ok, "{name}: {text}");
        assert!(
            text.contains("calls a function in a compound assignment's target"),
            "{name}: {text}"
        );
    }
}

#[cfg(feature = "llvm")]
#[test]
fn compound_assignments_without_calls_still_run() {
    let (ok, text, binary) = compile("valid", VALID);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const RANDOM: &str = r#"module test_compound_assign_mismatch;

fn get_idx() -> integer {
    let r: integer = randint(0, 3);
    print!("get_idx() = {}\n", r);
    return r;
}

#[test]
entity TestCompoundAssignMismatch {}
impl TestCompoundAssignMismatch {
    check: process {
        let arr: integer[4] = [10, 20, 30, 40];

        // arr initially is [10, 20, 30, 40]
        arr[get_idx()] += 100;
        print!("arr = [{}, {}, {}, {}]\n", arr[0], arr[1], arr[2], arr[3]);
    }
}
"#;

const COUNTER: &str = r#"module test_compound_assign_counter;

fn next_idx(c: integer) -> integer {
    print!("next_idx evaluated (c = {})\n", c);
    return c;
}

#[test]
entity TestCompoundAssignCounter {}
impl TestCompoundAssignCounter {
    check: process {
        let arr: integer[4] = [10, 20, 30, 40];
        arr[next_idx(1)] += 5;
    }
}
"#;

const VALID: &str = r#"module p458_ok;

fn next_idx(c: integer) -> integer {
    return c;
}

#[test]
entity Top {}
impl Top {
    check: process {
        let arr: integer[4] = [10, 20, 30, 40];
        let i: integer = next_idx(1);
        arr[i] += 5;
        arr[2] *= 2;
        arr[next_idx(3)] = arr[next_idx(3)] + 1;
        assert!(arr[1] == 25 and arr[2] == 60 and arr[3] == 41, "compound");
    }
}
"#;
