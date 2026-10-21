//! A write to one element of a vector (`local[0] = local[3]`) kept the
//! other elements through a constant `and` mask, which the metavalue planes
//! read as `std_logic`'s `and`: `'Z'` and `'W'` came back `'X'`, `'H'` and
//! `'L'` as `'1'` and `'0'`. Each write also cost a truth-table lookup per
//! element, and a chain of 48 of them took 161 s and 2.9 GB to compile
//! (#148).

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

fn chain(writes: usize, init: &str) -> String {
    let body: String = (0..writes)
        .map(|k| format!("        local[{}] = local[{}];\n", k % 8, (k + 3) % 8))
        .collect();
    format!(
        r#"module chain;
use std::bits::unsigned;
entity E {{ enable: Bool in, a: unsigned[8] in, word: unsigned[8] out }}
impl E {{
    if enable {{
        let local: unsigned[8] = a;
{body}        word = local;
    }} else {{
        word = 0;
    }}
}}
#[test] entity T {{}}
impl T {{
    let enable: Bool = true;
    let a: unsigned[8] = {init};
    let dut: E = {{ .enable = enable, .a = a }};
    check: process {{
        await 1ns;
        print!("|{{}} {{}} {{}} {{}} {{}} {{}} {{}} {{}}", dut.word[7], dut.word[6], dut.word[5], dut.word[4], dut.word[3], dut.word[2], dut.word[1], dut.word[0]);
    }}
}}
"#
    )
}

#[cfg(feature = "llvm")]
#[test]
fn an_element_write_keeps_the_other_elements_metavalues() {
    let dir = std::env::temp_dir().join(format!("siox_element_write_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("chain");
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("chain.siox"), chain(1, "\"ZHLW1X0U\"")),
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
    // `local[0] = local[3]` copies the '1'; the rest are as given.
    assert!(
        report.contains("|'Z' 'H' 'L' 'W' '1' 'X' '0' '1'"),
        "{report}"
    );
}

#[test]
fn a_write_chain_lowers_in_linear_size() {
    let values = |writes: usize| {
        let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
            CompileRequest::new(
                SourceInput::memory("/virtual/chain.siox", chain(writes, "250")),
                Emit::Metadata,
            ),
        );
        assert!(
            compilation.succeeded(),
            "{}",
            compilation.render_diagnostics()
        );
        compilation.design.unwrap().process_ir.values.len()
    };
    // About 70 values a write now; it was over 2,200. A long chain nests
    // deeply, so this measures a short one on a roomy stack (debug frames).
    let per_write = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || (values(24) - values(8)) / 16)
        .unwrap()
        .join()
        .unwrap();
    assert!(per_write < 200, "{per_write} IR values per write");
}
