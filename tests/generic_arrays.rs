//! `unsigned[W][4]` inside an entity generic over `W` was typed as
//! `unsigned[4]`: the width `W` was not yet known (length 0, "set at use"),
//! so the second index was read as the family's width and `t[0]` was a
//! `Logic`. An indexed base is now always a sized element.

#[cfg(feature = "llvm")]
#[test]
fn arrays_of_parametric_vectors_nest() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_generic_arrays_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("chain");
    let source = r#"module g;
use std::bits::unsigned;
entity Add1<W: integer> { a: unsigned[W] in, y: unsigned[W] out }
impl<W: integer> Add1<W> { y = a + 1; }
entity Chain<W: integer, N: integer> { a: unsigned[W] in, y: unsigned[W] out }
impl<W: integer, N: integer> Chain<W, N> {
    let t: unsigned[W][N + 1];
    t[0] = a;
    stages: for k in 0..N - 1 {
        let s: Add1<W> = { .a = t[k], .y = t[k + 1] };
    }
    y = t[N];
}
#[test] entity T {}
impl T {
    let a8: unsigned[8] = 250; let a16: unsigned[16] = 65534;
    let y8: unsigned[8]; let y16: unsigned[16];
    let c8: Chain<8, 3> = { .a = a8, .y = y8 };
    let c16: Chain<W = 16, N = 5> = { .a = a16, .y = y16 };
    check: process { await 1ns; print!("|{} {}\n", y8, y16); }
}
"#;
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("chain.siox"), source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let output = std::process::Command::new(&binary).output().unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    // 250 + 3, and 65534 + 5 wrapping at 16 bits.
    assert!(report.contains("|253 3\n"), "{report}");
}
