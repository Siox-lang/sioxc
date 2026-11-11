//! Regressions for sioxc #617 and #632: time units. A descending range of
//! mixed units (`1us..500ns`) orders by each side's scaled value in
//! hardware, as the testbench does; only one unit compared before. And a
//! `real` scales a `time` from either side (`10ns * 1.5`, `2.5 * 4ns`,
//! `9ns / 2.0`), as an `integer` does.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn time_units_scale_and_order() {
    let dir = std::env::temp_dir().join(format!("siox_i617_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("time.siox");
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
    assert!(run.status.success() && stdout.contains("done"), "{stdout}");
}

const SOURCE: &str = r#"module p617;

use std::sim::time;

entity Top {
    t: time in,
    a: integer out,
    b: integer out,
}

impl Top {
    process {
        match t {
            1us..500ns => { a = 1; }
            _ => { a = 0; }
        }
        match t {
            1s..500ms => { b = 1; }
            _ => { b = 0; }
        }
    }
}

#[test]
entity TB {}
impl TB {
    let t: time;
    let a: integer;
    let b: integer;
    let dut: Top = { .t = t, .a = a, .b = b };
    process {
        t = 700ns;
        await 1ns;
        assert!(a == 1 and b == 0, "700ns {} {}", a, b);
        t = 2us;
        await 1ns;
        assert!(a == 0, "2us {}", a);
        let x: time = 10ns * 1.5;
        let y: time = 2.5 * 4ns;
        let z: time = 9ns / 2.0;
        assert!(x == 15ns and y == 10ns and z == 4500ps, "real scaling");
        print!("done\n");
    }
}
"#;
