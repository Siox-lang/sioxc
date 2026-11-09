//! Regression for sioxc #572: a descending range pattern whose bounds are
//! not integers (`2.0..1.0`, `20ns..10ns`, `MAX..MIN` of real constants) is
//! one set either way, in hardware as in a testbench. Hardware folded only
//! integer bounds, so it compared `x >= 2.0 and x <= 1.0`: never true.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn descending_real_and_time_ranges_match_in_hardware() {
    let dir = std::env::temp_dir().join(format!("siox_i572_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("ranges.siox");
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

const SOURCE: &str = r#"module p572;

const MAX: real = 2.0;
const MIN: real = 1.0;

entity Top {
    x: real in,
    t: time in,
    a: integer out,
    b: integer out,
    c: integer out,
}

impl Top {
    process {
        match x {
            2.0..1.0 => { a = 42; }
            _ => { a = 0; }
        }
        match t {
            20ns..10ns => { b = 42; }
            _ => { b = 0; }
        }
        match x {
            MAX..MIN => { c = 42; }
            _ => { c = 0; }
        }
    }
}

#[test]
entity TB {}

impl TB {
    let x: real;
    let t: time;
    let a: integer;
    let b: integer;
    let c: integer;
    let dut: Top = { .x = x, .t = t, .a = a, .b = b, .c = c };

    process {
        x = 1.5;
        t = 15ns;
        await 10ns;
        assert!(a == 42 and b == 42 and c == 42, "inside {} {} {}", a, b, c);
        x = 2.5;
        t = 25ns;
        await 10ns;
        assert!(a == 0 and b == 0 and c == 0, "outside {} {} {}", a, b, c);
        print!("done\n");
    }
}
"#;
