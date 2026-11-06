//! Regressions for sioxc #452 and #444: a slice of an array with negative
//! bounds reads and writes the labelled elements, not element 0, and an
//! overlapping copy within one such array runs in the right order.

use std::process::Command;

fn passes(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i452_{name}_{}", std::process::id()));
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
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[cfg(feature = "llvm")]
#[test]
fn negative_slices_read_and_write_their_labels() {
    passes("labels", LABELS);
}

#[cfg(feature = "llvm")]
#[test]
fn the_issue_452_read_copies_the_right_elements() {
    passes("read", READ);
}

#[cfg(feature = "llvm")]
#[test]
fn the_issue_444_process_slice_runs() {
    passes("process", PROCESS);
}

const LABELS: &str = r#"module p452;

#[test]
entity Tb {}

impl Tb {
    let src: integer[-2..0] = [100, 200, 300];
    let dst: integer[0..2] = [0, 0, 0];
    let low: integer[-5..-1] = [10, 20, 30, 40, 50];
    let pair: integer[0..1] = [0, 0];
    let arr: integer[-3..0] = [10, 20, 30, 40];
    dst[0..1] = src[-2..-1];
    pair[0..1] = low[-5..-4];
    arr[-3..-2] = [98, 99];

    check: process {
        let p: integer[-3..0] = [1, 2, 3, 4];
        p[-2..-1] = p[-3..-2];
        await 1ns;
        assert!(dst[0] == 100 and dst[1] == 200, "dst {} {}", dst[0], dst[1]);
        assert!(pair[0] == 10 and pair[1] == 20, "pair {} {}", pair[0], pair[1]);
        assert!(arr[-3] == 98 and arr[-2] == 99 and arr[-1] == 30, "arr {} {} {}", arr[-3], arr[-2], arr[-1]);
        assert!(p[-3] == 1 and p[-2] == 1 and p[-1] == 2 and p[0] == 4, "p {} {} {} {}", p[-3], p[-2], p[-1], p[0]);
    }
}
"#;

const READ: &str = r#"module test_rhs_neg_slice;

#[test]
entity TestRhsNegSlice {}
impl TestRhsNegSlice {
    let src: integer[-2..0] = [100, 200, 300];
    let dst: integer[0..2] = [0, 0, 0];
    dst[0..1] = src[-2..-1];
    print!("dst[0]={} dst[1]={} dst[2]={}", dst[0], dst[1], dst[2]);
    assert!(dst[0] == 100, "dst[0] == 100");
}
"#;

const PROCESS: &str = r#"module test_neg_slice_process;

#[test]
entity Top {}
impl Top {
    check: process {
        // Bug 1: Slicing negative source indices silently evaluates to element index 0
        let other: integer[-3..0] = [10, 20, 30, 40];
        // other[-3] = 10, other[-2] = 20, other[-1] = 30, other[0] = 40
        let dest: integer[2] = [0, 0];
        dest[0..1] = other[-2..-1];
        print!("dest: [{}, {}]\n", dest[0], dest[1]);
        // Expected: dest[0] == 20, dest[1] == 30
        // Actual: dest is [40, 40] (both read other[0]!)
        assert!(dest[0] == 20, "dest[0] should be 20, got 40");
        assert!(dest[1] == 30, "dest[1] should be 30, got 40");

        // Bug 2: Slicing negative target indices fails code generation
        let target_arr: integer[-3..0] = [1, 2, 3, 4];
        target_arr[-2..-1] = [88, 99];
    }
}
"#;
