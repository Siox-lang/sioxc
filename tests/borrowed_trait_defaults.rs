#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn resolved_trait_defaults_keep_native_overrides_and_module_identity() {
    let directory =
        std::env::temp_dir().join(format!("siox-trait-defaults-{}", std::process::id()));
    std::fs::create_dir_all(directory.join("providers")).unwrap();
    std::fs::write(
        directory.join("providers/left.siox"),
        "module providers::left;
         pub struct Token { pub value: integer }
         impl Meter for Token {}
         pub trait Meter {
             static fn tag() -> integer { return 11; }
             fn measure(self) -> integer { return self.value + 10; }
         }",
    )
    .unwrap();
    std::fs::write(
        directory.join("providers/right.siox"),
        "module providers::right;
         pub struct Token { pub value: integer }
         impl Meter for Token {
             static fn tag() -> integer { return 22; }
             fn measure(self) -> integer { return self.value + 20; }
         }
         pub trait Meter {
             static fn tag() -> integer { return 99; }
             fn measure(self) -> integer { return 99; }
         }",
    )
    .unwrap();
    for reversed in [false, true] {
        let imports = if reversed {
            "use R = providers::right; use L = providers::left;"
        } else {
            "use L = providers::left; use R = providers::right;"
        };
        let text = format!(
            r#"module borrowed_trait_defaults_api;
{imports}
entity Dut {{ left: L::Token in, right: R::Token in, a: integer out, b: integer out,
              left_tag: integer out, right_tag: integer out }}
impl Dut {{
    a = left.measure(); b = right.measure();
    left_tag = L::Token::tag(); right_tag = R::Token::tag();
}}
#[test] entity Probe {{}}
impl Probe {{
    let left: L::Token = {{ .value = 3 }};
    let right: R::Token = {{ .value = 7 }};
    let dut: Dut = {{ .left = left, .right = right }};
    process {{
        assert!(left.measure() == 13 and right.measure() == 27);
        assert!(L::Token::tag() == 11 and R::Token::tag() == 22);
        await 1ns;
        assert!(dut.a == 13 and dut.b == 27);
        assert!(dut.left_tag == 11 and dut.right_tag == 22);
        left.value = 8; right.value = 9;
        await 1ns;
        assert!(dut.a == 18 and dut.b == 29);
        assert!(dut.left_tag == 11 and dut.right_tag == 22);
    }}
}}"#
        );
        let mut reference = None;
        for debug in [false, true] {
            let binary = directory.join(format!("{reversed}-{debug}.bin"));
            let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
                CompileRequest::new(
                    SourceInput::memory(directory.join("input.siox"), &text),
                    Emit::TestExecutable,
                )
                .with_output(&binary)
                .with_debug(debug),
            );
            assert!(
                compilation.succeeded(),
                "{}\n{:?}\nretained artifact: {}",
                compilation.render_diagnostics(),
                compilation.failure,
                directory.display()
            );
            let waveform = binary.with_extension("vcd");
            let output = std::process::Command::new(&binary)
                .arg("-o")
                .arg(&waveform)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}\nretained artifact: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
                directory.display()
            );
            let waveform = std::fs::read(waveform).unwrap();
            if let Some(reference) = &reference {
                assert_eq!(&waveform, reference, "debug waveform parity");
            } else {
                reference = Some(waveform);
            }
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}
