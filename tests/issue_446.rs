//! Regression for sioxc #446: a slice of an unpacked array passed to a
//! procedure (`swap(arr[0..1], arr[2..3])`, a procedure writing its slice
//! parameter) is copied in and written back element by element.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn slice_arguments_are_written_back() {
    let dir = std::env::temp_dir().join(format!("siox_i446_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("slices.siox");
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
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const SOURCE: &str = r#"module test_unpacked_slice_param;

use std::mem::swap;

fn reverse_two(slice: integer[2]) {
    let tmp: integer = slice[0];
    slice[0] = slice[1];
    slice[1] = tmp;
}

#[test]
entity Top {}
impl Top {
    check: process {
        let arr: integer[4] = [10, 20, 30, 40];

        // Case 1: std::mem::swap on array slices
        swap(arr[0..1], arr[2..3]);
        print!("after swap: arr = [{}, {}, {}, {}]\n", arr[0], arr[1], arr[2], arr[3]);
        assert!(arr[0] == 30);
        assert!(arr[1] == 40);
        assert!(arr[2] == 10);
        assert!(arr[3] == 20);

        // Case 2: Custom inlined procedure mutating slice parameter
        reverse_two(arr[0..1]);
        print!("after reverse: arr = [{}, {}, {}, {}]\n", arr[0], arr[1], arr[2], arr[3]);
        assert!(arr[0] == 40);
        assert!(arr[1] == 30);
    }
}
"#;
