//! Regression for sioxc #459: an index into a constant array or a field of
//! a constant struct (`WIDTHS[0]`, `CFG.width`, a ranged `RANGE[2]`) is a
//! constant width, in a hardware port and a testbench local.

use std::process::Command;

fn compile(name: &str, source: &str, test: bool) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i459_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    command.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")]);
    if test {
        command.arg("--test");
    }
    let out = command
        .arg(&file)
        .arg("-o")
        .arg(dir.join("t.bin"))
        .output()
        .unwrap();
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
fn a_hardware_port_takes_a_constant_element_width() {
    let (ok, text, _) = compile("hardware", HARDWARE, false);
    assert!(ok, "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_testbench_local_takes_a_constant_element_length() {
    runs("length", LENGTH);
}

#[cfg(feature = "llvm")]
#[test]
fn a_field_and_a_ranged_element_are_widths() {
    runs("field", FIELD);
}

const HARDWARE: &str = r#"module test_const_array_width_hw;

const WIDTHS: integer[3] = [8, 16, 32];

struct Config {
    pub width: integer,
}
const CFG: Config = Config { .width = 8 };

entity Dut {
    a: unsigned[WIDTHS[0]] in,     // or unsigned[CFG.width] in
    y: unsigned[WIDTHS[0]] out,
}

impl Dut {
    y = a;
}
"#;

const LENGTH: &str = r#"module test_array_len_const_index;

const SIZES: integer[2] = [3, 5];

#[test]
entity TestArrayLenConstIndex {}
impl TestArrayLenConstIndex {
    check: process {
        let arr: integer[SIZES[0]] = [10, 20, 30];
        print!("arr length = {}\n", arr'length);
        assert!(arr'length == 3, "arr'length == 3");
    }
}
"#;

const FIELD: &str = r#"module p459_field;

struct Config {
    pub width: integer,
}
const CFG: Config = Config { .width = 6 };
const RANGE: integer[1..2] = [4, 7];

#[test]
entity Tb {}
impl Tb {
    check: process {
        let a: unsigned[CFG.width] = 40;
        let b: integer[RANGE[2]];
        assert!(a'length == 6 and b'length == 7, "a={} b={}", a'length, b'length);
    }
}
"#;
