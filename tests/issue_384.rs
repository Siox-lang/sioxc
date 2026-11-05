//! Regressions for sioxc #384: testbench-only operations reached from
//! hardware through a method call, `stop()`/`finish()`, or a helper
//! function's stimulus are errors, not silently dropped.

use std::process::Command;

fn errors(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_i384_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--emit",
            "ir",
        ])
        .arg(&file)
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected errors");
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn a_method_reaching_a_testbench_operation_is_an_error() {
    let text = errors("methods", I384_METHODS);
    for (call, op) in [
        ("c.next_with_static", "static let"),
        ("c.spin_while", "while"),
        ("c.pick_random", "randint"),
    ] {
        assert!(text.contains(&format!("`{call}` reaches `{op}`")), "{text}");
    }
}

#[test]
fn stop_finish_and_a_checking_helper_are_testbench_only() {
    let text = errors("stop", I384_STOP);
    assert!(
        text.contains("`stop` is only available in a testbench"),
        "{text}"
    );
    assert!(
        text.contains("`finish` is only available in a testbench"),
        "{text}"
    );
    assert!(text.contains("`check_it` reaches `stop`"), "{text}");
}

const I384_METHODS: &str = r#"module test_hw_reaches_method;

pub struct Counter {
    pub v: unsigned[8],
}

impl Counter {
    pub fn next_with_static() -> unsigned[8] {
        static let s: unsigned[8] = 1;
        s = s + 1;
        return self.v + s;
    }

    pub fn spin_while() -> unsigned[8] {
        let k: unsigned[8] = self.v;
        while k > 0 {
            k = k - 1;
        }
        return k;
    }

    pub fn pick_random() -> integer {
        return randint(0, 10);
    }
}

entity Top {
    y1: unsigned[8] out,
    y2: unsigned[8] out,
    y3: integer out,
}

impl Top {
    let c: Counter = { .v = 5 };

    drive: process {
        y1 = c.next_with_static();
        y2 = c.spin_while();
        y3 = c.pick_random();
    }
}
"#;

const I384_STOP: &str = r#"module test_hw_stmt_call_stimulus;

fn check_it(x: Bit) {
    assert!(x == '0', "must be zero");
    print!("checked");
    stop();
}

entity Top {
    a: Bit in,
    y: Bit out,
}

impl Top {
    p: process {
        y = a;
        check_it(a);
        stop();
        finish();
    }
}
"#;
