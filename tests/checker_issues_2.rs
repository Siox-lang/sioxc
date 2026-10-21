//! More programs the checker passed that then failed late, or that compiled
//! to a silently wrong design. Each is named after its GitHub issue.

use std::process::Command;

/// Compile `source` (`--test` and run it when `run`, else `--emit metadata`)
/// and return everything the compiler and the binary printed.
fn compile(name: &str, source: &str, run: bool) -> String {
    let dir = std::env::temp_dir().join(format!("siox_checker_issues_2_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let binary = dir.join(name);
    let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    command.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")]);
    if run {
        command.arg("--test").arg("-o").arg(&binary);
    } else {
        command.args(["--emit", "metadata"]);
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

const TB: &str = "#[test] entity T {}\nimpl T { process { ";

#[test]
fn issue_171_a_struct_neg_impl_is_found() {
    let out = compile(
        "struct_neg",
        &format!(
            "module m;\nstruct P {{ pub x: integer, }}\n\
             impl Neg<P> for P {{ fn neg(self) -> P {{ return P {{ .x = -self.x }}; }} }}\n\
             {TB}let p: P = P {{ .x = 10 }}; let q: P = -p; print!(\"q={{}}\\n\", q.x); }} }}\n"
        ),
        true,
    );
    assert!(out.contains("q=-10") && out.contains("1 passed"), "{out}");
}

#[test]
fn late_failures_are_diagnosed() {
    for (issue, source, message) in [
        (
            "172",
            format!("module m;\nuse std::bits::unsigned;\n{TB}let x: integer = 1; let y: unsigned[8] = resize(x, -5); }} }}\n"),
            "a width must be at least 1, not -5",
        ),
        (
            "174",
            "module m;\nuse std::bits::unsigned;\nfn f(x: integer) -> integer { return f(x + 1); }\n\
             const X: integer = f(1);\nentity H { o: unsigned[X] out, }\nimpl H {}\n"
                .to_string(),
            "the value of `X` could not be computed",
        ),
        (
            "177_struct",
            format!("module m;\nuse std::bits::unsigned;\nstruct P<W: integer> {{ pub d: unsigned[W], }}\n{TB}let p: P<-4>; }} }}\n"),
            "`P`'s `W` is a width, so it must be at least 1, not -4",
        ),
        (
            "177_entity",
            "module m;\nuse std::bits::unsigned;\nentity R<W: integer> { d: unsigned[W] in, q: unsigned[W] out, }\n\
             impl<W: integer> R<W> { q = d; }\n#[test] entity T {}\nimpl T { let dut: R<0> = {}; }\n"
                .to_string(),
            "`R`'s `W` is a width, so it must be at least 1, not 0",
        ),
        (
            "178",
            "module m;\ntype Tree<T> = (T, Tree<(T, T)>);\nentity Top { x: Tree<integer> in, }\n".to_string(),
            "alias `Tree` is defined in terms of itself",
        ),
        (
            "179",
            format!("module m;\nstruct N {{ pub next: (N, integer), }}\n{TB}let n: N; }} }}\n"),
            "contains itself, so it has no finite layout",
        ),
        (
            "180_tuple",
            format!("module m;\n{TB}let p: (integer, integer) = (integer, integer)(1, 2); }} }}\n"),
            "a tuple type cannot be called",
        ),
        (
            "180_array",
            format!("module m;\nuse std::bits::unsigned;\n{TB}let a: unsigned[8][2] = (unsigned[8][2])([1, 2]); }} }}\n"),
            "an array type cannot be called",
        ),
        (
            "183_local",
            format!("module m;\nuse std::bits::unsigned;\n{TB}let x: unsigned[10] = {{ unsigned[4](0xA), unsigned[8](0xBC) }}; }} }}\n"),
            "cannot initialize unsigned[10] with unsigned[12]",
        ),
        (
            "183_port",
            "module m;\nuse std::bits::unsigned;\nentity E { a: unsigned[4] in, b: unsigned[8] in, y: unsigned[10] out, }\n\
             impl E { y = { a, b }; }\n"
                .to_string(),
            "cannot assign unsigned[12] to unsigned[10]",
        ),
        (
            "183_empty",
            format!("module m;\nuse std::bits::unsigned;\n{TB}let x: unsigned[8] = {{}}; }} }}\n"),
            "an empty concatenation has no bits",
        ),
        (
            "184",
            "module m;\nenum S { Zero, One, Two = 1, }\nentity E { s: S out, }\nimpl E { s = S::One; }\n".to_string(),
            "`S::Two` and `S::One` both have the value 1",
        ),
        (
            "186",
            "module m;\nuse std::bits::unsigned;\n\
             struct P { pub x: unsigned[8], pub y: unsigned[8], }\n\
             struct V { pub vx: unsigned[8], pub vy: unsigned[8], }\n\
             entity Sub { p: P in, o: unsigned[8] out, }\nimpl Sub { o = p.x; }\n\
             entity Top { v: V in, o: unsigned[8] out, }\nimpl Top { let sub: Sub = { .p = v, .o = o }; }\n"
                .to_string(),
            "type mismatch on port `p`: the port is `P` but `v` is `V`",
        ),
    ] {
        let out = compile(&format!("issue_{issue}"), &source, false);
        assert!(out.contains(message), "#{issue}: {out}");
    }
}

#[test]
fn issue_169_a_tuple_pattern_matches_its_type() {
    let out = compile(
        "issue_169",
        &format!("module m;\n{TB}let (a, b): (real, integer, integer) = (1.0, 2, 3); }} }}\n"),
        false,
    );
    assert!(
        out.contains("this pattern binds 2 position(s), but its type has 3"),
        "{out}"
    );
}

#[test]
fn issue_175_polymorphic_recursion_is_an_error() {
    let out = compile(
        "issue_175",
        &format!(
            "module m;\nfn f<T>(x: T) -> integer {{ return f((x, x)); }}\n{TB}let n: integer = f(1); }} }}\n"
        ),
        true,
    );
    assert!(
        out.contains("calls itself with arguments of other types"),
        "{out}"
    );
}
