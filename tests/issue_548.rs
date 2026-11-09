//! Regression for sioxc #548: `#[derive(Eq)]` on a newtype over an array
//! derives element by element when the element is named through a path
//! (`std::logic::Bit[8]`) or is an enum of the module (`State[4]`); both
//! were taken for packed families and the derive did not check.

use std::process::Command;

fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_i548_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
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
        "{name}: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout).to_string();
    assert!(run.status.success(), "{name}: {stdout}");
    stdout
}

#[cfg(feature = "llvm")]
#[test]
fn a_qualified_element_derives_element_wise() {
    assert!(run("qualified", QUALIFIED).contains("done"));
}

#[cfg(feature = "llvm")]
#[test]
fn an_enum_array_derives() {
    run("enum", ENUM);
}

const QUALIFIED: &str = r#"module p548a;

enum State { Idle, Run, Done }

#[derive(Eq)]
struct MyVec(std::logic::Bit[4]);

#[derive(Eq)]
struct States(State[2]);

#[test]
entity Tb {}
impl Tb {
    process {
        let a: MyVec = MyVec(['1', '0', '1', '1']);
        let b: MyVec = MyVec(['1', '0', '1', '1']);
        let c: MyVec = MyVec(['0', '0', '1', '1']);
        assert!(a == b and not (a == c), "bits");
        print!("done\n");
    }
}
"#;

const ENUM: &str = r#"module test_enum_array;

enum State { Idle, Busy }

#[derive(Eq)]
struct StateArray(State[4]);

#[test]
entity Tb {}
impl Tb {
    process {
        let s: StateArray;
    }
}
"#;
