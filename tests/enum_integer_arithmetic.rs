#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use std::process::Command;

#[test]
fn narrow_enum_conversion_uses_kernel_integer_width_in_resolvers() {
    let source = r#"module enum_integer_arithmetic;
enum Level { Low, High }
impl Resolve for Level {
    fn resolve(self, rhs: Level) -> Level {
        let sum: integer = integer(self) + integer(rhs);
        return if sum > 0 { Level::High } else { Level::Low };
    }
}
fn total(left: Level, right: Level) -> integer {
    return integer(left) + integer(right);
}
entity Probe { left: Level in, right: Level in, count: integer out, resolved: Level out }
impl Probe { count = total(left, right); resolved = left; resolved = right; }
#[test] entity Test {}
impl Test {
    let left: Level = Level::High; let right: Level = Level::High;
    let count: integer; let resolved: Level;
    let probe: Probe = { .left = left, .right = right, .count = count, .resolved = resolved };
    process {
        assert!(total(left, right) == 2, "procedural enum arithmetic");
        await 1ns;
        assert!(count == 2 and resolved == Level::High, "two High values stay positive");
        left = Level::Low; await 1ns;
        assert!(count == 1 and resolved == Level::High, "one High driver");
        right = Level::Low; await 1ns;
        assert!(count == 0 and resolved == Level::Low, "no High drivers");
    }
}
"#;
    let dir = std::env::temp_dir().join(format!("siox_enum_integer_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let compiler = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"));
    let mut traces = Vec::new();
    for debug in [false, true] {
        let binary = dir.join(format!("test_{debug}"));
        let result = compiler.compile(
            CompileRequest::new(
                SourceInput::memory(dir.join("test.siox"), source),
                Emit::TestExecutable,
            )
            .with_output(&binary)
            .with_debug(debug),
        );
        assert!(
            result.succeeded(),
            "{} {:?}",
            result.render_diagnostics(),
            result.failure
        );
        let waveform = binary.with_extension("vcd");
        let output = Command::new(&binary)
            .arg("-o")
            .arg(&waveform)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        traces.push(std::fs::read(&waveform).unwrap());
    }
    assert_eq!(
        traces[0], traces[1],
        "debugging must preserve native waveforms"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
