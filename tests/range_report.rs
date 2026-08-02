//! A range violation names the value that broke the domain.
//!
//! The engine flags a ranged signal *before* the value is narrowed to the
//! destination width, but the message was rebuilt by reading the signal back —
//! that is, after truncation. `t + step` of 10 into `integer<-8..7>` stores as
//! -6, so the report read "`t` left its range -8..7 (it was -6)": a number
//! inside the range it says was left. The engine now keeps the offending value
//! alongside the signal id.
//!
//! Only meaningful with the `llvm` feature + a clang toolchain.

use std::process::Command;

#[test]
fn a_range_violation_reports_the_value_that_broke_it() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let siox = env!("CARGO_BIN_EXE_sioxc");
    let root = env!("CARGO_MANIFEST_DIR");
    let out = std::env::temp_dir().join(format!("siox_range_{}", std::process::id()));

    let status = Command::new(siox)
        .current_dir(root)
        .args([
            "--test",
            "tests/fixtures/range_report_test.siox",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "sioxc --test failed");

    let run = Command::new(&out).output().unwrap();
    let text =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(
        !run.status.success(),
        "a design that leaves its range must fail:\n{text}"
    );

    // Overflow past the storage width: 5 + 5 = 10 stores as -6 in four bits.
    assert!(
        text.contains("left its range -8..7 (it was 10)"),
        "overflow must report 10, not the truncated -6:\n{text}"
    );
    // Underflow the same way: -5 + -5 = -10 stores as 6.
    assert!(
        text.contains("left its range -8..7 (it was -10)"),
        "underflow must report -10:\n{text}"
    );
    // Out of range but representable, so the post-settle scan reports it and
    // the stored value is already the right one to name.
    assert!(
        text.contains("left its range 0..5 (it was 6)"),
        "a representable violation must report 6:\n{text}"
    );

    // Every value named is genuinely outside the range quoted beside it.
    for line in text.lines().filter(|l| l.contains("left its range")) {
        let (range, value) = line
            .split_once("left its range ")
            .and_then(|(_, rest)| rest.split_once(" (it was "))
            .expect("malformed range report");
        let (lo, hi) = range.split_once("..").expect("malformed range");
        let lo: i64 = lo.trim().parse().unwrap();
        let hi: i64 = hi.trim().parse().unwrap();
        let value: i64 = value.trim_end_matches(')').trim().parse().unwrap();
        assert!(
            value < lo || value > hi,
            "reported {value} as leaving {lo}..{hi}, but it is inside it: {line}"
        );
    }

    let _ = std::fs::remove_file(&out);
}
