#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use std::process::Command;

const SOURCE: &str = r#"module wide_signed_conversion_test;
use std::bits::{unsigned, signed};

struct Inner { pub value: signed[81] }
struct Packet { pub inner: Inner }
fn wrap(value: signed[81]) -> Packet { return { .inner = { .value = value } }; }
fn compare(value: integer) -> Bool {
    let packet: Packet = wrap(signed[81](value));
    return packet.inner.value == signed[81](value);
}

entity Probe { value: integer in, wide: signed[81] out, same: Bool out }
impl Probe {
    wide = signed[81](value);
    same = compare(value);
}

struct Narrow { pub value: signed[3] }
fn make_narrow(value: integer) -> Narrow { return { .value = signed[3](value) }; }
entity NarrowProbe {
    value: integer in, raw: integer out, decoded: integer out,
    nested_raw: integer out, nested_decoded: integer out
}
impl NarrowProbe {
    raw = integer(signed[3](value));
    decoded = sext(signed[3](value));
    nested_raw = integer(make_narrow(value).value);
    nested_decoded = sext(make_narrow(value).value);
}

entity WideProbe { value: signed[81] in, raw: unsigned[81] in, raised: signed[129] out, raw_raised: signed[129] out, nested: signed[129] out }
impl WideProbe {
    raised = signed[129](value);
    raw_raised = signed[129](raw);
    nested = signed[129](wrap(value).inner.value);
}

