//! Regression for sioxc #696: range attributes fold through named and
//! derived integer constants (`unsigned[HI..LO]`, `TOP = HI + 1`) and
//! `'left`/`'right` fold on a declared range. Both folding passes read
//! literal bounds only, leaving an unresolved width or a `'left` of 0.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn range_attributes_fold_through_named_constants() {
    let dir = std::env::temp_dir().join(format!("siox_i696_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("attrs.siox");
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
        run.status.success() && stdout.contains("9 200 6 6"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module p696;

use std::bits::unsigned;

const LO: integer = 2;
const HI: integer = 5;
const TOP: integer = HI + 1;

entity TopPortRangeAttr {
    x: unsigned[HI..LO] in,
    y: unsigned[x'length] out,
}
impl TopPortRangeAttr {
    y = x;
}

entity TopLeft {
    x: unsigned[7..0] in,
    y: unsigned[x'left + 1] out,
}
impl TopLeft {
    y = x;
}

#[test]
entity Tb {}
impl Tb {
    let a: unsigned[4] = 9;
    let b: unsigned[4];
    let c: unsigned[8] = 200;
    let d: unsigned[8];
    let one: TopPortRangeAttr = { .x = a, .y = b };
    let two: TopLeft = { .x = c, .y = d };
    process {
        await 1ns;
        let e: integer = unsigned[6..4](5)'left;
        let f: integer = unsigned[TOP..LO](5)'left;
        print!("{} {} {} {}\n", b, d, e, f);
        assert!(b == 9 and d == 200 and e == 6 and f == 6);
        print!("done\n");
    }
}
"#;
