//! `#[derive(Eq, Ord)]` (derive proposal): field-by-field comparison impls,
//! `Ord` lexicographic in declared order, written into the struct's module.

use std::process::Command;

fn sioxc(name: &str, source: &str, test: bool) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("siox_derive_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let out = dir.join(name);
    let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    command.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")]);
    if test {
        command.arg("--test");
    }
    let output = command.arg("-o").arg(&out).arg(&file).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (out, text)
}

fn run(name: &str, source: &str) -> String {
    let (binary, built) = sioxc(name, source, true);
    let ran = Command::new(&binary)
        .output()
        .unwrap_or_else(|_| panic!("{name}: {built}"));
    String::from_utf8_lossy(&ran.stdout).to_string()
}

/// `==`, `<`, `>=` on a derived struct, in hardware and testbench, also
/// after an input changes.
#[test]
fn derived_comparisons_run() {
    let out = run(
        "version",
        r#"module d;
use std::bits::unsigned;
#[derive(Eq, Ord)]
struct Version { pub major: unsigned[4], pub minor: unsigned[4], pub patch: unsigned[8] }
#[derive(Eq)]
struct Flags { pub a: Bit, pub b: Logic }
entity Cmp { x: Version in, y: Version in, eq: Bool out, lt: Bool out, ge: Bool out }
impl Cmp { eq = x == y; lt = x < y; ge = x >= y; }
#[test] entity T {}
impl T {
    let x: Version = Version { .major = 1, .minor = 2, .patch = 3 };
    let y: Version = Version { .major = 1, .minor = 3, .patch = 0 };
    let eq: Bool; let lt: Bool; let ge: Bool;
    let c: Cmp = { .x = x, .y = y, .eq = eq, .lt = lt, .ge = ge };
    process {
        await 1ns;
        let f: Flags = Flags { .a = '1', .b = '0' };
        let g: Flags = Flags { .a = '1', .b = '0' };
        print!("HW {} {} {} TB {} {} {} {} {}\n", eq, lt, ge, x == y, x < y, x <= x, y > x, f == g);
        assert!(not eq and lt and not ge and x < y and x <= x and y > x and f == g and not (f != g));
        y = x; await 1ns;
        assert!(eq and not lt and ge);
    }
}
"#,
    );
    assert!(
        out.contains("HW false true false TB false true true true true")
            && out.contains("1 passed"),
        "{out}"
    );
}

/// A derived struct inside another compares through its own derived impl.
#[test]
fn derived_impls_nest() {
    let out = run(
        "nested",
        r#"module d4;
use std::bits::unsigned;
#[derive(Eq, Ord)]
struct Inner { pub a: unsigned[4] }
#[derive(Eq, Ord)]
struct Outer { pub i: Inner, pub b: unsigned[4] }
#[test] entity T {}
impl T { process {
    let x: Outer = Outer { .i = Inner { .a = 1 }, .b = 9 };
    let y: Outer = Outer { .i = Inner { .a = 2 }, .b = 0 };
    print!("{} {} {}\n", x == y, x < y, y >= x);
    assert!(not (x == y) and x < y and y >= x);
} }
"#,
    );
    assert!(
        out.contains("false true true") && out.contains("1 passed"),
        "{out}"
    );
}

/// Generic structs derive for every instance, and a newtype compares as its
/// base (`struct Count(integer)` had no `==` of its own).
#[test]
fn generic_and_newtype_structs_derive() {
    let generic = run(
        "generic",
        r#"module g1;
use std::bits::unsigned;
#[derive(Eq, Ord)]
struct Pair<A, B> { pub a: A, pub b: B }
#[derive(Eq)]
struct Word<W: integer> { pub v: unsigned[W] }
#[test] entity T {}
impl T { process {
    let x: Pair<unsigned[4], Bit> = Pair { .a = 3, .b = '1' };
    let y: Pair<unsigned[4], Bit> = Pair { .a = 3, .b = '0' };
    let p: Word<8> = Word { .v = 200 };
    let q: Word<8> = Word { .v = 200 };
    print!("{} {} {} {}\n", x == y, y < x, x >= y, p == q);
    assert!(not (x == y) and y < x and x >= y and p == q);
} }
"#,
    );
    assert!(
        generic.contains("false true true true") && generic.contains("1 passed"),
        "{generic}"
    );
    let newtype = run(
        "newtype",
        r#"module n2;
use std::bits::unsigned;
#[derive(Eq, Ord)]
struct Count(integer);
#[test] entity T {}
impl T { process {
    let c: Count = Count(3); let d: Count = Count(4);
    print!("{} {} {}\n", c == d, c < d, d >= c);
    assert!(not (c == d) and c < d and d >= c);
} }
"#,
    );
    assert!(
        newtype.contains("false true true") && newtype.contains("1 passed"),
        "{newtype}"
    );
}

/// Only `Eq` and `Ord` derive, and not yet for a generic newtype (E-P040).
#[test]
fn unsupported_derives_are_reported() {
    let (_, out) = sioxc(
        "unsupported",
        r#"module d2;
#[derive(Debug)]
struct A { pub x: integer }
#[derive(Eq)]
struct Tagged<W: integer>(integer);
"#,
        false,
    );
    assert!(
        out.contains("E-P040") && out.contains("cannot derive `Debug`"),
        "{out}"
    );
    assert!(
        out.contains("a generic newtype does not derive yet"),
        "{out}"
    );
}

/// A field without `==` is an error in the generated impl, which names the
/// derive it came from.
#[test]
fn an_error_in_a_derived_impl_names_the_derive() {
    let (_, out) = sioxc(
        "no_eq",
        r#"module d5;
struct NoEq { pub v: integer }
#[derive(Eq)]
struct Holder { pub n: NoEq }
"#,
        false,
    );
    assert!(out.contains("<derive Eq for Holder>"), "{out}");
    assert!(
        out.contains("that `#[derive(Eq)]` writes for `Holder`"),
        "{out}"
    );
}
