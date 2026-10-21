#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use std::process::Command;

#[test]
fn constrained_operator_results_keep_native_values_and_shapes() {
    let source = r#"module operator_result_constraints;
use std::bits::unsigned;
impl CustomOperator<"clip", unsigned, unsigned[3]> for unsigned {
    attr precedence = 35;
    fn apply(self, rhs: unsigned) -> unsigned[3] {
        return unsigned[3](self + rhs);
    }
}
impl CustomOperator<"grow", unsigned, unsigned[12]> for unsigned {
    attr precedence = 35;
    fn apply(self, rhs: unsigned) -> unsigned[12] { return 2561; }
}
impl CustomOperator<"choose", unsigned, Bool> for unsigned {
    attr precedence = 35;
    fn apply(self, rhs: unsigned) -> Bool { return self > rhs; }
}
impl CustomOperator<"combine", Bool, unsigned[8]> for Bool {
    attr precedence = 35;
    fn apply(self, rhs: Bool) -> unsigned[8] {
        return if self and rhs { 192 } else { 3 };
    }
}
struct Packet { pub value: unsigned[8] }
impl CustomOperator<"bundle", unsigned, Packet> for unsigned {
    attr precedence = 35;
    fn apply(self, rhs: unsigned) -> Packet { return { .value = self or rhs }; }
}
impl CustomOperator<"merge", unsigned, unsigned[8]> for Packet {
    attr precedence = 35;
    fn apply(self, rhs: unsigned) -> unsigned[8] { return self.value or rhs; }
}
entity Probe {
    a: unsigned[8] in, b: unsigned[8] in, short: unsigned[3] out,
    length: integer out, left: integer out, right: integer out,
    grown: unsigned[12] out, chosen: unsigned[8] out, bundled: unsigned[8] out
}
impl Probe {
    short = (a clip b) xor unsigned[3](1);
    length = (a clip b)'length;
    left = (a clip b)'left;
    right = (a clip b)'right;
    grown = (a grow b) xor unsigned[12](1);
    chosen = (a choose b) combine (a choose b);
    bundled = (a bundle b) merge unsigned[8](1);
}
#[test] entity Test {}
impl Test {
    let a: unsigned[8] = 160; let b: unsigned[8] = 6;
    let short: unsigned[3]; let length: integer;
    let left: integer; let right: integer;
    let grown: unsigned[12];
    let chosen: unsigned[8]; let bundled: unsigned[8];
    let probe: Probe = { .a = a, .b = b, .short = short,
        .length = length, .left = left, .right = right, .grown = grown,
        .chosen = chosen, .bundled = bundled };
    process {
        await 1ns;
        assert!(short == 7, "nested operator keeps the three-bit value");
        assert!(length == 3 and left == 0 and right == 2,
            "operator attributes use its result, not its left operand");
        assert!(grown == 2560, "a wider result is not truncated to the input width");
        assert!(chosen == 192 and bundled == 167, "named custom results retain their type");
        a = 255; b = 2; await 1ns;
        assert!(short == 0, "truncation follows the explicit output width");
        a = 1; b = 2; await 1ns;
        assert!(chosen == 3 and bundled == 3, "named custom results update with their inputs");
    }
}
"#;
    let dir = std::env::temp_dir().join(format!("siox_operator_results_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let compiler = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"));
    for range in [None, Some((2, 0)), Some((-1, -3)), Some((-3, -1))] {
        let source = if let Some((left, right)) = range {
            source
                .replace("unsigned[3]", &format!("unsigned[{left}..{right}]"))
                .replace(
                    "left == 0 and right == 2",
                    &format!("left == {left} and right == {right}"),
                )
        } else {
            source.to_owned()
        };
        let mut traces = Vec::new();
        for debug in [false, true] {
            let binary = dir.join(format!("test_{range:?}_{debug}"));
            let result = compiler.compile(
                CompileRequest::new(
                    SourceInput::memory(dir.join("test.siox"), source.clone()),
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
            "debugging must not change simulation output"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
