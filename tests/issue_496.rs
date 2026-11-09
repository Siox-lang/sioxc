//! Regression for sioxc #496: a partial range follows the object left to
//! right and keeps its order: an omitted bound is its `'left` or `'right`,
//! and a packed vector's slice keeps its significance, so `v[..]` is `v`,
//! and on an `unsigned[8]` `v[..4]` is bits 4 down to 0. Read in written
//! order a width-declared vector's slice came out bit-reversed.

use std::process::Command;

fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_i496_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
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
        "{name}: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout).to_string();
    assert!(run.status.success(), "{name}: {stdout}");
    stdout
}

#[cfg(feature = "llvm")]
#[test]
fn partial_ranges_keep_the_order_in_a_testbench() {
    assert!(run("process", PROCESS).contains("done"));
}

#[cfg(feature = "llvm")]
#[test]
fn a_full_partial_range_is_the_vector_in_hardware() {
    let out = run("hardware", HARDWARE);
    assert!(out.contains("out_full = 12"), "{out}");
}

const PROCESS: &str = r#"module p496;

use std::bits::unsigned;

#[test]
entity TB {}
impl TB {
    run: process {
        let v: unsigned[8] = 0xAB;
        let all: unsigned[8] = v[..];
        assert!(all == 0xAB, "v[..] {}", all);
        let low: unsigned[5] = v[..4];
        let high: unsigned[4] = v[4..];
        assert!(low == 11 and high == 10, "low {} high {}", low, high);
        let d: unsigned[7..0] = 0xAB;
        let dl: unsigned[4] = d[..4];
        let dr: unsigned[4] = d[3..];
        assert!(dl == 10 and dr == 11, "descending {} {}", dl, dr);
        let w: unsigned[8] = 0;
        w[..3] = 9;
        assert!(w == 9, "write {}", w);
        print!("done\n");
    }
}
"#;

const HARDWARE: &str = r#"module test_compare;

entity Foo {
    inp: unsigned[8] in,
    out_full: unsigned[8] out,
    out_slice: unsigned[8] out,
}
impl Foo {
    out_full = inp[..];     // Bit-reverses the bus!
    out_slice = inp[7..0];   // Preserves natural order
}

#[test]
entity TestBench {}
impl TestBench {
    let inp: unsigned[8];
    let out_full: unsigned[8];
    let out_slice: unsigned[8];
    let f: Foo = { .inp = inp, .out_full = out_full, .out_slice = out_slice };

    check: process {
        inp = 0x12; // 0b0001_0010
        await 0ns;
        print!("inp = {:x}, out_slice = {:x}, out_full = {:x}\n", inp, out_slice, out_full);
        assert!(out_slice == 0x12);
        assert!(out_full == 0x12);
    }
}
"#;
