//! Regression for sioxc #661: a slice bounded by a named constant
//! (`x[0..HI]`, `x[..HI]`) has its width, so a wrong-width return or
//! argument is reported and a right-width one runs. Only literal bounds
//! sized a slice; with a constant it typed as width 0, assignable anywhere.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i661_{name}_{}", std::process::id()));
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
    (out.status.success(), text, dir.join("t.bin"))
}

#[test]
fn a_wrong_width_constant_slice_is_reported() {
    let (ok, text, _) = compile("wrong", WRONG);
    assert!(!ok, "{text}");
    assert!(
        text.contains("cannot return unsigned[8] from a function declared to return unsigned[7]"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_constant_slice_runs() {
    let (ok, text, binary) = compile("right", RIGHT);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const WRONG: &str = r#"module test_slice_const_bug;

const HI: integer = 7;

fn get_7_bits(x: unsigned[16]) -> unsigned[7] {
    // x[0..HI] covers 8 bits (0..7), but is typed as unsigned[] (width 0).
    // Bypasses return width checking: compiles without error!
    return x[0..HI];
}

fn takes_7_bits(v: unsigned[7]) -> unsigned[7] {
    return v;
}

#[test]
entity TestBench {}

impl TestBench {
    run: process {
        let x: unsigned[16] = 0x00ff; // 255
        // Bypasses argument width checking: compiles without error!
        let a: unsigned[7] = takes_7_bits(x[0..HI]);
        let r: unsigned[7] = get_7_bits(x);
        await 10ns;
        print!("a={} r={}\n", a, r);
        // At runtime, 255 is silently truncated to 127:
        assert!(a == 127);
    }
}
"#;

const RIGHT: &str = r#"module p661;

use std::bits::unsigned;

const HI: integer = 6;

fn low7(x: unsigned[16]) -> unsigned[7] {
    return x[HI..0];
}

#[test]
entity TB {}
impl TB {
    run: process {
        let x: unsigned[16] = 0x00ff;
        let y: unsigned[7] = low7(x);
        let z: unsigned[7] = x[..HI];
        assert!(y == 127 and z == 127, "y {} z {}", y, z);
        print!("done\n");
    }
}
"#;
