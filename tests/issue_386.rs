//! Regressions for sioxc #386 (the parts that do not need generic struct
//! arguments in `Ty`): a function generic only over values (`<W: integer>`)
//! has its body checked, its `-> unsigned[W]` result takes `W` from the
//! argument, and a bound or a consistent type is checked for `T` inside a
//! tuple parameter.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i386_{name}_{}", std::process::id()));
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
fn a_value_generic_body_and_its_result_width_are_checked() {
    let (ok, text, _) = compile("bad", BAD);
    assert!(!ok, "{text}");
    assert!(
        text.contains("cannot initialize Bit with Char[25]"),
        "{text}"
    );
    assert!(
        text.contains("cannot initialize unsigned[4] with unsigned[8]"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_value_generic_function_runs_at_each_width() {
    let (ok, text, binary) = compile("good", GOOD);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[test]
fn a_type_parameter_inside_a_tuple_is_bounded_and_consistent() {
    let (ok, text, _) = compile("tuple", TUPLE);
    assert!(!ok, "{text}");
    assert!(
        text.contains("`NoOrd` does not satisfy the bound `T: Ord`"),
        "{text}"
    );
    assert!(
        text.contains("inferred as unsigned[8], but this argument gives it unsigned[16]"),
        "{text}"
    );
}

const BAD: &str = r#"module t386_bad;

fn id_w<W: integer>(x: unsigned[W]) -> unsigned[W] {
    let also_broken: Bit = "also completely unchecked";
    return x;
}

fn same<W: integer>(x: unsigned[W]) -> unsigned[W] {
    return x;
}

#[test]
entity Tb {}

impl Tb {
    process {
        let x: unsigned[4] = same(unsigned[8](42));
        print!("x={}", x);
    }
}
"#;

const GOOD: &str = r#"module t386_good;

fn twice<W: integer>(x: unsigned[W]) -> unsigned[W] {
    let doubled: unsigned[W] = x + x;
    return doubled;
}

#[test]
entity Tb {}

impl Tb {
    process {
        let a: unsigned[8] = twice(unsigned[8](21));
        let b: unsigned[4] = twice(unsigned[4](3));
        assert!(a == 42 and b == 6, "a={} b={}", a, b);
    }
}
"#;

const TUPLE: &str = r#"module tb;
use std::sim::*;

struct NoOrd {
    pub x: unsigned[8],
}

fn check_pair<T: Ord>(p: (T, T)) -> T {
    return p.0;
}

#[test]
entity Tb {}

impl Tb {
    process {
        let a: NoOrd = { .x = 1 };
        let r1: NoOrd = check_pair((a, a));
        let r2: unsigned[8] = check_pair((unsigned[8](1), unsigned[16](99)));
        print!("r1={} r2={}", r1.x, r2);
        stop();
    }
}
"#;
