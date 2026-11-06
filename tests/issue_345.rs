//! Antigravity's issue #345: a signed runtime index into an array with
//! negative bounds.

use std::process::Command;

fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_issue_345_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let binary = dir.join(name);
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
            "-o",
        ])
        .arg(&binary)
        .arg(&file)
        .output()
        .unwrap();
    assert!(output.status.success(), "{name}: {output:?}");
    String::from_utf8_lossy(&Command::new(&binary).output().unwrap().stdout).to_string()
}

/// `arr[i]` with `i: signed[4] = -2` on `T[-2..0]` is the first element, in
/// a function, a process and hardware, read and written.
#[test]
fn a_signed_index_reaches_negative_labels() {
    let issue = run(
        "issue",
        r#"module test_fn_dynamic_neg;

use std::bits::signed;

fn get(arr: unsigned[8][-2..0], i: signed[4]) -> unsigned[8] {
    return arr[i];
}

#[test]
entity TestBench {}

impl TestBench {
    process {
        let arr: unsigned[8][-2..0] = [10, 20, 30];
        let i_m2: signed[4] = -2;
        let i_m1: signed[4] = -1;
        let i_0: signed[4] = 0;
        let v_m2: unsigned[8] = get(arr, i_m2);
        let v_m1: unsigned[8] = get(arr, i_m1);
        let v_0: unsigned[8] = get(arr, i_0);
        assert!(v_m2 == 10, "v_m2 == 10");
        assert!(v_m1 == 20, "v_m1 == 20");
        assert!(v_0 == 30, "v_0 == 30");
    }
}
"#,
    );
    assert!(issue.contains("1 passed"), "{issue}");
    let both = run(
        "both",
        r#"module v345;
use std::bits::{unsigned, signed};
entity Top { i: signed[4] in, arr: unsigned[8][-2..0] in, y: unsigned[8] out, w: unsigned[8][-2..0] out }
impl Top {
    y = arr[i];
    process { w = [0, 0, 0]; w[i] = 99; }
}
#[test] entity T {}
impl T {
    let i: signed[4] = -2; let arr: unsigned[8][-2..0] = [10, 20, 30]; let y: unsigned[8]; let w: unsigned[8][-2..0];
    let t: Top = { .i = i, .arr = arr, .y = y, .w = w };
    process {
        await 1ns;
        let local: unsigned[8][-2..0] = [10, 20, 30];
        let k: signed[4] = -1;
        local[k] = 77;
        print!("{} {} {} {} {}\n", y, w[-2], w[-1], local[k], local[-2]);
        assert!(y == 10 and w[-2] == 99 and w[-1] == 0 and local[-1] == 77 and local[-2] == 10);
        i = -1; await 1ns;
        assert!(y == 20 and w[-1] == 99 and w[-2] == 0);
        i = 0; await 1ns;
        assert!(y == 30 and w[0] == 99);
    }
}
"#,
    );
    assert!(
        both.contains("10 99 0 77 10") && both.contains("1 passed"),
        "{both}"
    );
}
