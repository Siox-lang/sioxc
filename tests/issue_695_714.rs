//! Regression for sioxc #695 and #714: a slice chain folds named constants
//! (`arr[LO..HI][1]`), checks every slice from the inside out
//! (`arr[1..3][0..99][0]`), and indexes a packed vector's slice from its
//! least significant element, as binding the slice does (`v[5..2][0]` is
//! `v[2]`). An unfolded chain had no hardware form, a named index past the
//! slice read past it, and a middle slice was dropped unchecked.

use std::process::Command;

fn build(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i695_{name}_{}", std::process::id()));
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

#[cfg(feature = "llvm")]
#[test]
fn slice_chains_fold_constants_and_count_packed_slices_from_the_right() {
    let (ok, text, binary) = build("valid", VALID);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success() && stdout.contains("done"), "{stdout}");
}

#[test]
fn named_and_middle_slices_past_the_end_are_reported() {
    let (ok, text, _) = build("outside", OUTSIDE);
    assert!(!ok, "{text}");
    assert!(
        text.contains("index 5 is outside this 3-element slice"),
        "{text}"
    );
    assert!(
        text.contains("`0..99` is outside this 3-element slice"),
        "{text}"
    );
}

const VALID: &str = r#"module p695;

use std::bits::unsigned;

const LO: integer = 1;
const HI: integer = 3;
const TWO: integer = 2;

entity Hw {
    outp: integer out,
}

impl Hw {
    let arr: integer[8] = [10, 11, 12, 13, 14, 15, 16, 17];
    outp = arr[LO..HI][1];
}

#[test]
entity Tb {}

impl Tb {
    let outp: integer;
    let dut: Hw = { .outp = outp };

    process {
        await 1ns;
        let v2: unsigned[8] = 4;
        let sub: unsigned[4] = v2[5..2];
        let arr: integer[10] = [0, 10, 20, 30, 40, 50, 60, 70, 80, 90];
        let i: integer = 1;
        print!("{} {} {} {} {} {}\n", outp, sub[0], v2[5..2][0], v2[5..2][TWO], arr[3..1][0], arr[3..1][i]);
        assert!(outp == 12 and sub[0] == v2[5..2][0] and arr[3..1][0] == 30 and arr[3..1][i] == 20);
        assert!(v2[5..2][TWO] == v2[4]);
        print!("done\n");
    }
}
"#;

const OUTSIDE: &str = r#"module test_slice_chain_bugs;

const LO: integer = 1;
const HI: integer = 3;
const OOB_IDX: integer = 5;

entity TopNamedConstSlice {
    outp: integer out,
}
impl TopNamedConstSlice {
    let arr: integer[8] = [10, 11, 12, 13, 14, 15, 16, 17];
    outp = arr[LO..HI][1];
}

#[test]
entity TbOobBypass {}
impl TbOobBypass {
    process {
        let arr: integer[10] = [0, 10, 20, 30, 40, 50, 60, 70, 80, 90];
        // 1. Named constant index OOB_IDX = 5 on 3-element slice arr[1..3]:
        // Bypassed and rewrites to arr[1 + 5] = arr[6] = 60!
        let x: integer = arr[1..3][OOB_IDX];
        print!("x (named const OOB) = {}\n", x);

        // 2. 3-level chain with out-of-bounds middle slice [0..99] on 3-element slice arr[1..3]:
        // Top-down flatten collapses [0..99][0] -> [0] first, bypassing the check on [0..99]!
        let y: integer = arr[1..3][0..99][0];
        print!("y (middle slice OOB) = {}\n", y);
    }
}
"#;
