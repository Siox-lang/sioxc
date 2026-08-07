//! A struct-typed `let` initializer is folded, or reported.
//!
//! An initializer is a signal's power-on value, folded at elaboration. The
//! scalar path folds a call (`let a: unsigned[8] = double(6)` is 12) and
//! reports `E-P021` when it cannot fold one. The struct path did neither: the
//! block that folds is reached through the local's own signal, and a struct
//! local has signals only under `name.field`, so a struct initializer fell
//! through in silence and every field powered on at zero.
//!
//! The value half is covered by the corpus regression
//! `named_struct_literal_test.siox`. What belongs here is the half a running
//! testbench cannot assert: that an unfoldable body is *reported* rather than
//! quietly zeroed, and that the two shapes which must stay silent do.

use std::process::Command;

fn diagnostics(name: &str, body: &str) -> String {
    let src = format!(
        "module m;\n\
         using std::bits::{{unsigned}};\n\
         struct Pair {{ a: unsigned[8], b: unsigned[8] }}\n\
         fn make(v: unsigned[8]) -> Pair {{ return Pair {{ .a = v, .b = v + 1 }}; }}\n\
         fn branchy(v: unsigned[8]) -> Pair {{\n\
             if v == 0 {{ return Pair {{ .a = 1, .b = 1 }}; }}\n\
             return Pair {{ .a = 2, .b = 2 }};\n\
         }}\n\
         entity E {{ y: unsigned[8] out }}\n\
         impl E {{\n{body}\n}}\n"
    );
    let dir = std::env::temp_dir().join(format!("siox_structinit_{}", std::process::id()));
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
fn a_body_that_cannot_be_folded_is_reported() {
    // Two returns behind a condition: there is no single literal to fold, so
    // there is no power-on value. Saying so beats powering on at zero, and the
    // help names the spelling that works.
    let out = diagnostics("branchy", "    let p: Pair = branchy(1);\n    y = p.a;");
    assert!(
        out.contains("E-P021"),
        "an unfoldable struct initializer should be reported, got:\n{out}"
    );
    assert!(
        out.contains("the initializer for `p` is not a constant"),
        "and name the local, got:\n{out}"
    );
}

#[test]
fn a_foldable_call_is_not_reported() {
    // The control in the other direction: the fix must not have turned every
    // struct initializer into an error.
    let out = diagnostics("foldable", "    let p: Pair = make(6);\n    y = p.a;");
    assert!(
        !out.contains("E-P021"),
        "a foldable call must still be an initializer, got:\n{out}"
    );
}

#[test]
fn a_default_construction_stays_silent() {
    // `Pair::new()` and `Pair()` resolve to no declared function at all. Their
    // all-zero result is the structural default and is correct, so neither may
    // be mistaken for a body this cannot fold.
    for (name, body) in [
        ("newcall", "    let p: Pair = Pair::new();\n    y = p.a;"),
        ("tycall", "    let p: Pair = Pair();\n    y = p.a;"),
    ] {
        let out = diagnostics(name, body);
        assert!(
            !out.contains("E-P021"),
            "a default construction is not an unfoldable call, got:\n{out}"
        );
    }
}

#[test]
fn a_struct_literal_initializer_stays_silent() {
    // The long-standing spelling, which the new branch must not intercept.
    let out = diagnostics(
        "literal",
        "    let p: Pair = { .a = 6, .b = 7 };\n    y = p.a;",
    );
    assert!(
        !out.contains("E-P021"),
        "a struct literal is a constant initializer, got:\n{out}"
    );
}
