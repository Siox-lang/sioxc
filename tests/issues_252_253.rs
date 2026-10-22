//! Antigravity's issues #252–#253. Each case is named after its GitHub issue.

use std::process::Command;

/// Build `source` as a test binary and run it, returning everything printed.
fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_issues_252_{}", std::process::id()));
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
fn struct_literals_type_their_hardware_operands() {
    for (issue, source) in [
        (
            "tj253_0",
            r#"module test_method_on_lit_hw;

trait Area {
    fn width(self) -> integer;
}

struct Rect {
    pub w: integer,
}

impl Area for Rect {
    fn width(self) -> integer {
        return self.w;
    }
}

entity Top {
    w: integer in,
    d: integer out,
}
impl Top {
    d = Rect { .w = w }.width();
}
#[test] entity TB {}
impl TB { let w: integer = 7; let d: integer; let dut: Top = { .w = w, .d = d }; process { await 1ns; print!("v={}\n", d); } }
"#,
        ),
        (
            "tj253_1",
            r#"module test_op_on_lit_hw;

use std::ops::Add;

struct Complex {
    pub re: integer,
    pub im: integer,
}

impl Add<Complex, Complex> for Complex {
    fn add(self, rhs: Complex) -> Complex {
        return Complex {
            .re = self.re + rhs.re,
            .im = self.im + rhs.im,
        };
    }
}

entity Top {
    a: integer in,
    b: integer in,
    r: integer out,
}
impl Top {
    let res: Complex;
    res = Complex { .re = a, .im = 0 } + Complex { .re = b, .im = 0 };
    r = res.re;
}
#[test] entity TB {}
impl TB { let a: integer = 3; let b: integer = 4; let r: integer; let dut: Top = { .a = a, .b = b, .r = r }; process { await 1ns; print!("v={}\n", r); } }
"#,
        ),
        (
            "tj253_2",
            r#"module test_generic_struct_hw;

trait Area {
    fn width(self) -> integer;
}

struct Rect {
    pub w: integer,
}

impl Area for Rect {
    fn width(self) -> integer {
        return self.w;
    }
}

fn compute<T: Area>(item: T) -> integer {
    return item.width();
}

entity Top {
    w: integer in,
    d: integer out,
}
impl Top {
    d = compute(Rect { .w = w });
}
#[test] entity TB {}
impl TB { let w: integer = 7; let d: integer; let dut: Top = { .w = w, .d = d }; process { await 1ns; print!("v={}\n", d); } }
"#,
        ),
    ] {
        let out = run(issue, source);
        assert!(
            out.contains("v=7") && out.contains("1 passed"),
            "#253 {issue}: {out}"
        );
    }
}

#[test]
fn issue_252_runtime_intrinsics_are_testbench_only() {
    let out = run(
        "randint",
        r#"module repro_rand_hw;

entity Top {
    y_int: integer out,
}

impl Top {
    process {
        y_int = randint(0, 10);
    }
}
"#,
    );
    assert!(
        out.contains("`randint` is only available in a testbench"),
        "{out}"
    );
}
