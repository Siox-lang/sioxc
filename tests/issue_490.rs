//! Regression for sioxc #490: `impl Trait for Alias` (`type Pt = Point;`)
//! implements the aliased type, so method calls and trait bounds on a
//! `Point` find it, and `Pt::double(p)` is a qualified call of `Point`'s
//! method, reported for its receiver as `Point::double(p)` is.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i490_{name}_{}", std::process::id()));
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

#[cfg(feature = "llvm")]
#[test]
fn an_impl_for_an_alias_implements_the_type() {
    let (ok, text, binary) = compile("calls", CALLS);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[test]
fn a_qualified_call_through_the_alias_needs_its_receiver() {
    let (ok, text, _) = compile("qualified", QUALIFIED);
    assert!(!ok, "{text}");
    assert!(text.contains("Point.double` has a receiver"), "{text}");
}

const CALLS: &str = r#"module p490;

trait Doubler {
    fn double() -> integer;
}

struct Point { pub x: integer }
type Pt = Point;

// Accepted with 0 errors / warnings
impl Doubler for Pt {
    fn double() -> integer {
        return self.x * 2;
    }
}

fn call_doubler<T: Doubler>(v: T) -> integer {
    return 42;
}

#[test]
entity Top {}
impl Top {
    check: process {
        let p: Pt = Point { .x = 10 };

        // Error 1: Method call fails!
        // error[E-P008]: `test_alias_trait::Point` has no method `double`
        print!("double = {}\n", p.double());


        // Error 3: Trait bound fails!
        // error[E-P003]: `Point` does not satisfy the bound `T: Doubler`
        print!("bound = {}\n", call_doubler(p));
        assert!(p.double() == 20 and call_doubler(p) == 42, "alias impl");
    }
}
"#;

const QUALIFIED: &str = r#"module p490_2;

trait Doubler {
    fn double() -> integer;
}

struct Point { pub x: integer }
type Pt = Point;

// Accepted with 0 errors / warnings
impl Doubler for Pt {
    fn double() -> integer {
        return self.x * 2;
    }
}

fn call_doubler<T: Doubler>(v: T) -> integer {
    return 42;
}

#[test]
entity Top {}
impl Top {
    check: process {
        let p: Pt = Point { .x = 10 };
        let r: integer = Pt::double(p);
        print!("r={}\n", r);

        // Error 1: Method call fails!
        // error[E-P008]: `test_alias_trait::Point` has no method `double`

        // Error 2: Qualified call fails!
        // error[E-P008]: method `test_alias_trait::Pt.double` has a receiver
        //   = help: call it on a `test_alias_trait::Pt` value, e.g. `value.double(...)`

        // Error 3: Trait bound fails!
        // error[E-P003]: `Point` does not satisfy the bound `T: Doubler`
    }
}
"#;
