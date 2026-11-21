#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use siox::syntax::ast::{Expr, Item};
use siox::types::Ty;

#[test]
fn indexed_string_aliases_keep_nested_native_shapes_and_mutation() {
    let directory =
        std::env::temp_dir().join(format!("siox-indexed-string-shapes-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let text = r#"module string_shapes_api;
type Word = string[3];
struct Envelope { pub word: Word, pub rows: Word[2] }
#[test] entity Probe {}
impl Probe {
    let words: Word[2] = ["abc", "éλx"];
    let copied: Word[2] = words;
    let record: Envelope = { .word = "box", .rows = ["one", "two"] };
    let duplicate: Envelope = record;
    process {
        assert!(words[0] == "abc" and copied[1] == "éλx");
        words[1] = "xyz";
        assert!(words[1] == "xyz" and copied[1] == "éλx");
        assert!(record.word == "box" and duplicate.rows[1] == "two");
        record.rows[0] = "new";
        assert!(record.rows[0] == "new" and duplicate.rows[0] == "one");
    }
}"#;
    let mut reference = None;
    for debug in [false, true] {
        let binary = directory.join(if debug { "debug.bin" } else { "ordinary.bin" });
        let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
            CompileRequest::new(
                SourceInput::memory(directory.join("input.siox"), text),
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
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn radix_provider_family_and_element_identity_reach_native_execution() {
    let directory =
        std::env::temp_dir().join(format!("siox-prefix-contracts-{}", std::process::id()));
    let library = directory.join("std");
    std::fs::create_dir_all(&library).unwrap();
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), library.join(entry.file_name())).unwrap();
        }
    }
    let bits = library.join("bits.siox");
    let mut contents = std::fs::read_to_string(&bits).unwrap();
    for symbol in ["x", "o"] {
        let original = format!("impl Prefix<\"{symbol}\", string> for unsigned {{}}");
        assert_eq!(
            contents.matches(&original).count(),
            1,
            "retain the intended provider edit"
        );
        contents = contents.replace(
            &original,
            &format!("impl Prefix<\"{symbol}\", string> for PrefixAlias {{}}"),
        );
    }
    contents.push_str("\npub struct Radix(std::logic::ULogic[]);\npub type PrefixAlias = Radix;\n");
    std::fs::write(bits, contents).unwrap();
    let text = r#"module prefix_contracts_api;
use std::bits::{Radix, PrefixAlias};
entity Dut { value: PrefixAlias[7..0] out }
impl Dut { value = x"AB"; }
#[test] entity Probe {}
impl Probe {
    let dut: Dut = {};
    process {
        let hex: Radix[7..0] = x"AB";
        let octal: PrefixAlias[5..0] = o"17";
        assert!(hex'length == 8 and octal'length == 6);
        assert!(hex[7] == '1' and hex[6] == '0' and hex[5] == '1' and hex[4] == '0');
        assert!(hex[3] == '1' and hex[2] == '0' and hex[1] == '1' and hex[0] == '1');
        assert!(octal[5] == '0' and octal[4] == '0' and octal[3] == '1');
        assert!(octal[2] == '1' and octal[1] == '1' and octal[0] == '1');
        await 1ns;
        assert!(dut.value[7] == hex[7] and dut.value[6] == hex[6]);
        assert!(dut.value[5] == hex[5] and dut.value[4] == hex[4]);
        assert!(dut.value[3] == hex[3] and dut.value[2] == hex[2]);
        assert!(dut.value[1] == hex[1] and dut.value[0] == hex[0]);
    }
}"#;
    let mut reference = None;
    for debug in [false, true] {
        let binary = directory.join(if debug { "debug.bin" } else { "ordinary.bin" });
        let mut compilation = Compiler::new(&library).compile(
            CompileRequest::new(
                SourceInput::memory(directory.join("input.siox"), text),
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
        let resolved = compilation.resolved.as_ref().unwrap();
        let element = compilation
            .modules
            .iter()
            .find_map(|module| {
                if !module
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.text.as_str())
                    .eq(["std", "logic"])
                {
                    return None;
                }
                module.items.iter().find_map(|item| match item {
                    Item::Enum(declaration) if declaration.name.text == "ULogic" => {
                        resolved.declared(declaration.name.span)
                    }
                    _ => None,
                })
            })
            .unwrap();
        let typed = compilation.typed.as_ref().unwrap();
        let mut literals = 0;
        for module in &mut compilation.modules {
            if !module
                .path
                .segments
                .iter()
                .map(|segment| segment.text.as_str())
                .eq(["prefix_contracts_api"])
            {
                continue;
            }
            for item in &mut module.items {
                siox::syntax::walk::walk_item(item, &mut |expression| {
                    if let Expr::BitStrLit { base, span, .. } = expression {
                        assert_eq!(
                            typed.expr_type(*span),
                            Some(&Ty::Array {
                                elem: Box::new(Ty::Named(element)),
                                family: Some("std::bits::Radix".to_string()),
                                len: if *base == 'x' { 8 } else { 6 },
                                low: 0,
                            })
                        );
                        literals += 1;
                    }
                    true
                });
            }
        }
        assert_eq!(
            literals, 3,
            "inspect every entry literal's actual typed identity"
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
            assert_eq!(&waveform, reference, "debug parity");
        } else {
            reference = Some(waveform);
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}
