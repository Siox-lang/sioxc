//! Inside a member with a receiver, a bare field name is `self.<field>`:
//! reads, writes, a newtype's base fields, and in hardware as in a testbench.
//! Parameters and locals shadow fields; a `static fn` has no receiver and
//! sees none.

use std::process::Command;

fn sioxc(name: &str, source: &str, args: &[&str]) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_bare_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(args)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text, dir)
}

#[cfg(feature = "llvm")]
#[test]
fn bare_field_names_read_and_write_the_receiver() {
    let (ok, text, dir) = sioxc("run", GOOD, &["--test", "-o", "bare.bin"]);
    assert!(ok, "{text}");
    let run = Command::new(dir.join("bare.bin")).output().unwrap();
    let text =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{text}");
    assert!(text.contains("bare fields done"), "{text}");
}

#[test]
fn a_static_fn_sees_no_fields() {
    let (ok, text, _) = sioxc(
        "static",
        "module m;\nstruct C { pub value: integer }\nimpl C {\n    pub static fn peek() -> integer { return value; }\n}\n",
        &["--emit", "metadata"],
    );
    assert!(!ok, "{text}");
    assert!(text.contains("unknown value `value`"), "{text}");
}

const GOOD: &str = r#"module probe;

struct Counter { pub value: integer, pub step: integer }

impl Counter {
    pub fn next() -> integer { return value + step; }            // bare reads
    pub fn scaled(step: integer) -> integer { return value * step; } // param shadows
    pub fn same(other: Counter) -> Bool { return value == other.value; }
    pub fn explicit() -> integer { return self.value; }
    pub fn bump() { value = value + step; }                     // a bare write
    pub static fn at(value: integer) -> Counter { return Counter { .value = value, .step = 1 }; }
}

struct Base { pub a: integer }
struct Wrapped(Base);
impl Wrapped {
    pub fn doubled() -> integer { return a * 2; }               // the base's field
}

entity Hw { y: integer out }
impl Hw {
    let c: Counter = Counter { .value = 10, .step = 3 };
    y = c.next();
}

#[test]
entity Bare {}

impl Bare {
    let y: integer;
    let hw: Hw = { .y = y };
    check: process {
        let c: Counter = Counter { .value = 10, .step = 3 };
        assert!(c.next() == 13, "value + step");
        assert!(c.scaled(5) == 50, "the parameter shadows the field");
        assert!(c.same(Counter::at(10)), "other.value next to a bare value");
        assert!(c.explicit() == 10, "self.value still works");
        let w: Wrapped = Wrapped(Base { .a = 4 });
        assert!(w.doubled() == 8, "a field of the newtype's base");
        let m: Counter = Counter { .value = 1, .step = 2 };
        m.bump();
        m.bump();
        assert!(m.value == 5, "two bare writes reached the receiver");
        assert!(y == 13, "a bare-field method in hardware");
        print!("bare fields done");
    }
}
"#;
