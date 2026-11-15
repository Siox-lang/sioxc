#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use siox::ir::{LayoutKind, LayoutRange, ScalarDomain};

#[test]
fn generic_contextual_arguments_and_borrowed_fields_reach_native_execution() {
    let source = "module generic_inference_api;
        fn choose<T>(first: T, second: T) -> T { return second; }
        struct Packet { pub value: real, pub words: integer[7..5] }
        type PacketAlias = Packet;
        struct Wrapped(PacketAlias);
        fn last(packet: Wrapped) -> integer { return packet.words[5]; }
        entity Relay { input: real in, output: real out }
        impl Relay { output = choose(0, input); }
        #[test] entity Probe {}
        impl Probe {
            let input: real = 2.5;
            let relay: Relay = { .input = input };
            process {
                let value: real = 4.25;
                let result: real = choose(0, value);
                let literal: real = choose(0, 3.5);
                assert!(result == 4.25 and literal == 3.5);
                let packet: Packet = { .value = result, .words = [7, 9, 11] };
                let wrapped: Wrapped = Wrapped(packet);
                assert!(wrapped.value == 4.25);
                assert!(wrapped.words[7] == 7 and last(wrapped) == 11);
                await 1ns;
                assert!(relay.output == 2.5);
                input = 6.75;
                await 1ns;
                assert!(relay.output == 6.75);
            }
        }";
    for debug in [false, true] {
        let directory = std::env::temp_dir().join(format!(
            "siox-generic-inference-{}-{debug}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&directory).unwrap();
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
            "{}",
            compilation.render_diagnostics()
        );
        let wrapper = compilation
            .modules
            .iter()
            .flat_map(|module| &module.items)
            .find_map(|item| match item {
                siox::syntax::ast::Item::Struct(structure) if structure.name.text == "Wrapped" => {
                    compilation
                        .resolved
                        .as_ref()
                        .unwrap()
                        .declared(structure.name.span)
                }
                _ => None,
            })
            .unwrap();
        let layout = compilation
            .design
            .as_ref()
            .unwrap()
            .type_layouts
            .get(&wrapper)
            .expect("alias-derived types retain their declaration layout");
        assert_eq!(layout.packed_width(), Some(256));
        let LayoutKind::Struct { fields, .. } = &layout.kind else {
            panic!("alias-derived record: {layout:?}");
        };
        assert_eq!(
            fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["value", "words"]
        );
        assert!(matches!(
            fields[0].layout.kind,
            LayoutKind::Scalar {
                domain: ScalarDomain::Real,
                width: 64,
                ..
            }
        ));
        assert!(matches!(
            fields[1].layout.kind,
            LayoutKind::Array {
                range: Some(LayoutRange { left: 7, right: 5 }),
                ..
            }
        ));
        let output = std::process::Command::new(&binary).output().unwrap();
        let failure_ir = directory.join("failure-ir.txt");
        if !output.status.success() {
            use std::fmt::Write;
            let ir = &compilation.design.as_ref().unwrap().process_ir;
            let mut details = format!("{:#?}\n", ir.processes);
            for (index, value) in ir.values.iter().enumerate() {
                writeln!(
                    details,
                    "value {index}: {value:?} layout: {:?}",
                    ir.value_layouts[index]
                )
                .unwrap();
            }
            std::fs::write(&failure_ir, details).unwrap();
        }
        assert!(
            output.status.success(),
            "{}\n{}\n{}\nProcess IR: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            compilation.render_diagnostics(),
            failure_ir.display(),
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
