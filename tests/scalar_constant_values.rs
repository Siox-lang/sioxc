#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn real_constant_values_keep_native_owners_scopes_and_debug_waveforms() {
    let directory = std::env::temp_dir().join(format!(
        "siox-scalar-constant-values-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(directory.join("providers")).unwrap();
    for (name, value) in [("left", "1.25"), ("right", "2.5")] {
        std::fs::write(
            directory.join(format!("providers/{name}.siox")),
            format!(
                "module providers::{name}; pub const VALUE: real = {value};
             pub const COPY: real = VALUE;
             pub fn scale(value: real) -> real {{ return value * COPY; }}"
            ),
        )
        .unwrap();
    }
    for reverse in [false, true] {
        let imports = if reverse {
            "use R = providers::right; use L = providers::left;"
        } else {
            "use L = providers::left; use R = providers::right;"
        };
        let text = format!(
            r#"module scalar_constant_values_api;
{imports}
entity Dut {{ data: real in, left: real out, right: real out, local: real out }}
impl Dut {{
    const COPY: real = 3.0;
    left = L::scale(data); right = R::scale(data); local = data * COPY;
}}
#[test] entity Probe {{}}
impl Probe {{
    const COPY: real = 7.0;
    let data: real = 2.0;
    let dut: Dut = {{ .data = data }};
    process {{
        let held: real = COPY;
        if true {{
            let COPY: real = 9.0;
            assert!(held == 7.0 and COPY == 9.0);
        }}
        assert!(held == 7.0 and COPY == 7.0);
        await 1ns;
        assert!(dut.left == 2.5 and dut.right == 5.0 and dut.local == 6.0);
        data = -2.0;
        await 1ns;
        assert!(dut.left == -2.5 and dut.right == -5.0 and dut.local == -6.0);
    }}
}}"#
        );
        let mut reference = None;
        for debug in [false, true] {
            let binary = directory.join(format!("{reverse}-{debug}.bin"));
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
                "{}\n{:?}\nretained: {}",
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
                "{}\n{}\nretained: {}\nprocess values: {:#?}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
                directory.display(),
                compilation
                    .design
                    .as_ref()
                    .map(|design| &design.process_ir.values)
            );
            let waveform = std::fs::read(waveform).unwrap();
            if let Some(reference) = &reference {
                assert_eq!(&waveform, reference, "ordinary/debug waveform parity");
            } else {
                reference = Some(waveform);
            }
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}
