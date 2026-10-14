//! A testbench call expands where it is made. A pure recursive function
//! (one value, built from `if`/`return` and operators) is shared instead: its
//! recursive calls call the shared native copy, each evaluated only where its
//! selection reaches it. Recursion that needs a frame of its own (a loop or a
//! mutated local in the body) is a compile-time error naming the function and
//! the call, not a run-time "lowering is incomplete".

#[cfg(feature = "llvm")]
fn compile(name: &str, source: &str) -> (siox::compiler::Compilation, std::path::PathBuf) {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_recursion_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join(name);
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join(format!("{name}.siox")), source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    (compilation, binary)
}

#[cfg(feature = "llvm")]
#[test]
fn pure_recursive_functions_run() {
    let (compilation, binary) = compile(
        "pure",
        "module r;\n\
         fn fact(n: integer) -> integer { if n <= 1 { return 1; } return n * fact(n - 1); }\n\
         fn fib(n: integer) -> integer { if n < 2 { return n; } return fib(n - 1) + fib(n - 2); }\n\
         fn even(n: integer) -> Bool { if n == 0 { return true; } return odd(n - 1); }\n\
         fn odd(n: integer) -> Bool { if n == 0 { return false; } return even(n - 1); }\n\
         #[test] entity T {}\n\
         impl T { check: process {\n\
             let k: integer = 5; await 1ns;\n\
             print!(\"fact={} fib={} even={} odd={}\", fact(k), fib(2 * k), even(k + 2), odd(k + 2));\n\
         } }\n",
    );
    assert!(
        compilation.succeeded(),
        "{} {:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    let output = std::process::Command::new(&binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line == "fact=120 fib=55 even=false odd=true"),
        "{stdout}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn recursion_that_needs_a_frame_is_a_compile_error() {
    let (compilation, _) = compile(
        "framed",
        "module r;\n\
         fn total(n: integer) -> integer {\n\
             if n <= 0 { return 0; }\n\
             let s: integer = 0;\n\
             for i in 0..1 { s = s + i; }\n\
             return s + total(n - 1);\n\
         }\n\
         #[test] entity T {}\n\
         impl T { check: process { let k: integer = 3; await 1ns; print!(\"{}\", total(k)); } }\n",
    );
    let rendered = compilation.render_diagnostics();
    assert!(!compilation.succeeded(), "{rendered}");
    assert!(
        rendered.contains("E-P038") && rendered.contains("`total` calls itself"),
        "{rendered}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_repeated_non_recursive_call_is_not_recursion() {
    let (compilation, _) = compile(
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

#[cfg(feature = "llvm")]
#[test]
fn recursion_too_deep_for_the_stack_fails_the_test() {
    let (compilation, binary) = compile(
        "deep",
        "module r;\n\
         fn depth(n: integer) -> integer { if n == 0 { return 0; } return 1 + depth(n - 1); }\n\
         #[test] entity T {}\n\
         impl T { check: process {\n\
             let k: integer = 10000; await 1ns; print!(\"small={}\\n\", depth(k));\n\
             let big: integer = 100000000; print!(\"big={}\\n\", depth(big));\n\
         } }\n",
    );
    assert!(
        compilation.succeeded(),
        "{} {:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    let output = std::process::Command::new(&binary).output().unwrap();
    let report = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    // A failed test, with the function named: not a crash of the executable.
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert!(report.contains("small=10000"), "{report}");
    assert!(
        report.contains("recursion in `depth` is too deep for the native stack"),
        "{report}"
    );
    assert!(!report.contains("big="), "{report}");
}
