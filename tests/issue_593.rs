//! Regression for sioxc #593: `frequency` arithmetic stores its value into a
//! `frequency` output in hardware. An operator returning its own type was
//! sliced to the operand's width, and the slice hid that a `real`-backed
//! result was already real: the store converted its bits again (`2e8`
//! stored as 4730986895511650000).

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn frequency_arithmetic_stores_its_value_in_hardware() {
    let dir = std::env::temp_dir().join(format!("siox_i593_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("frequency.siox");
    std::fs::write(&file, SOURCE).unwrap();
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
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains("a=200000000 b=50000000 c=50000000"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module r582;
use std::sim::frequency;
entity Top {
    f: frequency in,
    a: frequency out,
    b: frequency out,
    c: frequency out,
}
impl Top {
    a = f * 2;
    b = f / 2.0;
    c = f / 2;
}
#[test] entity TB {}
impl TB {
    let f: frequency;
    let a: frequency;
    let b: frequency;
    let c: frequency;
    let dut: Top = { .f = f, .a = a, .b = b, .c = c };
    process {
        f = 100MHz; await 1ns;
        print!("a={} b={} c={}\n", real(a), real(b), real(c));
        assert!(real(a) == 200000000.0 and real(b) == 50000000.0 and real(c) == 50000000.0);
        print!("done\n");
    }
}
"#;
