#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn string_constant_metadata_and_character_projection_reach_native_execution() {
    let source = "module string_constant_contexts;
        type Text = Char[];
        const TEXT: Text = \"é🦀\";
        const LABELLED: Char[6..4] = \"Aé🦀\";
        fn width() -> integer { return TEXT'length + 1; }
        fn first() -> Char { return TEXT[0]; }
        fn last() -> Char { return LABELLED[4]; }
        pub attr count: integer for entity = TEXT'length;
        entity Dut { index: integer in, words: integer[width()] out, glyph: Char out, descending: Char out }
        impl Dut {
            attr count = width();
            words[0] = integer(first());
            words[1] = integer(last());
            words[2] = LABELLED'left;
            glyph = TEXT[index];
            descending = LABELLED[index + 4];
        }
        #[test] entity Probe {}
        impl Probe {
            let index: integer = 0;
            let dut: Dut = { .index = index };
            process {
                let first_character: Char = first();
                let last_character: Char = last();
                assert!(integer(first_character) == 233);
                assert!(integer(last_character) == 129408);
                assert!(dut.words'length == 3);
                await 1ns;
                assert!(dut.words[0] == 233 and dut.words[1] == 129408);
                assert!(dut.words[2] == 6 and dut'count == 3);
                assert!(integer(dut.glyph) == 233 and integer(dut.descending) == 129408);
                index = 1;
                await 1ns;
                assert!(integer(dut.glyph) == 129408 and integer(dut.descending) == 233);
            }
        }";
    for debug in [false, true] {
        let directory = std::env::temp_dir().join(format!(
            "siox-string-constants-{}-{debug}",
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
