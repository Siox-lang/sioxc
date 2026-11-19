#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn borrowed_array_bases_preserve_native_labels_and_nominal_scope() {
    for reversed in [false, true] {
        let mut reference = None;
        for debug in [false, true] {
            let directory = std::env::temp_dir().join(format!(
                "siox-borrowed-structs-{}-{reversed}-{debug}",
                std::process::id(),
            ));
            std::fs::create_dir_all(directory.join("providers")).unwrap();
            for (name, range) in [("left", "9..7"), ("right", "4..2")] {
                std::fs::write(
                    directory.join(format!("providers/{name}.siox")),
                    format!(
                        "module providers::{name};
                         pub type Storage = Logic[{range}];
                         pub struct Words(Storage);
                         pub struct Chained(Words);"
                    ),
                )
                .unwrap();
            }
            let imports = if reversed {
                "use R = providers::right; use L = providers::left;"
            } else {
                "use L = providers::left; use R = providers::right;"
            };
            let source = format!(
                "module borrowed_structs;
                 {imports}
                 entity Dut {{ left: L::Chained in, right: R::Chained in,
                              index: integer in, first: Logic out, second: Logic out }}
                 impl Dut {{ first = left[index]; second = right[index - 5]; }}
                 #[test] entity Probe {{}}
                 impl Probe {{
                     let left: L::Chained = L::Chained(\"1Z0\");
                     let right: R::Chained = R::Chained(\"0X1\");
                     let index: integer = 9;
                     let dut: Dut = {{ .left = left, .right = right, .index = index }};
                     process {{
                         let local: L::Chained = L::Chained(\"1Z0\");
                         assert!(local'length == 3 and local'left == 9 and local'right == 7);
                         assert!(local[8] == 'Z');
                         assert!(dut.right'length == 3 and dut.right'left == 4 and dut.right'right == 2);
                         await 1ns;
                         assert!(dut.first == '1' and dut.second == '0');
                         index = 8;
                         await 1ns;
                         assert!(dut.first == 'Z' and dut.second == 'X');
                         index = 7;
                         await 1ns;
                         assert!(dut.first == '0' and dut.second == '1');
                     }}
                 }}"
            );
            let binary = directory.join("probe");
            let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
                CompileRequest::new(
                    SourceInput::memory(directory.join("input.siox"), source),
                    Emit::TestExecutable,
                )
                .with_output(&binary)
                .with_debug(debug),
            );
            assert!(
                compilation.succeeded(),
                "{}\n{:?}\nretained artifacts: {}",
                compilation.render_diagnostics(),
                compilation.failure,
                directory.display(),
            );
            let waveform = directory.join("wave.vcd");
            let output = std::process::Command::new(&binary)
                .arg("-o")
                .arg(&waveform)
                .output()
                .unwrap();
            if !output.status.success() {
                use std::fmt::Write;
                let design = compilation.design.as_ref().unwrap();
                let ir = &design.process_ir;
                let mut details = format!("{:#?}\n", ir.processes);
                for (index, value) in ir.values.iter().enumerate() {
                    writeln!(
                        details,
                        "value {index}: {value:?} layout: {:?}",
                        ir.value_layouts[index]
                    )
                    .unwrap();
                }
                writeln!(details, "source layouts: {:#?}", design.source_layouts).unwrap();
                std::fs::write(directory.join("failure-ir.txt"), details).unwrap();
            }
            assert!(
                output.status.success(),
                "{}\n{}\nretained artifacts: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
                directory.display(),
            );
            let wave = std::fs::read(&waveform).unwrap();
            if let Some(reference) = &reference {
                assert_eq!(&wave, reference, "debug mode must retain the same waveform");
            } else {
                reference = Some(wave);
            }
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
}
