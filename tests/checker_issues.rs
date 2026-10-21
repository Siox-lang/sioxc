//! Programs the checker passed that then failed late — in code generation,
//! or as a test binary reporting "direct Process IR lowering is incomplete" —
//! or that it diagnosed wrongly. Each is named after its GitHub issue.

use std::process::Command;

/// Compile `source` (`--emit` as given, or `--test` and run it when `run`)
/// and return everything the compiler and the binary printed.
fn compile(name: &str, source: &str, args: &[&str], run: bool) -> String {
    let dir = std::env::temp_dir().join(format!("siox_checker_issues_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let binary = dir.join(name);
    let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    command.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")]);
    command.args(args);
    if run {
        command.arg("--test").arg("-o").arg(&binary);
    }
    let output = command.arg(&file).output().unwrap();
    let mut text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    if run && output.status.success() {
        let ran = Command::new(&binary).output().unwrap();
        text += &String::from_utf8_lossy(&ran.stdout);
        text += &String::from_utf8_lossy(&ran.stderr);
    }
    text
}

fn metadata(name: &str, source: &str) -> String {
    compile(name, source, &["--emit", "metadata"], false)
}

#[test]
fn issue_145_an_empty_match_statement_runs() {
    let out = compile(
        "empty_match",
        "module m;\n#[test] entity T {}\nimpl T { process { let x: integer = 5; match x {} await 1ns; } }\n",
        &[],
        true,
    );
    assert!(out.contains("1 passed"), "{out}");
}

#[test]
fn issues_146_149_157_type_widths_are_checked_everywhere() {
    for (name, ty, message) in [
        ("zero", "unsigned[0]", "a type width must be at least 1"),
        (
            "huge",
            "unsigned[18446744073709551615]",
            "exceeds the compiler layout maximum",
        ),
    ] {
        let out = metadata(
            name,
            &format!("module m;\nuse std::bits::unsigned;\nentity E {{ x: {ty} in, y: {ty} out }}\nimpl E {{ y = x; }}\n"),
        );
        assert!(out.contains(message), "{ty}: {out}");
    }
    let out = metadata(
        "negative_local",
        "module m;\n#[test] entity T {}\nimpl T { process { let x: integer[-5]; await 1ns; } }\n",
    );
    assert!(out.contains("a type width cannot be negative"), "{out}");
}

#[test]
fn issue_147_llvm_ir_rejects_unresolved_widths() {
    let out = compile(
        "generic_root",
        "module m;\nuse std::bits::unsigned;\nentity C<W: integer> { q: unsigned[W] out }\nimpl<W: integer> C<W> { q = 0; }\n",
        &["--emit", "llvm-ir"],
        false,
    );
    assert!(out.contains("has an unresolved width"), "{out}");
    assert!(!out.contains("ProcessValueId"), "{out}");
}

#[test]
fn issue_151_generic_arity_is_checked() {
    for (name, use_, message) in [
        (
            "too_few",
            "G2<integer>",
            "`G2` takes 2 generic argument(s), but 1 were given",
        ),
        (
            "too_many",
            "G1<integer, integer>",
            "`G1` takes 1 generic argument(s), but 2 were given",
        ),
    ] {
        let out = metadata(
            name,
            &format!(
                "module m;\nstruct G1<A> {{ pub a: A }}\nstruct G2<A, B> {{ pub a: A, pub b: B }}\n\
                 entity E {{ x: {use_} in }}\nimpl E {{}}\n"
            ),
        );
        assert!(out.contains(message), "{use_}: {out}");
    }
}

#[test]
fn issue_152_trait_impls_match_the_trait() {
    let out = metadata(
        "trait_impl",
        "module m;\ntrait Tr { fn f(self, x: integer) -> integer; fn g(self) -> integer; }\n\
         struct S { pub v: integer }\n\
         impl Tr for S { fn f(self) -> integer { return 0; } fn h(self) -> integer { return 1; } }\n",
    );
    assert!(
        out.contains("`Tr::f` takes 1 parameter(s), but this impl's takes 0"),
        "{out}"
    );
    assert!(out.contains("`h` is not a method of trait `Tr`"), "{out}");
    assert!(
        out.contains("missing `g`, which trait `Tr` requires"),
        "{out}"
    );
}

#[test]
fn issue_153_a_match_expression_must_be_exhaustive() {
    let enum_match = "module m;\nenum E { A, B, C }\n";
    let out = metadata(
        "expression",
        &format!("{enum_match}fn f(x: E) -> integer {{ return match x {{ E::A => 10, E::B => 20 }}; }}\n"),
    );
    assert!(
        out.contains("error[E-P039]") && out.contains("missing `C`"),
        "{out}"
    );
    // A statement may take no arm: it stays the W-P007 lint.
    let out = metadata(
        "statement",
        &format!(
            "{enum_match}entity X {{ e: E in, y: integer out }}\n\
             impl X {{ p: process {{ y = 0; match e {{ E::A => {{ y = 1; }} }} }} }}\n"
        ),
    );
    assert!(
        out.contains("warning[W-P007]") && !out.contains("E-P039"),
        "{out}"
    );
}

#[test]
fn issue_154_a_conflict_names_a_kernel_type() {
    let out = compile(
        "conflict",
        "module m;\nentity D { a: integer in, y: integer out }\nimpl D { y = a; }\n\
         #[test] entity T {}\nimpl T { let a: integer = 1; let b: integer = 0; let y: integer;\n\
         let x: D = { .a = a, .y = y }; let z: D = { .a = b, .y = y }; }\n",
        &[],
        true,
    );
    assert!(out.contains("`integer` needs an `impl Resolve`"), "{out}");
}

#[test]
fn issue_156_constant_cycles_are_reported() {
    for (name, constants) in [
        (
            "mutual",
            "const A: integer = B + 1;\nconst B: integer = A + 1;\n",
        ),
        ("self", "const A: integer = A + 1;\n"),
    ] {
        let out = metadata(
            name,
            &format!("module m;\n{constants}entity E {{ v: integer out }}\nimpl E {{ v = A; }}\n"),
        );
        assert!(
            out.contains("constant `A` is defined in terms of itself"),
            "{name}: {out}"
        );
        assert!(!out.contains("no value named"), "{name}: {out}");
    }
}

#[test]
fn issue_161_an_unrolled_accumulator_is_not_a_dead_assignment() {
    let out = compile(
        "accumulator",
        "module m;\nentity E { v: integer out }\n\
         impl E { process { let sum: integer = 0; for i in 0..3 { sum = sum + i; } v = sum; } }\n",
        &["-D", "warnings", "--emit", "metadata"],
        false,
    );
    assert!(out.contains("check ok") && !out.contains("W-P014"), "{out}");
}
