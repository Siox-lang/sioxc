#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn typed_default_queries_size_native_layouts_and_keep_debug_waveforms() {
    let directory =
        std::env::temp_dir().join(format!("siox-typed-defaults-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let text = r#"module typed_defaults_api;
enum Phase { First = 7, Last = 19 }
struct Count(integer);
impl Default for Count { static fn default() -> Self { return Count(13); } }
struct Config { pub number: integer, pub phase: Phase, pub count: Count, pub samples: integer[-2..1] }
type Lanes<N: integer> = integer[N];
const LANES: Lanes<3> = Lanes<3>();
type Pair = (integer, real);
const PAIR: Pair = Pair();
const GRID: integer[3][2] = integer[3][2]();
const INITIAL: Config = Config { .number = 3 };
const FIRST: Phase = Phase();
fn width() -> integer { return INITIAL.number + integer(INITIAL.count) + LANES'length - 15; }
entity Dut { data: unsigned[width()] in, value: integer out, size: integer out }
impl Dut { value = integer(data); size = width(); }
#[test] entity Probe {}
impl Probe {
    let data: unsigned[width()] = 3;
    let dut: Dut = { .data = data };
    process {
        assert!(width() == 4 and FIRST == Phase::First);
        assert!(PAIR.0 == 0 and PAIR.1 == 0.0);
        assert!(GRID[1][2] == 0);
        await 1ns;
        assert!(dut.size == 4 and dut.value == 3);
        data = 9;
        await 1ns;
        assert!(dut.size == 4 and dut.value == 9);
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
            "{}\n{}\nretained artifact: {}\nprocess values: {:#?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            directory.display(),
            compilation
                .design
                .as_ref()
                .map(|design| &design.process_ir.values)
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
