//! `std::mem::{swap, replace}`: a testbench function's parameters are its
//! caller's places, so they write the arguments themselves.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn swap_and_replace_write_their_arguments() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_std_mem_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("mem");
    let source = r#"
module mem;
use std::mem::{replace, swap};
struct Point { pub x: integer, pub y: integer }
#[test] entity T {}
impl T {
    let sig: unsigned[8] = 3;
    let other: unsigned[8] = 9;
    check: process {
        let a: integer = 1;
        let b: integer = 2;
        swap(a, b);
        let p: Point = Point { .x = 1, .y = 2 };
        let q: Point = Point { .x = 3, .y = 4 };
        swap(p, q);
        let v: unsigned[8] = 7;
        let old: unsigned[8] = replace(v, 200);
        swap(sig, other);
        await 1ns;
        print!("a={} b={} p={} q={} old={} v={} sig={} other={}", a, b, p, q, old, v, sig, other);
    }
}
"#;
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("mem.siox"), source),
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
    let output = Command::new(&binary)
        .output()
        .expect("test executable runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected =
        "a=2 b=1 p=Point { x: 3, y: 4 } q=Point { x: 1, y: 2 } old=7 v=200 sig=9 other=3";
    assert!(stdout.lines().any(|line| line == expected), "{stdout}");
}
