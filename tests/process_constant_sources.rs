#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn process_metadata_keeps_module_owners_and_instance_parameters_without_seeds() {
    let directory = std::env::temp_dir().join(format!(
        "siox-process-constant-sources-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    for (name, count) in [("left", 3), ("right", 5)] {
        std::fs::write(
            directory.join(format!("{name}.siox")),
            format!(
                "module {name}; pub const COUNT: integer = {count};
             pub fn width() -> integer {{ return COUNT; }}"
            ),
        )
        .unwrap();
    }
    let source = r#"module process_constant_sources;
        use left::width;
        use right::COUNT;
        entity Bank<W: integer> { index: integer in, value: integer out }
        impl<W: integer> Bank<W> {
            process {
                let cells: integer[W];
                for j in 0..(W - 1) {
                    cells[j] = j + left::width() + right::width();
                }
                value = cells[index];
            }
        }
        #[test] entity Probe {}
        impl Probe {
            let a: integer = 0;
            let b: integer = 0;
            let small: Bank<W = width()> = { .index = a };
            let large: Bank<W = COUNT> = { .index = b };
            process {
                await 1ns;
                assert!(small.value == 8 and large.value == 8);
                a = 2;
                b = 4;
                await 1ns;
                assert!(small.value == 10 and large.value == 12);
            }
        }
    "#;
    let mut reference = None;
    for debug in [false, true] {
        let binary = directory.join(if debug { "debug.bin" } else { "ordinary.bin" });
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
            "{}\n{}\nretained: {}",
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
    std::fs::remove_dir_all(directory).unwrap();
}
