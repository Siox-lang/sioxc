//! Programs that were accepted, or rejected, wrongly:
//!
//! - An entity instantiated inside a `process` was accepted and never
//!   elaborated (a `match` arm and a function already were E-P020).
//! - `[1, 20]` into `unsigned[4][2]` stored 4: an array literal's elements
//!   skipped the constant-fit check a single value gets.
//! - `p = { .x = 3 }` warned that `y` keeps its default (W-P016), then failed
//!   with E-P017; the missing field now takes its default.
//! - A register clocked from two processes was accepted, last-wins (E-P014
//!   held only for combinational drivers): disjoint slices of an `unsigned`
//!   clobbered each other's half.

use std::process::Command;

fn compile(name: &str, source: &str, test: bool) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_wrong_programs_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    let binary = dir.join(name);
    std::fs::write(&file, source).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    command.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")]);
    if test {
        command.arg("--test").arg(&file).arg("-o").arg(&binary);
    } else {
        command.args(["--emit", "metadata"]).arg(&file);
    }
    let output = command.output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text, binary)
}

#[test]
fn an_entity_cannot_be_instantiated_in_a_process() {
    let (ok, text, _) = compile(
        "process",
        "module m;\nuse std::logic::Bit;\nentity A { a: Bit in }\nimpl A {}\n\
         entity E { a: Bit in }\nimpl E { p: process { let i: A = { .a = a }; } }\n",
        false,
    );
    assert!(
        !ok && text.contains("an entity cannot be instantiated in a process"),
        "{text}"
    );
}

#[test]
fn array_literal_elements_must_fit() {
    let program = |values: &str| {
        format!(
            "module m;\nuse std::bits::unsigned;\nentity E {{ y: unsigned[4][2] out }}\n\
             impl E {{ y = [{values}]; }}\n"
        )
    };
    let (ok, text, _) = compile("unfit", &program("1, 20"), false);
    assert!(
        !ok && text.contains("`20` does not fit in a 4-bit vector"),
        "{text}"
    );
    let (ok, text, _) = compile("fit", &program("1, 15"), false);
    assert!(ok, "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_partial_struct_literal_defaults_the_rest() {
    let (ok, text, binary) = compile(
        "partial",
        "module m;\nuse std::bits::unsigned;\nuse std::logic::Bit;\nenum K { A = 5, B = 9 }\n\
         struct P { pub x: unsigned[4], pub y: Bit, pub k: K }\n\
         entity E { p: P out }\nimpl E { p = { .x = 3 }; }\n\
         #[test] entity T {}\nimpl T { let p: P; let e: E = { .p = p }; \
         check: process { await 1ns; print!(\"|{} {} {:?}\\n\", p.x, p.y, p.k); } }\n",
        true,
    );
    assert!(ok, "{text}");
    let output = Command::new(&binary).output().unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("|3 '0' A\n"), "{report}");
}

#[test]
fn a_register_clocked_from_two_processes_conflicts() {
    for (ty, help) in [
        ("Bit", "only one source may drive"),
        ("unsigned[8]", "do not resolve across"),
    ] {
        let body = if ty == "Bit" {
            "pa: process { if clk.rising() { v = a; } }\npb: process { if clk.rising() { v = a; } }"
        } else {
            "pa: process { if clk.rising() { v[3..0] = 1; } }\npb: process { if clk.rising() { v[7..4] = 2; } }"
        };
        let (ok, text, _) = compile(
            "clocked",
            &format!(
                "module m;\nuse std::logic::Bit;\nuse std::bits::unsigned;\n\
                 entity E {{ clk: Bit in, a: Bit in, s: {ty} out }}\n\
                 impl E {{ let v: {ty};\n{body}\ns = v; }}\n"
            ),
            false,
        );
        assert!(
            !ok && text.contains("E-P014")
                && text.contains("is written by 2 processes")
                && text.contains(help),
            "{ty}: {text}"
        );
    }
}
