//! Regression for sioxc #530 and #545: `std::math::Complex` has the
//! field's arithmetic (product, quotient, negation, equality, conjugate)
//! and mixes with `real` and `integer` operands on either side.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn complex_arithmetic() {
    let dir = std::env::temp_dir().join(format!("siox_i530_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("complex.siox");
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

const SOURCE: &str = r#"module p530b;

use std::math::{Complex, conj};

#[test]
entity TB {}
impl TB {
    run: process {
        let z1: Complex = 1 + 2i;
        let z2: Complex = 3 + 4i;
        let prod: Complex = z1 * z2;
        assert!(prod.re == 0.0 - 5.0 and prod.im == 10.0, "mul {}", prod);
        let q: Complex = prod / z2;
        assert!(q == z1, "div {}", q);
        assert!(-z1 == Complex { .re = 0.0 - 1.0, .im = 0.0 - 2.0 }, "neg");
        assert!(z1 - 1 == 2i and z1 - 0.5 == Complex { .re = 0.5, .im = 2.0 }, "scalar sub");
        assert!(z1 + 1.5 == Complex { .re = 2.5, .im = 2.0 }, "real add");
        assert!(z1 * 2.0 == z1 * 2 and z1 / 2 == z1 * 0.5, "scalars");
        assert!(2.0 * z1 == z1 * 2.0 and 2 * z1 == z1 * 2, "scalar on the left");
        assert!(1 - z1 == Complex { .re = 0.0, .im = 0.0 - 2.0 } and 1.5 + z1 == z1 + 1.5, "left sub and add");
        assert!(Complex(3.14) == Complex { .re = 3.14, .im = 0.0 }, "from real");
        assert!(conj(z1) == Complex { .re = 1.0, .im = 0.0 - 2.0 }, "conj");
        assert!(z1 == z1 and not (z1 == z2), "eq");
        print!("done\n");
    }
}
"#;
