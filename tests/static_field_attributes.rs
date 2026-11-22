#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn nested_field_bounds_reach_native_constants_without_signal_contents() {
    let directory =
        std::env::temp_dir().join(format!("siox-static-field-bounds-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let text = r#"module static_field_bounds_api;
struct Payload<N: integer> { pub data: integer[N], pub backwards: integer[6..2] }
type Packet = Payload<4>;
struct Wrapped(Packet);
struct Bus { pub payload: Wrapped }
entity Dut { bus: Bus in, count: integer out, left: integer out,
             right: integer out, low: integer out, high: integer out, up: Bool out }
impl Dut {
    const WIDTH: integer = bus.payload.data'length;
    const LEFT: integer = bus.payload.backwards'left;
    const RIGHT: integer = bus.payload.backwards'right;
    const LOW: integer = bus.payload.backwards'low;
    const HIGH: integer = bus.payload.backwards'high;
    const UP: Bool = bus.payload.backwards'ascending;
    count = WIDTH;
    left = LEFT;
    right = RIGHT;
    low = LOW;
    high = HIGH;
    up = UP;
}
#[test] entity Probe {}
impl Probe {
    let bus: Bus = { .payload = { .data = [1, 2, 3, 4], .backwards = [8, 7, 6, 5, 4] } };
    let dut: Dut = { .bus = bus };
    process {
        await 1ns;
        assert!(dut.count == 4 and dut.left == 6 and dut.right == 2);
        assert!(dut.low == 2 and dut.high == 6 and not dut.up);
        bus.payload.data[0] = 99;
        bus.payload.backwards[6] = 42;
        await 1ns;
        assert!(dut.count == 4 and dut.left == 6 and dut.right == 2);
        assert!(dut.low == 2 and dut.high == 6 and not dut.up);
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
