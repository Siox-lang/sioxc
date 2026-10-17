//! An array-typed `let` initializer other than a literal is folded, or
//! reported, as a struct one is (`struct_let_initializer.rs`).
//!
//! The scalar fold is reached through the local's own signal, and an array
//! local has signals only under `name[i]`, so a constant (`= K`), a call
//! returning one literal (`= table()`) and a read of another signal (`= v`)
//! all powered on at zero, in silence.

use std::process::Command;

fn compile(name: &str, declaration: &str, test: bool) -> (String, std::path::PathBuf) {
    let src = format!(
        "module m;\n\
         use std::bits::unsigned;\n\
         const K: unsigned[3][3] = [1, 2, 6];\n\
         fn table() -> unsigned[3][3] {{ return [1, 2, 6]; }}\n\
         fn scaled(n: integer) -> unsigned[3][3] {{ return [n, n + 1, 2 * n]; }}\n\
         entity E {{ c: unsigned[3] out }}\n\
         impl E {{ let v: unsigned[3][3] = [1, 2, 6]; {declaration} c = w[2]; }}\n\
         #[test] entity T {{}}\n\
         impl T {{ let dut: E = {{}}; check: process {{ await 1ns; print!(\"|{{}}\\n\", dut.c); }} }}\n"
    );
    let dir = std::env::temp_dir().join(format!("siox_arrayinit_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, src).unwrap();
    let binary = dir.join(name);
    let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    command.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")]);
    if test {
        command.arg("--test").arg("-o").arg(&binary);
    } else {
        command.args(["--emit", "metadata"]);
    }
    let out = command.arg(&file).output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
        binary,
    )
}

#[test]
fn constant_array_initializers_fold() {
    for (name, value) in [
        ("constant", "K"),
        ("literal_call", "table()"),
        ("argument_call", "scaled(3)"),
    ] {
        let (out, binary) = compile(name, &format!("let w: unsigned[3][3] = {value};"), true);
        assert!(!out.contains("error"), "{value}: {out}");
        let report = Command::new(&binary).output().unwrap();
        assert!(
            String::from_utf8_lossy(&report.stdout).contains("|6\n"),
            "{value}: {}",
            String::from_utf8_lossy(&report.stdout)
        );
    }
}

#[test]
fn a_copy_from_another_array_is_reported() {
    let (out, _) = compile("copy", "let w: unsigned[3][3] = v;", false);
    assert!(
        out.contains("E-P021") && out.contains("the initializer for `w` is not a constant"),
        "{out}"
    );
}
