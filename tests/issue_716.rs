//! Regression for sioxc #716: a user `impl Suffix<"..", real>` literal folds
//! through its impl in a constant and a signal initializer. The built-in
//! unit shortcut read `2.0scale10` as 2, `1.5scale10` did not fold, and a
//! `real` initializer value was dropped (E-P021, or its bits read as 1e-323).

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn real_suffix_constants_fold_through_their_impl() {
    let dir = std::env::temp_dir().join(format!("siox_i716_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("suffix.siox");
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
        run.status.success() && stdout.contains("y_drv=15 y_b=20 y_init=20 a=15"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module repro;

impl Suffix<"scale10", real> for real {
    static fn suffix(x: real) -> real {
        return x * 10.0;
    }
}

// 1. If uncommented, fails to fold (`no value named A is in scope`):
const A: real = 1.5scale10;

// 2. Folds to 2.0 instead of 20.0:
const B: real = 2.0scale10;

entity Top {
    y_drv: real out,
    y_b: real out,
    y_init: real out,
}

impl Top {
    // 3. Initializes `s` to `f64::from_bits(2)` (1e-323) instead of 20.0
    //    (and `1.5scale10` or `2scale10` here fails with E-P021):
    let s: real = 2.0scale10;
    y_drv = 1.5scale10;
    y_b = B;
    y_init = s;
}

#[test]
entity Tb {}

impl Tb {
    let y_drv: real;
    let y_b: real;
    let y_init: real;
    let dut: Top = { .y_drv = y_drv, .y_b = y_b, .y_init = y_init };

    process {
        await 1ns;
        print!("y_drv={} y_b={} y_init={} a={}\n", y_drv, y_b, y_init, A);
        assert!(y_drv == 15.0 and y_b == 20.0 and y_init == 20.0 and A == 15.0);
        print!("done\n");
    }
}
"#;
