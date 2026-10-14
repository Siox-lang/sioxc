//! A long testbench loop keeps a constant native stack. Word buffers for
//! formatting, scheduled writes, file reads and recursion frames were
//! allocated where they were used, so every pass through a loop took more
//! stack: printing a 100-bit value two million times crashed the executable.

#[cfg(feature = "llvm")]
#[test]
fn a_long_printing_loop_keeps_a_constant_stack() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_loop_stack_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("loop");
    let source = "module l;\nuse std::bits::unsigned;\n#[test] entity T {}\n\
        impl T { check: process {\n\
            let v: unsigned[100] = 5; let n: integer = 0;\n\
            for i in 0..1999999 { print!(\"{}\", v); n = n + 1; }\n\
            print!(\"|n={}\", n);\n\
        } }\n";
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("loop.siox"), source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compilation.succeeded(),
        "{} {:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    let output = std::process::Command::new(&binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        &stdout[stdout.len().saturating_sub(300)..]
    );
    assert!(
        stdout.contains("|n=2000000"),
        "{}",
        &stdout[stdout.len().saturating_sub(300)..]
    );
}
