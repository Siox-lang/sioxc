//! Testbench programs the checker passed and the native test then refused
//! ("direct Process IR lowering is incomplete"), and trait impls the
//! checker accepted for targets that cannot hold one. Each is named after
//! its GitHub issue.

use std::process::Command;

fn compile(name: &str, source: &str, run: bool) -> String {
    let dir = std::env::temp_dir().join(format!("siox_testbench_issues_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source.replace("$DIR", &dir.display().to_string())).unwrap();
    std::fs::write(dir.join("text.txt"), "wörld").unwrap();
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

#[test]
fn issues_155_158_159_testbench_values_lower() {
    let out = compile(
        "values",
        r#"module m;
trait Scale { fn scale(self, k: integer) -> integer; }
struct S { pub v: integer }
impl Scale for S { fn scale(self, k: integer) -> integer { return self.v * k; } }
#[test] entity T {}
impl T {
    process {
        let t: (integer,) = (42,);
        let arr: integer[4] = [10, 20, 30, 40];
        let sub: integer[2] = arr[1..2];
        let s: S = { .v = 7 };
        print!("|{} {} {} {}", t.0, sub[0], sub[1], Scale::scale(s, 3));
    }
}
"#,
        true,
    );
    // #155 a 1-tuple, #158 a slice of `integer[4]`, #159 `Trait::method(s)`.
    assert!(out.contains("|42 20 30 21"), "{out}");
}

#[test]
fn issue_160_strings_slice() {
    let out = compile(
        "strings",
        r#"module m;
#[test] entity T {}
impl T {
    process {
        let s: Char[] = "héllo";
        let r: Char[] = read<Char[]>("$DIR/text.txt");
        let c: Char[] = r[1..3];
        print!("|{} {} {} {}", s[1..3], s[2..], s[3..1], s[..1]);
        print!("|{} {} {} {} {}", c, c'length, r[2..], r[3..1], r[..1]);
        print!("|{}", r[1..9]);
    }
}
"#,
        true,
    );
    // A fixed `Char[5]` and a runtime string read from a file; a
    // descending range reverses, as a fixed array's slice does.
    assert!(out.contains("|éll llo llé hé"), "{out}");
    assert!(out.contains("|örl 3 rld lrö wö"), "{out}");
    assert!(
        out.contains("slice 1..9 is outside declared range 0..4"),
        "{out}"
    );
}

#[test]
fn issues_150_162_structural_trait_targets_are_rejected_at_the_impl() {
    for (name, target) in [("tuple", "(integer, integer)"), ("array", "integer[2]")] {
        let out = compile(
            name,
            &format!(
                "module m;\nuse std::ops::Add;\n\
                 impl Add<{target}, {target}> for {target} {{ fn add(self, rhs: {target}) -> {target} {{ return self; }} }}\n"
            ),
            false,
        );
        assert!(
            out.contains(&format!("a trait cannot be implemented for `{target}`"))
                && out.contains("wrap the value in a struct"),
            "{target}: {out}"
        );
    }
}
