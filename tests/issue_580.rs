//! Regression for sioxc #580: a system attribute in a type width names a
//! port or signal: an unknown one (`unsigned[nonexistent'length]`) is
//! reported, and a single-bit scalar's (`x: Bit`, `unsigned[x'length]`)
//! folds to its one element. Both left the width unknown for lowering.

use std::process::Command;

fn compile(name: &str, source: &str, test: bool) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i580_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    command.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")]);
    if test {
        command.arg("--test");
    } else {
        command.args(["--emit", "ir"]);
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

#[test]
fn an_unknown_attribute_object_in_a_width_is_reported() {
    let (ok, text, _) = compile("unknown", UNKNOWN, false);
    assert!(!ok, "{text}");
    assert!(
        text.contains("cannot find value `nonexistent` in this scope"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_bit_ports_length_sizes_a_width() {
    let (ok, text, binary) = compile("bit", BIT, true);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success() && stdout.contains("done"), "{stdout}");
}

const UNKNOWN: &str = r#"module m;

entity Dut {
    y: unsigned[nonexistent'length] out,
}

impl Dut {
    y = 0;
}
"#;

const BIT: &str = r#"module p580;

use std::bits::unsigned;

entity Dut {
    x: Bit in,
    y: unsigned[x'length] out,
}

impl Dut {
    y = unsigned[1](1);
}

#[test]
entity TB {}
impl TB {
    let x: Bit;
    let y: unsigned[1];
    let dut: Dut = { .x = x, .y = y };
    process {
        await 1ns;
        assert!(y == 1, "y {}", y);
        print!("done\n");
    }
}
"#;
