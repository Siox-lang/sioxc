//! Regressions for sioxc #626 and #655: chained slices. An index or a
//! range into a slice is checked against the slice's length
//! (`arr[1..3][5]`, `[-1]`, `[0..5]` of three elements are reported); it
//! read past the slice into the array (#626). And a chain of several
//! slices (`arr[0..7][1..5][2]`) flattens fully; one pass left a chain
//! that had no hardware form (#655).

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i626_{name}_{}", std::process::id()));
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
fn an_index_past_a_slice_is_reported() {
    let (ok, text, _) = compile("index", INDEX);
    assert!(!ok, "{text}");
    assert!(
        text.contains("index 5 is outside this 3-element slice"),
        "{text}"
    );
    assert!(
        text.contains("index -1 is outside this 3-element slice"),
        "{text}"
    );
    let (ok, text, _) = compile("range", RANGE);
    assert!(!ok, "{text}");
    assert!(
        text.contains("`0..5` is outside this 3-element slice"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_chain_of_slices_flattens() {
    let (ok, text, binary) = compile("chain", CHAIN);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success() && stdout.contains("done"), "{stdout}");
}

const INDEX: &str = r#"module test_slice_oob;

#[test] entity T {}
impl T {
    process {
        let arr: integer[10] = [0, 10, 20, 30, 40, 50, 60, 70, 80, 90];
        // arr[1..3] contains: arr[1]=10, arr[2]=20, arr[3]=30 (len 3).
        // Index 5 is out-of-bounds, but rewrites to arr[6], printing 60:
        let x: integer = arr[1..3][5];
        print!("x = {}\n", x);
        assert!(x == 60);

        // Index -1 is negative, but rewrites to arr[0], printing 0:
        let y: integer = arr[1..3][-1];
        print!("y = {}\n", y);
        assert!(y == 0);
    }
}
"#;

const RANGE: &str = r#"module test_slice_expansion;

#[test] entity T {}
impl T {
    process {
        let arr: integer[10] = [0, 10, 20, 30, 40, 50, 60, 70, 80, 90];
        // arr[1..3] has 3 elements.
        // arr[1..3][0..5] asks for 6 elements from a 3-element slice:
        let sub: integer[6] = arr[1..3][0..5];
        print!("sub[5] = {}\n", sub[5]);
        assert!(sub[5] == 60);
    }
}
"#;

const CHAIN: &str = r#"module p655;

entity Top {
    outp: integer out,
}

impl Top {
    let arr: integer[8] = [10, 11, 12, 13, 14, 15, 16, 17];
    outp = arr[0..7][1..5][2];
}

#[test]
entity TB {}
impl TB {
    let outp: integer;
    let dut: Top = { .outp = outp };
    run: process {
        await 1ns;
        let arr: integer[8] = [10, 11, 12, 13, 14, 15, 16, 17];
        let x: integer = arr[0..7][1..5][2];
        let y: integer = arr[1..3][2];
        assert!(outp == 13 and x == 13 and y == 13, "outp {} x {} y {}", outp, x, y);
        print!("done\n");
    }
}
"#;
