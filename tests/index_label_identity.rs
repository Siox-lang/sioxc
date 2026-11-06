#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn declared_labels_survive_nested_indexing_in_hardware_and_processes() {
    let source = r#"module index_label_identity_api;
        struct Holder { pub data: integer[10..11] }
        fn field(value: Holder) -> integer { return value.data[10] + value.data[11]; }
        fn nested(value: integer[-2..-1][10..11]) -> integer {
            return value[10][-2] + value[11][-1];
        }
        entity Dut { input: Holder in, sum: integer out }
        impl Dut { sum = field(input); }
        #[test] entity Probe {}
        impl Probe {
            let holder: Holder = { .data = [31, 47] };
            let dut: Dut = { .input = holder };
            process {
                let matrix: integer[-2..-1][10..11] = [[10, 20], [30, 40]];
                assert!(matrix[10][-2] == 10 and matrix[11][-1] == 40);
                assert!(nested(matrix) == 50);
                let selected: integer[2] = matrix[10][-2..-1];
                assert!(selected[0] == 10 and selected[1] == 20);
                let descending: integer[11..10] = [55, 66];
                assert!(descending[11] == 55 and descending[10] == 66);
                assert!(field(holder) == 78);
                await 1ns;
                assert!(dut.sum == 78);
            }
        }"#;
    let mut reference = None;
    for debug in [false, true] {
        let dir = std::env::temp_dir().join(format!(
            "siox-index-label-identity-{}-{debug}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("test");
        let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
            CompileRequest::new(
                SourceInput::memory(dir.join("input.siox"), source),
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
            dir.display()
        );
        let waveform = dir.join("wave.vcd");
        let output = std::process::Command::new(&binary)
            .arg("-o")
            .arg(&waveform)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}\nretained artifacts: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            dir.display()
        );
        let wave = std::fs::read(waveform).unwrap();
        if let Some(reference) = &reference {
            assert_eq!(&wave, reference, "debug mode must preserve the waveform");
        } else {
            reference = Some(wave);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
