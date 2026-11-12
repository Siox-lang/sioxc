//! Regression for sioxc #610: an instance's `out` port is driven by the
//! instance, so assigning it from the instantiating entity or testbench
//! (`u.q = ..`) is reported. It overwrote the output in a testbench and
//! met a second driver in hardware.

use std::process::Command;

fn diagnostics(name: &str, source: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("siox_i610_{name}_{}", std::process::id()));
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
    (out.status.success(), text)
}

#[test]
fn an_instance_output_is_not_assigned() {
    for (name, source, entity) in [
        ("testbench", TESTBENCH, "Dut"),
        ("hardware", HARDWARE, "Sub"),
    ] {
        let (ok, text) = diagnostics(name, source);
        assert!(!ok, "{text}");
        assert!(
            text.contains(&format!(
                "cannot assign to `u.q`: `q` is an output of `{entity}`"
            )),
            "{text}"
        );
    }
}

const TESTBENCH: &str = r#"module test_tb_overwrite_dut;

entity Dut {
    a: Bit in,
    q: Bit out
}

impl Dut {
    q = not a;
}

#[test]
entity Tb {}

impl Tb {
    let u: Dut;

    run: process {
        u.a = '1';
        // u.q should compute to '0' (not '1'), but testbench illegally overwrites it:
        u.q = '1';
        assert!(u.q == '1');
    }
}
"#;

const HARDWARE: &str = r#"module test_drive_sub_out;

entity Sub {
    q: Bit out,
    y: Bit out
}

impl Sub {
    y = q;
}

entity Top {
    d: Bit in,
    res: Bit out
}

impl Top {
    let u: Sub;
    // Top drives child instance's output port q:
    u.q = d;
    res = u.y;
}
"#;
