//! Regression for sioxc #604: a newtype built from its base newtype
//! (`Outer(inner)` of `struct Outer(Inner)`) is the base's value, in
//! hardware and in a testbench; it lowered to nothing and crashed
//! hardware. Uninitialized, such a newtype has its base's width, which the
//! unsized-vector check (#570) must not take for none.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_newtype_of_a_newtype_constructs() {
    let dir = std::env::temp_dir().join(format!("siox_i604_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("newtype.siox");
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

const SOURCE: &str = r#"module p604;

use std::bits::unsigned;

struct Inner(unsigned[8]);
struct Outer(Inner);

entity Dut {
    a: Inner in,
    b: Outer out,
}

impl Dut {
    b = Outer(a);
}

#[test]
entity Tb {}
impl Tb {
    let i: Inner = Inner(42);
    let b: Outer;
    let d: Dut = { .a = i, .b = b };
    run: process {
        await 1ns;
        let o: Outer = Outer(i);
        assert!(unsigned[8](Inner(b)) == 42 and unsigned[8](Inner(o)) == 42, "values");
        print!("done\n");
    }
}
"#;