#[test] entity WideSignedConversionTest {}
impl WideSignedConversionTest {
    let value: integer = -7;
    let wide: signed[81];
    let same: Bool;
    let probe: Probe = { .value = value, .wide = wide, .same = same };
    let narrow_probe: NarrowProbe = { .value = value };
    let source: signed[81] = "010000000000000000000000000000000000000000000000000000000000000000000000000000011";
    let raw: unsigned[81] = "100000000000000000000000000000000000000000000000000000000000000000000000000000011";
    let raised_signal: signed[129];
    let raw_raised_signal: signed[129];
    let wide_probe: WideProbe = { .value = source, .raw = raw, .raised = raised_signal, .raw_raised = raw_raised_signal };
    stimulus: process {
        let expected: signed[81] = "111111111111111111111111111111111111111111111111111111111111111111111111111111001";
        let converted: signed[81] = signed[81](-7);
        assert!(unsigned[81](converted) == unsigned[81](expected), "procedural conversion preserves all negative bits");
        await 1ns;
        assert!(unsigned[81](wide) == unsigned[81](expected), "hardware conversion preserves all negative bits");
        assert!(same, "returned nested signed field equals its argument");
        assert!(narrow_probe.raw == 1 and narrow_probe.decoded == 1
            and narrow_probe.nested_raw == 1 and narrow_probe.nested_decoded == 1,
            "negative integer narrows before raw or signed interpretation");
        assert!(raised_signal[79] == '1' and raised_signal[128] == '0', "hardware widening keeps high positive bits");
        assert!(wide_probe.nested == raised_signal, "returned fields retain their widening format");
        assert!(raw_raised_signal[80] == '1' and raw_raised_signal[128] == '0', "hardware unsigned widening");
        source = expected; await 1ns;
        assert!(raised_signal[128] == '1' and raised_signal[80] == '1' and raised_signal[2] == '0', "hardware signed-to-signed widening");
        assert!(wide_probe.nested == raised_signal, "returned fields retain their signed range");
        let high: signed[81] = "010000000000000000000000000000000000000000000000000000000000000000000000000000011";
        let raised: signed[129] = signed[129](high);
        assert!(raised[79] == '1' and raised[80] == '0' and raised[128] == '0', "widening preserves positive bits above the ABI word");
        let negative: signed[129] = signed[129](expected);
        assert!(expected[expected'high] == '1', "wide source sign bit");
        assert!(expected'length == 81 and expected'high == 80, "wide source bounds");
        assert!(negative[128] == '1', "wide-to-wide high sign");
        assert!(negative[80] == '1', "wide-to-wide original sign");
        assert!(negative[2] == '0', "wide-to-wide low zero");
        assert!(negative[0] == '1', "wide-to-wide low one");
        let unsigned_high: unsigned[81] = "100000000000000000000000000000000000000000000000000000000000000000000000000000011";
        let unsigned_raised: signed[129] = signed[129](unsigned_high);
        assert!(unsigned_raised[80] == '1' and unsigned_raised[128] == '0' and unsigned_raised[0] == '1', "unsigned widening keeps every source bit");
        let minimum: signed[129] = signed[129](-9223372036854775807 - 1);
        assert!(minimum[128] == '1' and minimum[63] == '1' and minimum[62] == '0' and minimum[0] == '0', "minimum integer conversion");
        let narrow: signed[4] = signed[4](-7);
        assert!(unsigned[4](narrow) == 9, "narrowing keeps low bits");
        assert!(integer(narrow) == 9 and sext(narrow) == -7,
            "procedural raw bits and signed value are distinct");
        let clipped: signed[4] = signed[4](expected);
        assert!(unsigned[4](clipped) == 9, "vector narrowing skips sign fill");
        let intermediate: signed[97] = signed[97](expected);
        assert!(intermediate[96] == '1' and intermediate[80] == '1', "each call keeps its own target format");
        let scalar_parts: unsigned[8] = { expected[0], expected[1], expected[2] };
        assert!(scalar_parts == 4, "Logic parts contribute one bit before widening");
        let vector_parts: unsigned[8] = { unsigned[3](5), unsigned[2](2) };
        assert!(vector_parts == 22, "vector parts keep their own widths before widening");
        source[80] = 'X'; source[1] = 'Z'; await 1ns;
        assert!(raised_signal[128] == 'X' and raised_signal[80] == 'X' and raised_signal[1] == 'Z', "hardware widening preserves individual metavalues");
        assert!(wide_probe.nested[128] == 'X' and wide_probe.nested[1] == 'Z',
            "returned-field widening preserves individual metavalues");
        let meta: signed[129] = signed[129](source);
        assert!(meta[128] == 'X' and meta[80] == 'X' and meta[1] == 'Z', "procedural widening preserves individual metavalues");
        source[80] = 'H'; await 1ns;
        assert!(raised_signal[128] == 'H' and raised_signal[80] == 'H' and raised_signal[1] == 'Z', "sign fill preserves weak values");
        value = 13; await 1ns;
        assert!(unsigned[81](wide) == 13 and same, "positive hardware conversion and nested equality");
        assert!(narrow_probe.raw == 5 and narrow_probe.decoded == -3
            and narrow_probe.nested_raw == 5 and narrow_probe.nested_decoded == -3,
            "direct and returned signed fields keep raw and decoded conversions distinct");
        let narrowed: signed[3] = signed[3](value);
        assert!(integer(narrowed) == 5 and sext(narrowed) == -3,
            "dynamic procedural narrowing keeps raw and decoded conversions distinct");
        value = -9223372036854775807 - 1; await 1ns;
        assert!(wide[80] == '1' and wide[63] == '1' and wide[62] == '0' and same, "minimum integer hardware conversion and nested equality");
    }
}
"#;

#[test]
fn multiword_conversions_preserve_sign_bits_and_logic_companions() {
    let directory =
        std::env::temp_dir().join(format!("siox_wide_conversions_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let compiler = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"));
    let mut traces = Vec::new();
    for debug in [false, true] {
        let binary = directory.join(format!("test_{debug}"));
        let compilation = compiler.compile(
            CompileRequest::new(
                SourceInput::memory(directory.join("test.siox"), SOURCE),
                Emit::TestExecutable,
            )
            .with_output(&binary)
            .with_debug(debug),
        );
        assert!(
            compilation.succeeded(),
            "{} {:?}",
            compilation.render_diagnostics(),
            compilation.failure
        );
        let waveform = binary.with_extension("vcd");
        let output = Command::new(&binary)
            .arg("-o")
            .arg(&waveform)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let trace = std::fs::read(&waveform).unwrap();
        assert!(!trace.is_empty());
        traces.push(trace);
    }
    assert_eq!(
        traces[0], traces[1],
        "debugging must preserve multiword waveforms"
    );
    std::fs::remove_dir_all(directory).unwrap();
}
