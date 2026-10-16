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

/// A read whose index the parameters fold (`acc[i * C + j]` in `G<R, C>`)
/// was a run-time index muxing every element, so a generate chain through
/// such reads warned W-P010 and a 2x3 grid never settled.
#[cfg(feature = "llvm")]
#[test]
fn parameter_indices_fold_on_reads() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_generic_arrays_grid_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("grid");
    let source = r#"module g;
use std::bits::unsigned;
entity Inc { a: unsigned[4] in, y: unsigned[4] out }
impl Inc { y = a + 1; }
entity Dbl { a: unsigned[4] in, y: unsigned[4] out }
impl Dbl { y = a + a; }
entity Grid<R: integer, C: integer, MODE: integer> { z: unsigned[4] out }
impl<R: integer, C: integer, MODE: integer> Grid<R, C, MODE> {
    let acc: unsigned[4][R * C + 1];
    acc[0] = 0;
    rows: for r in 0..R - 1 {
        cols: for c in 0..C - 1 {
            if MODE == 1 {
                let s: Inc = { .a = acc[r * C + c], .y = acc[r * C + c + 1] };
            } else {
                let s: Dbl = { .a = acc[r * C + c] + 1, .y = acc[r * C + c + 1] };
            }
        }
    }
    z = acc[R * C];
}
#[test] entity T {}
impl T {
    let z1: unsigned[4]; let z2: unsigned[4];
    let g1: Grid<2, 3, 1> = { .z = z1 };
    let g2: Grid<2, 2, 0> = { .z = z2 };
    check: process { await 1ns; print!("|{} {}\n", z1, z2); }
}
"#;
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("grid.siox"), source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    let rendered = compilation.render_diagnostics();
    assert!(compilation.succeeded(), "{rendered}");
    assert!(!rendered.contains("W-P010"), "{rendered}");
    let output = std::process::Command::new(&binary).output().unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    // Six increments from 0; four doublings of `a + 1` wrap to 14 in 4 bits.
    assert!(report.contains("|6 14\n"), "{report}");
}
