//! A binary operator with no impl for its right operand is an error.
//!
//! `inline_op` returning `None` normally means "fall back to builtin
//! arithmetic on the packed word", which is right for a vector family. For an
//! *aggregate* struct there is nothing to fall back to: the expression yields
//! no fields, the assignment it feeds is dropped, and the only trace is a
//! downstream "signal never driven" warning naming the destination rather than
//! the operator.
//!
//! The shape that finds it:
//!
//! ```siox
//! impl Operator<"*", unsigned[8], Vec2> for Vec2 { .. }
//! s = a * 3;      // silently nothing — 3 is `integer`, the impl wants `unsigned`
//! s = a * unsigned[8](3);   // fine
//! ```
//!
//! Overload selection matches the declared `Rhs` exactly, and retries a bare
//! `integer` literal only against a `Self`-typed `Rhs`. Whether it *should*
//! also coerce a literal to a vector-family `Rhs` is a language question; this
//! only makes the failure visible instead of silent.

use std::process::Command;

const PRELUDE: &str = "module m;\n\
     using std::bits::{unsigned};\n\
     using std::logic::{Logic};\n\
     using std::ops::{Operator};\n\
     struct Vec2 { x: unsigned[8], y: unsigned[8] }\n\
     impl Operator<\"*\", unsigned[8], Vec2> for Vec2 {\n\
         fn apply(self, rhs: unsigned[8]) -> Vec2 { return Vec2 { .x = self.x * rhs, .y = self.y * rhs }; }\n\
     }\n\
     impl Operator<\"+\", Vec2, Vec2> for Vec2 {\n\
         fn apply(self, rhs: Vec2) -> Vec2 { return Vec2 { .x = self.x + rhs.x, .y = self.y + rhs.y }; }\n\
     }\n\
     struct Q(unsigned[8]);\n\
     impl Operator<\"*\", unsigned[8], Q> for Q {\n\
         fn apply(self, rhs: unsigned[8]) -> Q { return Q(unsigned[8](self) * rhs); }\n\
     }\n";

fn diagnostics(name: &str, body: &str) -> String {
    let src = format!("{PRELUDE}entity E {{ y: unsigned[8] out }}\nimpl E {{ {body} }}\n");
    let dir = std::env::temp_dir().join(format!("siox_op_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(["--emit", "metadata"])
        .arg(&file)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn an_unmatched_right_operand_on_a_struct_is_reported() {
    let out = diagnostics(
        "bad",
        "let a: Vec2; a = Vec2 { .x = 3, .y = 5 }; let s: Vec2; s = a * 3; y = s.x + s.y;",
    );
    assert!(
        out.contains("no `*` operator for `Vec2` with a right operand of type `integer`"),
        "expected an operator error, got:\n{out}"
    );
    assert!(out.contains("E-P003"), "and a stable code, got:\n{out}");
}

#[test]
fn the_declared_right_operand_type_still_dispatches() {
    // The same expression with the literal converted: this is what the help
    // tells the author to write, so it had better work.
    let out = diagnostics(
        "typed",
        "let a: Vec2; a = Vec2 { .x = 3, .y = 5 }; let s: Vec2; s = a * unsigned[8](3); y = s.x + s.y;",
    );
    assert!(
        !out.contains("E-P003"),
        "an exactly-typed right operand must dispatch, got:\n{out}"
    );
}

#[test]
fn a_self_typed_right_operand_still_dispatches() {
    let out = diagnostics(
        "selfrhs",
        "let a: Vec2; a = Vec2 { .x = 3, .y = 5 }; let s: Vec2; s = a + a; y = s.x + s.y;",
    );
    assert!(
        !out.contains("E-P003"),
        "a Self-typed right operand must dispatch, got:\n{out}"
    );
}

#[test]
fn a_nominal_array_newtype_falls_back_to_builtin_arithmetic() {
    // `struct Q(unsigned[8])` joins the array families, so `q * 3` is packed
    // arithmetic on the word and needs no impl at all. Reporting here would
    // reject a whole class of working designs — this is why the check is
    // restricted to aggregates.
    let out = diagnostics(
        "newtype",
        "let q: Q; q = Q(4); let s: Q; s = q * 3; y = unsigned[8](s);",
    );
    assert!(
        !out.contains("E-P003"),
        "a newtype over a vector must keep builtin arithmetic, got:\n{out}"
    );
}

#[test]
fn plain_vector_arithmetic_is_untouched() {
    let out = diagnostics("plain", "let v: unsigned[8] = 4; y = v * 3;");
    assert!(
        !out.contains("E-P003"),
        "ordinary arithmetic must be untouched, got:\n{out}"
    );
}

#[test]
fn a_std_nominal_array_family_is_not_reported() {
    // `pub struct unsigned(Logic[])` is a struct, so testing only "the left
    // operand is a struct" reports on ordinary vector expressions. A field-less
    // newtype is one word with builtin arithmetic behind it; whether `u * l`
    // should be rejected for other reasons is not this check's business, and
    // it did not error before.
    let out = diagnostics(
        "family",
        "let u: unsigned[8] = 4; let g: Logic = '1'; y = u * g;",
    );
    assert!(
        !out.contains("no `*` operator"),
        "a std nominal array family must not be reported, got:\n{out}"
    );
}

#[test]
fn an_aggregate_struct_is_still_reported() {
    // A multi-field struct is many signals with no packed-word arithmetic
    // behind it. Excluding all structs from this check would leave the failed
    // operator dispatch silent again.
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               using std::ops::{Operator};\n\
               struct Vec2 { x: unsigned[8], y: unsigned[8] }\n\
               impl Operator<\"*\", unsigned[8], Vec2> for Vec2 {\n\
                   fn apply(self, rhs: unsigned[8]) -> Vec2 { return Vec2 { .x = self.x * rhs, .y = self.y * rhs }; }\n\
               }\n\
               entity E { y: unsigned[8] out }\n\
               impl E { let a: Vec2; a = Vec2 { .x = 3, .y = 5 }; let s: Vec2; s = a * 3; y = s.x + s.y; }\n";
    let dir = std::env::temp_dir().join(format!("siox_op_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("vecagg.siox");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(["--emit", "metadata"])
        .arg(&file)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("no `*` operator for `Vec2`"),
        "an aggregate struct must still be reported, got:\n{text}"
    );
}
