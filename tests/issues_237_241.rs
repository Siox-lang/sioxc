//! Antigravity's issues #237–#241. Each case is named after its GitHub issue.

use std::process::Command;

/// Build `source` as a test binary and run it, returning everything printed.
fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_issues_237_{}", std::process::id()));
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
    let mut text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        let ran = Command::new(&binary).output().unwrap();
        text += &String::from_utf8_lossy(&ran.stdout);
        text += &String::from_utf8_lossy(&ran.stderr);
    }
    text
}

#[test]
fn slices_pass_to_unsized_parameters() {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "237_loop",
            r#"module test_slice_for;

fn sum_slice(arr: integer[]) -> integer {
    let s: integer = 0;
    for x in arr {
        s = s + x;
    }
    return s;
}

#[test]
entity Top {}

impl Top {
    p: process {
        let data: integer[3] = [10, 20, 30];
        // Passing slice directly crashes during IR validation:
        let total: integer = sum_slice(data[1..2]); print!("total = {}\n", total);
    }
}
"#,
            &[r#"total = 50"#],
        ),
        (
            "237_index",
            r#"module test_slice_index;

fn first(arr: integer[]) -> integer {
    return arr[0];
}

#[test]
entity Top {}

impl Top {
    p: process {
        let data: integer[3] = [10, 20, 30];
        // Compiles, but testbench simulation aborts:
        let x: integer = first(data[1..2]); print!("x = {}\n", x);
    }
}
"#,
            &[r#"x = 20"#],
        ),
    ];
    for (issue, source, printed) in cases {
        let out = run(&format!("run_{issue}"), source);
        assert!(
            out.contains("passed;") && !out.contains("FAILED"),
            "#{issue}: {out}"
        );
        for line in *printed {
            assert!(out.contains(line), "#{issue}: missing `{line}`: {out}");
        }
    }
}

#[test]
fn named_widths_and_generic_bodies_are_checked() {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "239",
            r#"module test_resize_call_wrong;

use std::logic::*;

const WIDTH: integer = 16;

fn takes8(x: unsigned[8]) -> integer {
    print!("takes8 received x = {}, x'length = {}\n", x, x'length);
    assert!(x'length == 8); // FAILS! x'length is 16!
    return 1;
}

#[test]
entity Top {}

impl Top {
    p: process {
        let v: unsigned[8] = 0xAB;
        // WIDTH is 16, but takes8 expects unsigned[8]!
        // Passes Stage 4 with 0 errors:
        let res: integer = takes8(resize(v, WIDTH));
    }
}
"#,
            &[r#"cannot pass unsigned[16] to a unsigned[8] parameter of `takes8`"#],
        ),
        (
            "240",
            r#"module test_conv_call_wrong;

use std::logic::*;

const WIDTH: integer = 16;

fn takes8(x: unsigned[8]) -> integer {
    print!("takes8 received x = {}, x'length = {}\n", x, x'length);
    assert!(x'length == 8); // FAILS! x'length is 16!
    return 1;
}

#[test]
entity Top {}

impl Top {
    p: process {
        let v: unsigned[8] = 0xAB;
        // WIDTH is 16! unsigned[WIDTH](v) is 16 bits.
        // Takes8 expects unsigned[8]. Passes Stage 4 with 0 errors:
        let res: integer = takes8(unsigned[WIDTH](v));
    }
}
"#,
            &[r#"cannot pass unsigned[16] to a unsigned[8] parameter of `takes8`"#],
        ),
        (
            "241_body",
            r#"module test_generic_body_unchecked;

fn bad<T>(x: T) -> T {
    // None of these methods, operators, or indices exist or are bounded:
    x.this_method_does_not_exist();
    let y = x + 99999;
    let z = x[1234];
    return x;
}

#[test]
entity Top {}

impl Top {
    p: process {
        let n: integer = bad(42);
        print!("n = {}\n", n);
    }
}
"#,
            &[r#"`integer` has no method `this_method_does_not_exist`"#],
        ),
        (
            "241_bounds",
            r#"module test_rem_incomplete_bounds;

use std::ops::*;

struct MyVal {
    pub v: integer
}

impl Div<MyVal, MyVal> for MyVal {
    fn div(self, rhs: MyVal) -> MyVal {
        return MyVal { .v = self.v / rhs.v };
    }
}

// Same signature as std::math::rem:
fn my_rem<T: Div<T, T>>(a: T, m: T) -> T {
    return a - (a / m) * m;
}

#[test]
entity Top {}

impl Top {
    p: process {
        let x: MyVal = MyVal { .v = 10 };
        let y: MyVal = MyVal { .v = 3 };
        let r: MyVal = my_rem(x, y); // Passes Stage 4, fails at runtime
    }
}
"#,
            &[
                r#"no `*` operator for `MyVal`"#,
                r#"no `-` operator for `MyVal`"#,
            ],
        ),
    ];
    for (issue, source, messages) in cases {
        let out = run(&format!("error_{issue}"), source);
        for message in *messages {
            assert!(
                out.contains(message),
                "#{issue}: missing `{message}`: {out}"
            );
        }
    }
}
