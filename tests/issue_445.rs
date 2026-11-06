//! Regression for sioxc #445: a range literal where a `Range` struct is
//! expected (an `IndexAssign<Range, _>` index, a `Range` parameter and
//! local, a hardware `Range` signal) is the `Range { left, right }` value.

use std::process::Command;

fn compile(name: &str, source: &str, test: bool) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i445_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    command.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")]);
    if test {
        command.arg("--test").arg("-o").arg(dir.join("t.bin"));
    } else {
        command.args(["--emit", "ir"]);
    }
    let out = command.arg(&file).output().unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text, dir.join("t.bin"))
}

fn runs(name: &str, source: &str) {
    let (ok, text, binary) = compile(name, source, true);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[cfg(feature = "llvm")]
#[test]
fn an_index_assign_takes_a_range() {
    runs("index_assign", INDEX_ASSIGN);
}

#[cfg(feature = "llvm")]
#[test]
fn a_range_parameter_and_local_take_a_range() {
    runs("parameter", PARAMETER);
}

#[test]
fn a_hardware_range_signal_takes_a_range() {
    let (ok, text, _) = compile("hardware", HARDWARE, false);
    assert!(ok, "{text}");
    assert!(
        text.contains("Top.r.left") && text.contains("Top.r.right"),
        "{text}"
    );
}

const INDEX_ASSIGN: &str = r#"module test_index_assign_range;

use std::ops::{Index, IndexAssign, Range};

struct RegFile {
    pub r0: integer,
    pub r1: integer,
}

impl Index<Range, integer[2]> for RegFile {
    fn index(index: Range) -> integer[2] {
        return [self.r0, self.r1];
    }
}

impl IndexAssign<Range, integer[2]> for RegFile {
    fn index_assign(index: Range, value: integer[2]) {
        self.r0 = value[0];
        self.r1 = value[1];
    }
}

#[test]
entity Top {}
impl Top {
    check: process {
        let rf: RegFile = RegFile { .r0 = 10, .r1 = 20 };
        rf[0..1] = [100, 200];
        print!("rf.r0 = {}, rf.r1 = {}\n", rf.r0, rf.r1);
        assert!(rf.r0 == 100);
        assert!(rf.r1 == 200);
    }
}
"#;

const PARAMETER: &str = r#"module test_range_func;

use std::ops::Range;

fn check_range(r: Range) -> integer {
    return r.right - r.left;
}

#[test]
entity Top {}
impl Top {
    check: process {
        // Case A: Function argument
        let diff: integer = check_range(10..25);
        print!("diff = {}\n", diff);
        assert!(diff == 15);

        // Case B: Local variable binding
        let r: Range = 10..25;
        print!("r = {}..{}\n", r.left, r.right);
    }
}
"#;

const HARDWARE: &str = r#"module test_range_hw;

use std::ops::Range;

entity Top {}
impl Top {
    let r: Range;
    update: process {
        r = 10..25;
    }
}
"#;
