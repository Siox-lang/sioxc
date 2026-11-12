#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn finite_generic_storage_simulates_without_treating_unused_arguments_as_fields() {
    let mut reference = None;
    for debug in [false, true] {
        let dir = std::env::temp_dir().join(format!(
            "siox-finite-generic-storage-{}-{debug}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let source = "module finite_generic_storage;
            struct Ignore<T> { pub value: integer }
            struct Store<T> { pub value: T }
            type Double<T> = Store<Store<T>>;
            struct Packet {
                pub nested: Double<integer>,
                pub phantom: Ignore<Packet>
            }
            entity Dut { data: Packet in, result: integer out }
            impl Dut { result = data.nested.value.value + data.phantom.value; }
            #[test] entity Probe {}
            impl Probe {
                let packet: Packet = {
                    .nested = { .value = { .value = 7 } },
                    .phantom = { .value = 11 }
                };
                let dut: Dut = { .data = packet };
                process {
                    await 1ns;
                    assert!(dut.result == 18);
                    packet.nested.value.value = 19;
                    await 1ns;
                    assert!(dut.result == 30);
                }
            }";
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
            "{}\n{:?}\nretained artifact: {}",
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
            "{}\n{}\nretained artifact: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            dir.display()
        );
        let wave = std::fs::read(&waveform).unwrap();
        if let Some(reference) = &reference {
            assert_eq!(&wave, reference, "debug/native waveforms must agree");
        } else {
            reference = Some(wave);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
