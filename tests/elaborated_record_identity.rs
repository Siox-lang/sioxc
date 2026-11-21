#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use siox::elab::EType;
use siox::syntax::ast::Item;

#[test]
fn elaborated_record_aliases_and_distinct_owners_keep_native_wiring() {
    let directory = std::env::temp_dir().join(format!(
        "siox-elaborated-record-identity-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("left.siox"),
        r#"module left;
        pub const LOAD_LEFT: integer = 1;
        pub struct Pair { pub value: integer }
        pub type Alias = Pair;
        pub struct Wrapped(Alias);
        pub entity Relay { input: Alias in, output: integer out }
        impl Relay { output = input.value; }
        pub entity WrappedRelay { input: Wrapped in, output: integer out }
        impl WrappedRelay { output = input.value; }
    "#,
    )
    .unwrap();
    std::fs::write(
        directory.join("right.siox"),
        r#"module right;
        pub const LOAD_RIGHT: integer = 2;
        pub struct Pair { pub value: integer }
        pub entity Relay { input: Pair in, output: integer out }
        impl Relay { output = input.value; }
    "#,
    )
    .unwrap();
    let source = r#"module elaborated_record_identity;
        use left::LOAD_LEFT;
        use right::LOAD_RIGHT;
        #[test] entity Probe {}
        impl Probe {
            let a: left::Alias = { .value = 3 };
            let b: right::Pair = { .value = 7 };
            let wrapped: left::Wrapped = left::Wrapped(a);
            let left_relay: left::Relay = { .input = a };
            let right_relay: right::Relay = { .input = b };
            let wrapped_relay: left::WrappedRelay = { .input = wrapped };
            process {
                await 1ns;
                assert!(LOAD_LEFT == 1 and LOAD_RIGHT == 2);
                assert!(left_relay.output == 3 and right_relay.output == 7);
                assert!(wrapped_relay.output == 3);
                a.value = 41;
                b.value = 29;
                wrapped.value = 13;
                await 1ns;
                assert!(left_relay.output == 41 and right_relay.output == 29);
                assert!(wrapped_relay.output == 13);
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
        let resolved = compilation.resolved.as_ref().unwrap();
        let hierarchy = compilation.hierarchy.as_ref().unwrap();
        for (module_name, instance_name) in [("left", "left_relay"), ("right", "right_relay")] {
            let module = compilation
                .modules
                .iter()
                .find(|module| {
                    module.path.segments.len() == 1 && module.path.segments[0].text == module_name
                })
                .unwrap();
            let pair = module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Struct(structure) if structure.name.text == "Pair" => {
                        resolved.declared(structure.name.span)
                    }
                    _ => None,
                })
                .unwrap();
            let instance = hierarchy
                .instances
                .iter()
                .find(|instance| instance.name == instance_name)
                .unwrap();
            assert!(matches!(&instance.connections[0].ty,
                EType::Named { definition: Some(owner), .. } if *owner == pair));
        }
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
