//! A constant bit index is not a conditional write.
//!
//! Packed bit writes route through the runtime-index expansion, which builds
//! one guarded update per candidate position. A *constant* index has a single
//! candidate and a `hit` of `Const(1)`, but that was still handed to the update
//! as `cond: Some(Const(1))` — a conditional driver as far as everything
//! downstream is concerned. Two consequences, one cosmetic and one not:
//!
//!   * `word[1] = '1';` at an entity's root drew an inferred-latch warning
//!     (W-P002), advising an unconditional default for a write that already is
//!     one; and
//!   * nothing could find an unconditional driver to merge the next partial
//!     write over, so `word[1] = '1'; word[3] = '1';` produced 8 rather than
//!     10. That half is covered by the corpus regression
//!     `packed_partial_write_test.siox`; the warning is covered here, because a
//!     diagnostic that is absent cannot be asserted from a running testbench.
//!
//! The negative control matters as much: a write that really is conditional
//! must keep warning, or this has simply switched the lint off.

use std::process::Command;

fn diagnostics(name: &str, body: &str) -> String {
    let src = format!(
        "module m;\n\
         using std::bits::{{unsigned}};\n\
         using std::logic::{{Bit}};\n\
         entity E {{ c: Bit in, y: unsigned[8] out }}\n\
         impl E {{\n{body}\n}}\n"
    );
    let dir = std::env::temp_dir().join(format!("siox_partial_{}", std::process::id()));
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
fn a_constant_bit_write_is_not_an_inferred_latch() {
    let out = diagnostics(
        "constbit",
        "    let word: unsigned[8] = 0;\n\
         \x20   word[1] = '1';\n\
         \x20   word[3] = '1';\n\
         \x20   y = word;",
    );
    assert!(
        !out.contains("W-P002"),
        "a constant bit write is unconditional, got:\n{out}"
    );
}

#[test]
fn a_conditional_bit_write_still_warns() {
    // The control: this one genuinely holds its value when `c` is low, and the
    // lint has to keep saying so.
    let out = diagnostics(
        "condbit",
        "    let word: unsigned[8] = 0;\n\
         \x20   if c == '1' { word[1] = '1'; }\n\
         \x20   y = word;",
    );
    assert!(
        out.contains("W-P002"),
        "a conditional bit write is still a latch, got:\n{out}"
    );
}

#[test]
fn a_runtime_bit_write_still_warns() {
    // A runtime index leaves the signal undriven whenever the index is out of
    // range, so it is a latch too.
    let out = diagnostics(
        "runbit",
        "    let idx: unsigned[8] = 1;\n\
         \x20   let word: unsigned[8] = 0;\n\
         \x20   word[idx] = '1';\n\
         \x20   y = word;",
    );
    assert!(
        out.contains("W-P002"),
        "a runtime bit write can leave the signal undriven, got:\n{out}"
    );
}
