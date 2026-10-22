use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

fn source(left: i64, right: i64) -> String {
    let middle = (left + right) / 2;
    r#"module returned_array_indices;
use std::bits::unsigned;
struct Body { pub gain: real }
struct Packet { pub body: Body, pub gains: real[@LEFT@..@RIGHT@] }
fn make(gain: real) -> Packet {
    return { .body = { .gain = gain }, .gains = [gain, -gain, gain * 2.0] };
}
fn consume(gain: real) -> real {
    let value: Packet = make(gain);
    return value.body.gain + value.gains[@LEFT@]
        + value.gains[@MIDDLE@] + value.gains[@RIGHT@];
}
fn selected(gain: real, index: integer) -> real {
    let value: Packet = make(gain);
    return value.gains[index];
}
entity Probe { gain: real in, index: integer in, result: real out, dynamic: real out }
impl Probe { result = consume(gain); dynamic = selected(gain, index); }
#[test] entity Test {}
impl Test {
    let gain: real = 2.5; let index: integer = @RIGHT@;
    let result: real; let dynamic: real;
    let probe: Probe = { .gain = gain, .index = index, .result = result, .dynamic = dynamic };
    process {
        assert!(consume(gain) == 7.5, "procedural returned real array");
        await 1ns;
        assert!(result == 7.5 and dynamic == 5.0, "hardware returned real array");
        index = @MIDDLE@; await 1ns;
        assert!(dynamic == -2.5, "middle array label");
        index = @LEFT@; await 1ns;
        assert!(dynamic == 2.5, "first array label");
        gain = -1.25; await 1ns;
        assert!(result == -3.75 and dynamic == -1.25, "negative real values");
    }
}
"#
    .replace("@LEFT@", &left.to_string())
    .replace("@MIDDLE@", &middle.to_string())
    .replace("@RIGHT@", &right.to_string())
}

#[test]
fn aggregate_read_selectors_keep_integer_evaluation_boundaries() {
    let compiler = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"));
    for (left, right) in [(-1, -3), (-3, -1), (-1, 1), (1, -1)] {
        let compilation = compiler.compile(CompileRequest::new(
            SourceInput::memory("/virtual/returned_array_indices.siox", source(left, right)),
            Emit::Metadata,
        ));
        assert!(
            compilation.succeeded(),
            "{}",
            compilation.render_diagnostics()
        );
        let design = compilation.design.unwrap();
        let mut checked = 0;
        for value in &design.process_ir.values {
            if let siox::ir::ProcessValueKind::CheckedIndex { index, .. } = value.kind {
                let index = &design.process_ir.values[index.0 as usize];
                // Nonnegative literals do not need a signed evaluation boundary.
                if matches!(index.kind, siox::ir::ProcessValueKind::Number(_))
                    && index.bit_width.is_some_and(|width| width < i64::BITS)
                {
                    continue;
                }
                assert_eq!(index.ty, Some(siox::types::Ty::Integer), "{index:?}");
                assert_eq!(index.bit_width, Some(64), "{index:?}");
                checked += 1;
            }
        }
        assert!(checked > 0, "array selectors must be exercised");
    }
}

#[test]
fn nonnegative_constant_array_selectors_keep_their_literal_width() {
    let source = "module constant_array_projection;\n\
        struct Packet { pub gains: real[0..2] }\n\
        fn selected(packet: Packet) -> real { return packet.gains[2]; }\n\
        entity Dut { packet: Packet in, result: real out }\n\
        impl Dut { result = selected(packet); }\n";
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/constant_array_projection.siox", source),
            Emit::Metadata,
        ));
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let design = compilation.design.unwrap();
    let mut checked = 0;
    for value in &design.process_ir.values {
        if let siox::ir::ProcessValueKind::CheckedIndex { index, .. } = value.kind {
            assert_eq!(
                design.process_ir.values[index.0 as usize].bit_width,
                Some(2)
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "preserve the checked selection's demand boundary"
    );
}

#[test]
fn nonnegative_array_selectors_keep_their_constrained_word_width() {
    let source = "module narrow_array_selector;\n\
        use std::bits::unsigned;\n\
        fn selected(value: Bit[8], index: unsigned[3]) -> Bit { return value[index]; }\n\
        entity Dut { value: Bit[8] in, index: unsigned[3] in, result: Bit out }\n\
        impl Dut { result = selected(value, index); }\n";
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/narrow_array_selector.siox", source),
            Emit::Metadata,
        ));
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let design = compilation.design.unwrap();
    let mut checked = 0;
    for value in &design.process_ir.values {
        if let siox::ir::ProcessValueKind::CheckedIndex { index, .. } = value.kind {
            assert_eq!(
                design.process_ir.values[index.0 as usize].bit_width,
                Some(3)
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "dynamic array indexing must be exercised");
}

#[cfg(feature = "llvm")]
#[test]
fn returned_real_arrays_preserve_signed_labels_values_and_debug_waveforms() {
    use std::process::Command;
    let directory =
        std::env::temp_dir().join(format!("siox_returned_arrays_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let compiler = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"));
    for (left, right, invalid, packed) in [
        (-1, -3, false, false),
        (-3, -1, false, false),
        (-1, 1, false, false),
        (1, -1, false, false),
        (-1, -3, true, false),
        (0, 2, false, true),
        (0, 2, true, true),
    ] {
        let mut source = source(left, right);
        let index_type = if packed { "unsigned[3]" } else { "integer" };
        if packed {
            source = source.replace("index: integer", "index: unsigned[3]");
        }
        let invalid_index = if packed { 7 } else { 999 };
        if invalid {
            source = source.replace(
                &format!("let index: {index_type} = {right}"),
                &format!("let index: {index_type} = {invalid_index}"),
            );
        }
        let mut traces = Vec::new();
        for debug in [false, true] {
            let binary = directory.join(format!("test_{left}_{right}_{invalid}_{packed}_{debug}"));
            let result = compiler.compile(
                CompileRequest::new(
                    SourceInput::memory(directory.join("test.siox"), source.clone()),
                    Emit::TestExecutable,
                )
                .with_output(&binary)
                .with_debug(debug),
            );
            assert!(
                result.succeeded(),
                "{} {:?}",
                result.render_diagnostics(),
                result.failure
            );
            let waveform = binary.with_extension("vcd");
            let output = Command::new(&binary)
                .arg("-o")
                .arg(&waveform)
                .output()
                .unwrap();
            if invalid {
                assert!(!output.status.success(), "invalid index must fail");
                let diagnostic = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(
                    diagnostic.contains(&format!(
                        "index {invalid_index} is outside declared range {left}..{right}"
                    )),
                    "{diagnostic}"
                );
                continue;
            }
            assert!(output.status.success(), "{output:?}");
            let trace = std::fs::read(&waveform).unwrap();
            assert!(!trace.is_empty());
            traces.push(trace);
        }
        if !invalid {
            assert_eq!(
                traces[0], traces[1],
                "debugging must preserve array waveforms"
            );
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}
