//! Regressions for sioxc #575, #576 and #577: generic argument counts are
//! checked both ways. A bare generic struct as a declared type (`let b:
//! Box;`, `b: Box in`) takes its arguments, a struct, enum or entity
//! declaring no parameters takes none (`Point<integer, Bool, 123>`), and
//! an impl's trait is counted and its unknown types reported. Each passed
//! and failed later or not at all.

use std::process::Command;

fn diagnostics(name: &str, source: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("siox_i575_{name}_{}", std::process::id()));
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
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

#[test]
fn a_bare_generic_struct_needs_its_arguments() {
    for (name, source) in [("port", PORT), ("local", LOCAL)] {
        let (ok, text) = diagnostics(name, source);
        assert!(!ok, "{text}");
        assert!(
            text.contains("`Box` takes 1 generic argument(s), but 0 were given"),
            "{text}"
        );
    }
}

#[test]
fn a_non_generic_type_takes_no_arguments() {
    let (ok, text) = diagnostics("extra", EXTRA);
    assert!(!ok, "{text}");
    for expected in [
        "`Point` takes 0 generic argument(s), but 3 were given",
        "`Color` takes 0 generic argument(s), but 2 were given",
        "`Sub` takes 0 generic argument(s), but 3 were given",
    ] {
        assert!(text.contains(expected), "{text}");
    }
}

#[test]
fn an_impls_trait_arguments_are_counted() {
    let (ok, text) = diagnostics("traits", TRAITS);
    assert!(!ok, "{text}");
    for expected in [
        "`NoParams` takes 0 generic argument(s), but 2 were given",
        "`OneParam` takes 1 generic argument(s), but 0 were given",
        "unknown type `NonExistentType123`",
    ] {
        assert!(text.contains(expected), "{text}");
    }
}

const TRAITS: &str = r#"module test_trait_impl_args;

trait NoParams {
    fn action_a();
}

trait OneParam<T> {
    fn action_b();
}

pub struct Dummy {
    pub x: integer,
}

// Case 1: Trait takes 0 parameters, but impl supplies 2
impl NoParams<integer, Bool> for Dummy {
    fn action_a() {}
}

// Case 2: Trait takes 1 parameter, but impl supplies 0
impl OneParam for Dummy {
    fn action_b() {}
}

// Case 3: Bogus nonexistent types and invalid expressions
impl NoParams<NonExistentType123, (1 / 0)> for Dummy {
    fn action_a() {}
}

entity Dut {
    d: Dummy in,
}
"#;

const PORT: &str = r#"module test_bare_generic_hw;

pub struct Box<T> {
    pub val: T,
}

entity Dut {
    b: Box in,
    y: integer out,
}

impl Dut {
    y = b.val;
}
"#;

const LOCAL: &str = r#"module test_bare_generic_sim;

pub struct Box<T> {
    pub val: T,
}

#[test]
entity Testbench {}

impl Testbench {
    run: process {
        let b: Box;
        b.val = 42;
    }
}
"#;

const EXTRA: &str = r#"module test_nongeneric_args;

pub struct Point {
    pub x: integer,
}

pub enum Color {
    Red,
    Green,
}

entity Sub {}

entity Top {
    s: Sub<1, 2, 3>,
}

#[test]
entity Testbench {}

impl Testbench {
    run: process {
        let p: Point<integer, Bool, 123>;
        let c: Color<1, 2>;
    }
}
"#;
