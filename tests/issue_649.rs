//! Regression for sioxc #649: attribute reads find the entity and the
//! declaration of their own module. Entity attributes and declaration
//! defaults were kept by bare name across every module, so a second
//! module's `Worker` (or `priority` default) stood for the first's.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn attribute_reads_keep_to_their_module() {
    let dir = std::env::temp_dir().join(format!("siox_i649_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m1.siox"), M1).unwrap();
    std::fs::write(dir.join("m2.siox"), M2).unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(&dir)
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
            "m2.siox",
        ])
        .arg("-o")
        .arg(dir.join("t.bin"))
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains("10 50 10 99"),
        "{stdout}"
    );
}

const M1: &str = r#"module m1;

pub attr speed: integer for entity = 0;
pub attr priority: integer for entity = 10;

pub entity Worker {}
impl Worker {
    attr speed = 10;
}

pub entity Dut {
    out_speed: integer out,
    prio_out: integer out,
}
impl Dut {
    let w: Worker;
    out_speed = w'speed;
    prio_out = Dut'priority;
}
"#;

const M2: &str = r#"module m2;
use m1::Dut;

pub attr speed: integer for entity = 0;
pub attr priority: integer for entity = 99;

pub entity Worker {}
impl Worker {
    attr speed = 50;
}

entity Top {
    out_val: integer out,
    top_speed: integer out,
    dut_prio: integer out,
    top_prio: integer out,
}
impl Top {
    let dut: Dut;
    out_val = dut.out_speed;
    dut_prio = dut.prio_out;
    let w: Worker;
    top_speed = w'speed;
    top_prio = Top'priority;
}

#[test] entity T {}
impl T {
    let out_val: integer;
    let top_speed: integer;
    let dut_prio: integer;
    let top_prio: integer;
    let top: Top = { .out_val = out_val, .top_speed = top_speed, .dut_prio = dut_prio, .top_prio = top_prio };
    process {
        await 1ns;
        print!("{} {} {} {}\n", out_val, top_speed, dut_prio, top_prio);
        assert!(out_val == 10 and top_speed == 50 and dut_prio == 10 and top_prio == 99);
        print!("done\n");
    }
}
"#;
