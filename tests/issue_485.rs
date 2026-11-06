//! Regression for sioxc #485: an instance array declared with a range
//! (`let s: Sub[10..12];`) is indexed by its declared labels, and a label
//! outside them is reported.

use std::process::Command;

fn compile(name: &str, source: &str, test: bool) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i485_{name}_{}", std::process::id()));
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

#[cfg(feature = "llvm")]
#[test]
fn ranged_instances_chain_by_their_labels() {
    let (ok, text, binary) = compile("chain", CHAIN, true);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[test]
fn a_label_outside_the_instance_range_is_reported() {
    let (ok, text, _) = compile("outside", OUTSIDE, false);
    assert!(!ok, "{text}");
    assert!(text.contains("instance 0 is outside `10..12`"), "{text}");
}

const CHAIN: &str = r#"module p485;

entity Sub {
    in_val: unsigned[8] in,
    out_val: unsigned[8] out,
}
impl Sub {
    out_val = in_val + 1;
}

entity Top {
    x: unsigned[8] in,
    y: unsigned[8] out,
}

impl Top {
    let s: Sub[10..12];
    s[10].in_val = x;
    s[11].in_val = s[10].out_val;
    s[12].in_val = s[11].out_val;
    y = s[12].out_val;
}

#[test]
entity Tb {}
impl Tb {
    let x: unsigned[8] = 4;
    let y: unsigned[8];
    let dut: Top = { .x = x, .y = y };
    check: process {
        await 1ns;
        assert!(y == 7, "y={}", y);
    }
}
"#;

const OUTSIDE: &str = r#"module p485_oob;

entity Sub {
    in_val: unsigned[8] in,
    out_val: unsigned[8] out,
}
impl Sub {
    out_val = in_val;
}

entity Top {
    x: unsigned[8] in,
}

impl Top {
    let s: Sub[10..12];
    s[0].in_val = x;
}
"#;
