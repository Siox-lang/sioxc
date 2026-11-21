#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn static_port_bounds_reach_native_constants_without_signal_values() {
    let directory =
        std::env::temp_dir().join(format!("siox-static-port-bounds-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let text = r#"module static_port_bounds_api;
type Lanes = integer[4];
entity Dut { data: unsigned[13..6] in, lanes: Lanes in, nibble: unsigned[4] in,
             count: integer out, bounds: integer[5] out, lane_count: integer out,
             nibble_left: integer out, nibble_right: integer out, nibble_up: Bool out }
impl Dut {
    const WIDTH: integer = data'length;
    const LEFT: integer = data'left;
    const RIGHT: integer = data'right;
    const LOW: integer = data'low;
    const HIGH: integer = data'high;
    const LANES: integer = lanes'length;
    const NIBBLE_LEFT: integer = nibble'left;
    const NIBBLE_RIGHT: integer = nibble'right;
    const NIBBLE_UP: Bool = nibble'ascending;
    count = WIDTH;
    lane_count = LANES;
    nibble_left = NIBBLE_LEFT;
    nibble_right = NIBBLE_RIGHT;
    nibble_up = NIBBLE_UP;
    bounds[0] = LEFT;
    bounds[1] = RIGHT;
    bounds[2] = LOW;
    bounds[3] = HIGH;
    bounds[4] = lanes'right;
}
#[test] entity Probe {}
impl Probe {
    let data: unsigned[13..6] = 171;
    let lanes: Lanes = [1, 2, 3, 4];
    let nibble: unsigned[4] = 9;
    let dut: Dut = { .data = data, .lanes = lanes, .nibble = nibble };
    process {
        await 1ns;
        assert!(dut.count == 8 and dut.lane_count == 4);
        assert!(dut.nibble_left == 0 and dut.nibble_right == 3 and dut.nibble_up);
        assert!(dut.bounds[0] == 13 and dut.bounds[1] == 6);
        assert!(dut.bounds[2] == 6 and dut.bounds[3] == 13 and dut.bounds[4] == 3);
        data = 3;
        lanes[0] = 99;
        nibble = 2;
        await 1ns;
        assert!(dut.count == 8 and dut.lane_count == 4);
        assert!(dut.nibble_left == 0 and dut.nibble_right == 3 and dut.nibble_up);
        assert!(dut.bounds[0] == 13 and dut.bounds[1] == 6 and dut.bounds[4] == 3);
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
