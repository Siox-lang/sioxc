//! A `From` body that is not one value expression (a loop, a statement
//! `if`) expands in the testbench CFG, as an ordinary call does. It used to
//! fall back silently to a raw resize: `Count(0xB5)` read 181, not 5.

#[cfg(feature = "llvm")]
#[test]
fn a_looping_conversion_runs_its_body() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_cfg_conversions_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("conv");
    let source = r#"
module conv;
use std::bits::unsigned;
struct Count { pub n: integer }
impl From<unsigned> for Count {
    fn from(value: unsigned) -> Count {
        let n: integer = 0;
        for i in 0..value'length - 1 {
            if value[i] == '1' { n = n + 1; }
        }
        return Count { .n = n };
    }
}
#[test] entity T {}
impl T {
    check: process {
        let v: unsigned[8] = 0xB5;
        await 1ns;
        let c: Count = Count(v);
        print!("ones={} nested={}", c.n, Count(v + 1).n);
    }
}
"#;
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("conv.siox"), source),
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
    let output = std::process::Command::new(&binary)
        .output()
        .expect("test executable runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 0xB5 = 1011_0101 has five ones; 0xB6 = 1011_0110 has five too.
    assert!(
        stdout.lines().any(|line| line == "ones=5 nested=5"),
        "{stdout}"
    );
}
