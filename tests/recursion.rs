//! A testbench call expands where it is made and has no frame, so a function
//! that reaches itself cannot be expanded. That is a compile-time error naming
//! the function and the call, not a run-time "lowering is incomplete".

#[cfg(feature = "llvm")]
fn compile(name: &str, source: &str) -> siox::compiler::Compilation {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_recursion_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join(format!("{name}.siox")), source),
            Emit::TestExecutable,
        )
        .with_output(dir.join(name)),
    )
}

#[cfg(feature = "llvm")]
#[test]
fn recursive_testbench_calls_are_compile_errors() {
    for (name, source, function) in [
        (
            "direct",
            "module r;\n\
             fn fact(n: integer) -> integer { if n <= 1 { return 1; } return n * fact(n - 1); }\n\
             #[test] entity T {}\n\
             impl T { check: process { let k: integer = 5; await 1ns; print!(\"{}\", fact(k)); } }\n",
            "fact",
        ),
        (
            "mutual",
            "module r;\n\
             fn even(n: integer) -> Bool { if n == 0 { return true; } return odd(n - 1); }\n\
             fn odd(n: integer) -> Bool { if n == 0 { return false; } return even(n - 1); }\n\
             #[test] entity T {}\n\
             impl T { check: process { let k: integer = 4; await 1ns; print!(\"{}\", even(k)); } }\n",
            "even",
        ),
    ] {
        let compilation = compile(name, source);
        let rendered = compilation.render_diagnostics();
        assert!(!compilation.succeeded(), "{name}: {rendered}");
        assert!(
            rendered.contains("E-P038") && rendered.contains(&format!("`{function}` calls itself")),
            "{name}: {rendered}"
        );
    }
}

#[cfg(feature = "llvm")]
#[test]
fn a_repeated_non_recursive_call_is_not_recursion() {
    let compilation = compile(
        "repeated",
        "module r;\n\
         fn sum(n: integer) -> integer { let s: integer = 0; for i in 0..n { s = s + i; } return s; }\n\
         fn twice(n: integer) -> integer { return sum(n) + sum(n + 1); }\n\
         #[test] entity T {}\n\
         impl T { check: process { let k: integer = 3; await 1ns; print!(\"{}\", twice(sum(k))); } }\n",
    );
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
}
