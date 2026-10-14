//! Recursion in testbenches. A pure recursive function (one value, built from
//! `if`/`return` and operators) calls its shared native copy. Any other is
//! expanded once with a runtime frame stack: a recursive call saves the
//! activation's locals, jumps back to the body, and restores them on return.
//! A frame saves each local with its X/Z plane and copies written parameters
//! back to local arguments on return, as a plain call writes through them.
//! Writing back to a signal or storage argument is a compile-time error.

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
fn recursion_with_loops_locals_effects_and_await_runs_in_frames() {
    let (compilation, binary) = compile(
        "framed",
        "module r;\n\
         extern \"C\" { fn putchar(value: integer) -> integer; }\n\
         fn total(n: integer) -> integer {\n\
             if n <= 0 { return 0; }\n\
             let s: integer = 0;\n\
             for i in 0..1 { s = s + i; }\n\
             return s + total(n - 1);\n\
         }\n\
         fn fibl(n: integer) -> integer {\n\
             if n < 2 { return n; }\n\
             let a: integer = fibl(n - 1);\n\
             let b: integer = fibl(n - 2);\n\
             return a + b;\n\
         }\n\
         fn noisy(n: integer) -> integer {\n\
             if n == 0 { return 0; }\n\
             let printed: integer = putchar(48 + n);\n\
             return 1 + noisy(n - 1);\n\
         }\n\
         fn countdown(n: integer) -> integer {\n\
             if n == 0 { return 0; }\n\
             await 1ns;\n\
             let below: integer = countdown(n - 1);\n\
             return below + n;\n\
         }\n\
         #[test] entity T {}\n\
         impl T { check: process {\n\
             let k: integer = 3; await 1ns;\n\
             let n: integer = noisy(k + 1);\n\
             print!(\"|total={} fibl={} noisy={} countdown={}\", total(k), fibl(10), n, countdown(k + 1));\n\
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
    // Each activation's effect runs once, outermost first.
    assert!(
        stdout.contains("4321|total=3 fibl=55 noisy=4 countdown=10"),
        "{stdout}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn written_parameters_reach_their_arguments_through_recursion() {
    // As in a plain call, a written parameter reaches the caller's local:
    // a frame copies it back on return.
    let (compilation, binary) = compile(
        "writes",
        "module r;\n\
         fn bump(n: integer) -> integer {\n\
             if n <= 0 { return 0; }\n\
             n = n - 1;\n\
             for i in 0..1 { n = n + 0; }\n\
             return 1 + bump(n);\n\
         }\n\
         fn shuffle(a: integer, b: integer, depth: integer) -> integer {\n\
             if depth == 0 { return a; }\n\
             let t: integer = a;\n\
             a = b;\n\
             b = t + 1;\n\
             return shuffle(a, b, depth - 1);\n\
         }\n\
         #[test] entity T {}\n\
         impl T { check: process {\n\
             let k: integer = 3; let x: integer = 10; let y: integer = 20; await 1ns;\n\
             let r: integer = bump(k);\n\
             let s: integer = shuffle(x, y, 3);\n\
             print!(\"|r={} k={} s={} x={} y={}\", r, k, s, x, y);\n\
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
    // The values the same calls give when parameters alias the arguments.
    assert!(stdout.contains("|r=3 k=0 s=21 x=21 y=12"), "{stdout}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_written_parameter_on_storage_is_a_compile_error() {
    let (compilation, _) = compile(
        "storage",
        "module r;\n\
         fn bump(n: integer) -> integer {\n\
             if n <= 0 { return 0; }\n\
             n = n - 1;\n\
             for i in 0..1 { n = n + 0; }\n\
             return 1 + bump(n);\n\
         }\n\
         #[test] entity T {}\n\
         impl T { let counter: integer = 3; check: process { await 1ns; print!(\"{}\", bump(counter)); } }\n",
    );
    let rendered = compilation.render_diagnostics();
    assert!(!compilation.succeeded(), "{rendered}");
    assert!(
        rendered.contains("E-P038") && rendered.contains("`bump` calls itself"),
        "{rendered}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn frames_save_x_z_planes() {
    let (compilation, binary) = compile(
        "planes",
        "module r;\nuse std::bits::unsigned;\n\
         fn ones(n: integer) -> integer {\n\
             if n <= 0 { return 0; }\n\
             let v: unsigned[8] = 3;\n\
             for i in 0..1 { v = v + 1; }\n\
             return integer(v) + ones(n - 1);\n\
         }\n\
         fn keepx(n: integer) -> Logic {\n\
             let l: Logic = if n == 2 { 'X' } else { 'Z' };\n\
             if n == 0 { return '1'; }\n\
             for i in 0..0 { l = l; }\n\
             let below: Logic = keepx(n - 1);\n\
             return l;\n\
         }\n\
         #[test] entity T {}\n\
         impl T { check: process { let k: integer = 2; await 1ns; print!(\"|ones={} keepx={}\", ones(k), keepx(k)); } }\n",
    );
    assert!(
        compilation.succeeded(),
        "{} {:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    let output = std::process::Command::new(&binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The outer activation's `'X'` survives inner ones holding `'Z'`.
    assert!(stdout.contains("|ones=10 keepx='X'"), "{stdout}");
}

#[cfg(feature = "llvm")]
#[test]
fn an_unused_effectful_let_still_runs() {
    let (compilation, binary) = compile(
        "effect",
        "module r;\n\
         extern \"C\" { fn putchar(value: integer) -> integer; }\n\
         fn twice(n: integer) -> integer { let printed: integer = putchar(65); return n + n; }\n\
         #[test] entity T {}\n\
         impl T { check: process { let k: integer = 4; await 1ns; print!(\"|r={}\", twice(k)); } }\n",
    );
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let output = std::process::Command::new(&binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("A|r=8"), "{stdout}");
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
