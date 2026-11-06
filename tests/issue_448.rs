//! Regression for sioxc #448: an array whose outer dimension starts above
//! or below 0 (`integer[2][10..11]`, `integer[-1..0][-2..-1]`) is indexed
//! by its declared labels, and a slice of one of its rows lowers.

use std::process::Command;

fn passes(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i448_{name}_{}", std::process::id()));
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
fn a_row_of_an_offset_array_slices() {
    passes("issue", ISSUE);
}

#[cfg(feature = "llvm")]
#[test]
fn offset_and_negative_outer_labels_index() {
    passes("labels", LABELS);
}

const ISSUE: &str = r#"module test_2d_slice;

#[test]
entity Top {}
impl Top {
    check: process {
        let grid: integer[2][10..11] = [
            [100, 200],
            [300, 400],
        ];
        // Slicing inner row of multidimensional array with non-zero outer bounds
        let s: integer[2] = grid[10][0..1];
        print!("s[0] = {}, s[1] = {}\n", s[0], s[1]);
        assert!(s[0] == 100);
        assert!(s[1] == 200);
    }
}
"#;

const LABELS: &str = r#"module p448;

#[test]
entity Top {}
impl Top {
    check: process {
        let grid: integer[2][10..11] = [[100, 200], [300, 400]];
        let neg: integer[-1..0][-2..-1] = [[1, 2], [3, 4]];
        let s: integer[2] = grid[11][0..1];
        assert!(grid[10][1] == 200 and s[0] == 300, "grid {} {}", grid[10][1], s[0]);
        assert!(neg[-2][-1] == 1 and neg[-1][0] == 4, "neg {} {}", neg[-2][-1], neg[-1][0]);
    }
}
"#;
