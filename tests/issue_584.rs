//! Regression for sioxc #584: a generate `if` or `for` on a module-level
//! constant (`if USE_FEATURE`, `for i in 0..N`) is elaborated. The
//! elaborator folded only the entity's parameters, so the `if` read as a
//! signal test ("an entity cannot be instantiated in a process") and the
//! `for` gathered no instances.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn generates_fold_module_constants() {
    let dir = std::env::temp_dir().join(format!("siox_i584_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("generate.siox");
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

const SOURCE: &str = r#"module p584;

use std::bits::unsigned;

const USE_FEATURE: Bool = true;
const N: integer = 3;

entity Sub {
    y: Logic out,
}

impl Sub {
    y = '1';
}

entity Dut {
    on: Logic out,
    y: unsigned[4] out,
}

impl Dut {
    if USE_FEATURE {
        let s: Sub = {};
        on = s.y;
    } else {
        on = '0';
    }
    for i in 0..N {
        let s: Sub = {};
        y[i] = s.y;
    }
}

#[test]
entity TB {}
impl TB {
    let on: Logic;
    let y: unsigned[4];
    let dut: Dut = { .on = on, .y = y };
    process {
        await 1ns;
        assert!(on == '1' and y == 15, "on {} y {}", on, y);
        print!("done\n");
    }
}
"#;
