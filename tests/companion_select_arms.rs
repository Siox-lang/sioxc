//! Companion planes of a value both arms of a selection read are built once,
//! under the selection's activity. Built per arm, every write in a chain of
//! block-local bit writes doubled the companion graph (a 109-line corpus
//! test compiled for 20 s).

#[cfg(feature = "llvm")]
#[test]
fn chained_local_writes_build_companions_once() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_companion_arms_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("arms");
    let writes = (0..8)
        .map(|bit| format!("local[{bit}] = '{}';", bit % 2))
        .collect::<String>();
    let source = format!(
        r#"
module arms;
use std::bits::unsigned;
entity E {{ enable: Bool in, a: unsigned[8] in, index: integer in, word: unsigned[8] out, selected: unsigned[8] out }}
impl E {{
    if enable {{
        let local: unsigned[8] = a;
        {writes}
        let matrix: unsigned[8][2][2] = [[a, 2], [3, 4]];
        matrix[1][index] = local;
        selected = matrix[1][index];
        word = local;
    }} else {{ word = 0; selected = 0; }}
}}
#[test] entity T {{}}
impl T {{
    let enable: Bool = true; let a: unsigned[8] = 250; let index: integer = 1;
    let dut: E = {{ .enable = enable, .a = a, .index = index }};
    check: process {{ await 1ns; print!("word={{}} selected={{}}", dut.word, dut.selected); }}
}}
"#
    );
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("arms.siox"), source),
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
    let values = compilation.design.as_ref().unwrap().process_ir.values.len();
    eprintln!("VALUES {values}");
    assert!(values < 60_000, "companions built per arm: {values} values");
    let output = std::process::Command::new(&binary)
        .output()
        .expect("test executable runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Odd bits set, even bits clear: 0b1010_1010.
    assert!(
        stdout.lines().any(|line| line == "word=170 selected=170"),
        "{stdout}"
    );
}
