//! `recv.method(args)` in testbench stimulus (spec 3.20): the runner inlines
//! the impl method's body, so a struct-typed testbench local can drive a DUT
//! through a method result. Runs the native fixture via the CLI.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn initializer_cfgs_reset_all_roots_once_under_test_filters() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let source = r#"module reset_filter;
        extern "C" { fn putchar(value: integer) -> integer; }
        fn initialize(tag: integer) -> integer {
            let printed: integer = putchar(tag);
            let sum: integer = 0;
            for i in 0..3 { sum = sum + i; }
            return sum;
        }
        #[test] entity First {}
        impl First {
            let value: integer = initialize(65);
            process { assert!(value == 6, "fresh first root"); value = 99; }
        }
        #[test] entity Second {}
        impl Second {
            let value: integer = initialize(66);
            process { assert!(value == 6, "fresh second root"); value = 100; }
        }"#;
    let binary = std::env::temp_dir().join(format!("siox_reset_filter_{}", std::process::id()));
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory("/virtual/reset_filter.siox", source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compilation.succeeded(),
        "{}\n{:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    for (filter, count) in [(None, 2), (Some("reset_filter::Second"), 1)] {
        let mut command = Command::new(&binary);
        if let Some(filter) = filter {
            command.arg(filter);
        }
        let output = command.output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout).to_string()
            + &String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{text}");
        assert_eq!(
            text.matches("AB").count(),
            count,
            "each reset executes both roots once in order: {text}"
        );
    }
    let _ = std::fs::remove_file(binary);
}

#[cfg(feature = "llvm")]
#[test]
fn reset_initializers_use_ordered_cfgs_before_hardware_and_stimulus() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let source = r#"module reset_cfg;
        use std::bits::unsigned;
        struct Packet { pub n: integer, pub data: unsigned[128], pub mark: Logic }
        fn accumulate(seed: integer) -> integer {
            let total: integer = seed;
            for i in 0..3 { total = total + i; }
            return total;
        }
        fn make(seed: integer) -> Packet {
            let packet: Packet = {.n=accumulate(seed),.data=0,.mark='Z'};
            packet.data[63] = 'Z'; packet.data[0] = 'X';
            await 1ns; return packet;
        }
        fn publish(target: Bit) -> integer {
            target = '1' after 1ns; await target == '1'; return 9;
        }
        entity Probe { packet: Packet in, n: integer out, data: unsigned[128] out }
        impl Probe { n = packet.n; data = packet.data; }
        #[test] entity Test {}
        impl Test {
            let empty: string = "";
            let ready: Bit = '0';
            let base: integer = accumulate(0);
            let derived: integer = base + 2;
            let packet: Packet = make(derived);
            let confirmed: integer = publish(ready);
            let dut: Probe = {.packet=packet};
            process {
                assert!(empty == "", "zero-element initializer");
                print!("empty <{}>", empty);
                assert!(base == 6 and derived == 8 and confirmed == 9 and ready == '1', "source order");
                assert!(packet.n == 14 and dut.n == 14 and packet.mark == 'Z', "startup barrier");
                assert!(dut.data[63] == 'Z' and dut.data[0] == 'X', "returned multiword metadata");
            }
        }"#;
    let binary = std::env::temp_dir().join(format!("siox_reset_cfg_{}", std::process::id()));
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory("/virtual/reset_cfg.siox", source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compilation.succeeded(),
        "{}\n{:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    let design = compilation.design.unwrap();
    assert!(design.validate().is_empty(), "{:?}", design.validate());
    assert!(design.process_ir.processes.iter().any(|process| matches!(
        process.activation,
        siox::ir::ProcessActivation::Initialization
    )));
    let empty = design
        .process_ir
        .storages
        .iter()
        .find(|storage| storage.name == "empty")
        .unwrap();
    let initializer = empty.initializer.unwrap();
    assert_eq!(
        design.process_ir.values[initializer.0 as usize].bit_width,
        None
    );
    let output = Command::new(&binary).output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("empty <>"));
    let _ = std::fs::remove_file(binary);
}

#[cfg(feature = "llvm")]
#[test]
fn value_calls_return_lexical_aggregates_through_cfgs() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let source = r#"module value_cfg;
        use std::bits::unsigned;
        struct Packet { pub n: integer, pub data: unsigned[128], pub mark: Logic }
        impl Packet { pub fn advance(self, steps: integer) -> Packet {
            for i in 0..steps { self.n = self.n + i; }
            await 1ns; return self;
        } }
        fn relabel(rows: Packet[3..2]) -> Packet[-1..0] {
            let copied: Packet[-1..0] = rows;
            for i in -1..0 { copied[i].n = copied[i].n + 1; }
            return copied;
        }
        fn hold<T>(value: T, steps: integer) -> T {
            for i in 0..steps { await 1ns; }
            return value;
        }
        fn bump(target: integer) -> integer { target = 9; return 2; }
        fn observe(value: integer) -> integer { await 1ns; return value + bump(value); }
        fn observe_after(value: integer) -> integer { await 1ns; return value + (bump(value) + value); }
        fn trio(value: integer) -> integer[3] { await 1ns; return [value, bump(value), value]; }
        fn formatted(value: integer) -> integer {
            print!("snapshot={} return={} after={}", value, bump(value), value);
            await 1ns; return value;
        }
        fn end(count: integer) -> integer { count = count + 1; await 1ns; return 2; }
        #[test] entity Test {}
        impl Test { process {
            let count: integer = 0;
            let sum: integer = 0;
            for i in 0..end(count) { sum = sum + i; }
            assert!(count == 1 and sum == 3, "loop bound call executes only at loop entry");
            let original: integer = 4;
            let observed: integer = observe(original);
            assert!(observed == 6 and original == 9, "snapshot must not redirect parameter alias");
            original = 4;
            let after: integer = observe_after(original);
            assert!(after == 15 and original == 9, "later read sees call's write");
            original = 4;
            let three: integer[3] = trio(original);
            assert!(three[0] == 4 and three[1] == 2 and three[2] == 9, "array operands retain written order");
            original = 4;
            let printed: integer = formatted(original);
            assert!(printed == 9 and original == 9, "formatted call writes its caller alias");
            let rows: Packet[3..2] = [{.n=4,.data=18446744073709551616,.mark='Z'},
                                     {.n=20,.data=7,.mark='X'}];
            let advanced: Packet = rows[3].advance(2);
            assert!(advanced.n == 7 and advanced.data == 18446744073709551616 and advanced.mark == 'Z', "aggregate return");
            let copied: Packet[-1..0] = relabel(rows);
            assert!(copied[-1].n == 8 and copied[0].n == 21 and copied[0].mark == 'X', "array return");
            let held: Packet = hold(advanced, 2);
            assert!(held.n == 7 and held.data == 18446744073709551616 and held.mark == 'Z', "generic return");
        } }"#;
    let binary = std::env::temp_dir().join(format!("siox_value_cfg_{}", std::process::id()));
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory("/virtual/value_cfg.siox", source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compilation.succeeded(),
        "{}\n{:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    let design = compilation.design.unwrap();
    assert!(design.validate().is_empty(), "{:?}", design.validate());
    let output = Command::new(&binary).output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("snapshot=4 return=2 after=9"));
    let _ = std::fs::remove_file(binary);
}

#[cfg(feature = "llvm")]
#[test]
fn delayed_value_calls_capture_targets_and_values() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let source = r#"module delay_cfg;
        fn duration(selector: integer) -> time { selector = 1; return 1ns; }
        fn duration_and_change(value: Bit, selector: integer) -> time {
            value = '0'; selector = 0; await 1ns; return 1ns;
        }
        entity Probe { pulses: Bit[2] in, seen: Bit[2] out }
        impl Probe { seen = pulses; }
        #[test] entity Test {}
        impl Test {
            let pulses: Bit[2] = ['0', '0'];
            let dut: Probe = {.pulses=pulses};
            process {
                let selector: integer = 0;
                pulses[selector] = '1' after duration(selector);
                await 2ns;
                assert!(selector == 1 and pulses[0] == '1' and pulses[1] == '0', "delay retargeted the first write");
                let value: Bit = '1';
                pulses[selector] = value after duration_and_change(value, selector);
                await 2ns;
                assert!(value == '0' and selector == 0 and pulses[1] == '1', "delay lost the second target/value snapshot");
            }
        }"#;
    let binary = std::env::temp_dir().join(format!("siox_delay_cfg_{}", std::process::id()));
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory("/virtual/delay_cfg.siox", source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compilation.succeeded(),
        "{}\n{:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    let output = Command::new(&binary).output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_file(binary);
}

#[cfg(feature = "llvm")]
#[test]
fn delayed_dynamic_composites_keep_scalar_masks_and_packed_metadata() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let source = r#"module delayed_composites;
        use std::bits::unsigned;
        struct Packet { pub data: unsigned[128], pub tag: Logic }
        #[test] entity Test {}
        impl Test {
            let rows: Packet[3..2] = [{.data=0,.tag='Z'}, {.data=0,.tag='X'}];
            let bus: unsigned[128] = 0;
            process {
                let index: integer = 3;
                let bit: integer = 63;
                rows[3].data[63] = 'Z';
                rows[index].data = 0 after 5ns;
                index = 2;
                rows[3].data[63] = 'X' after 10ns;
                rows[index].data[64] = 'Z' after 5ns;
                // Both X and Z have the same primary value bit. Their
                // companion values must participate in pulse rejection.
                bus[bit] = 'X' after 5ns;
                bus[63] = 'Z' after 10ns;
                bus[64] = 'Z' after 5ns;
                bit = 64;
                bus[0] = 'Z';
                await 6ns;
                assert!(bus[63] == '0' and bus[64] == 'Z' and bus[0] == 'Z', "captured bit target, metadata rejection and untouched lanes");
                assert!(rows[3].data[63] == 'Z' and rows[2].data[64] == 'Z', "whole/dynamic scalar overlap uses one physical waveform");
                assert!(rows[3].tag == 'Z' and rows[2].tag == 'X', "unwritten struct fields stay intact");
                await 5ns;
                assert!(bus[63] == 'Z' and bus[64] == 'Z' and bus[0] == 'Z', "metadata survives scheduled expiry");
                assert!(rows[3].data[63] == 'X' and rows[2].data[64] == 'Z', "multiword value and companion masks preserve independent elements");
            }
        }"#;
    let binary = std::env::temp_dir().join(format!("siox_delay_composites_{}", std::process::id()));
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory("/virtual/delayed_composites.siox", source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compilation.succeeded(),
        "{}\n{:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    let output = Command::new(&binary).output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_file(binary);
}

#[cfg(feature = "llvm")]
#[test]
fn recursive_value_call_fails_before_argument_or_callee_effects() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let source = r#"module recursive_value;
        extern "C" { fn putchar(value: integer) -> integer; }
        fn recurse(target: integer, key: integer) -> integer {
            let printed: integer = putchar(36);
            target = key;
            if key > 0 { return recurse(target, key); }
            return 0;
        }
        #[test] entity Test {}
        impl Test { process {
            let target: integer = 0;
            let result: integer = recurse(target, putchar(64));
        } }
    "#;
    let binary = std::env::temp_dir().join(format!("siox_recursive_value_{}", std::process::id()));
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory("/virtual/recursive_value.siox", source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compilation.succeeded(),
        "{}\n{:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    let output = Command::new(&binary).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains('@') && !stdout.contains('$'),
        "call effects escaped the failed inline: {stdout}"
    );
    assert!(stdout.contains("direct Process IR lowering is incomplete"));
    let _ = std::fs::remove_file(binary);
}

#[cfg(feature = "llvm")]
#[test]
fn procedural_calls_share_cfg_and_preserve_call_evaluation() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let source = r#"module procedural_cfg;
        use std::bits::unsigned;
        struct Counter { pub n: integer, pub bits: unsigned[128] }
        impl Counter {
            pub fn update(self, delta: integer) {
                if delta <= 0 { return; }
                let next: integer = self.n + delta;
                self.n = next;
                await 1ns;
                for i in 2..0 { self.n = self.n + i; }
                return;
                self.n = 999;
            }
        }
        fn forward(counter: Counter, delta: integer) { counter.update(delta); }
        fn select_once(counter: Counter, selector: integer) {
            selector = 1;
            if selector == 1 { forward(counter, 2); }
            match selector { 1 => { counter.bits = counter.bits + 1; } _ => { return; } }
        }
        extern "C" { fn putchar(value: integer) -> integer; }
        fn record(counter: Counter, a: integer, b: integer) {
            if a == 65 { counter.n = a; }
            await 1ns;
            counter.n = counter.n + b;
        }
        entity Probe { pulse: Bit in, seen: Bit out }
        impl Probe { seen = pulse; }
        fn delayed(target: Bit) { target = '1' after 1ns; await 2ns; return; }
        #[test] entity Test {}
        impl Test {
            let rows: Counter[-1..0] = [
                { .n = 4, .bits = 18446744073709551616 }, { .n = 20, .bits = 7 }
            ];
            let selector: integer = 0;
            let pulse: Bit = '0';
            let seen: Bit;
            let probe: Probe = { .pulse = pulse, .seen = seen };
            select_once(rows[selector - 1], selector);
            assert!(selector == 1 and rows[-1].n == 9 and rows[0].n == 20,
                    "receiver selection is captured before callee mutation");
            assert!(rows[-1].bits == 18446744073709551617, "wide alias writes");
            rows[-1].update(3);
            assert!(rows[-1].n == 15, "callee locals are fresh per inline");
            rows[-1].update(0);
            assert!(rows[-1].n == 15, "early return does not terminate the caller");
            record(rows[-1], putchar(65), putchar(66));
            assert!(rows[-1].n == 131, "computed operands survive suspension");
            delayed(pulse);
            assert!(pulse == '1' and seen == '1', "delayed write settles");
        }"#;
    let binary = std::env::temp_dir().join(format!("siox_procedural_cfg_{}", std::process::id()));
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory("/virtual/procedural_cfg.siox", source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let design = compilation.design.unwrap();
    assert!(design.validate().is_empty(), "{:?}", design.validate());
    assert!(design
        .process_ir
        .processes
        .iter()
        .flat_map(|process| &process.blocks)
        .flat_map(|block| &block.instructions)
        .all(|instruction| !matches!(
            instruction,
            siox::ir::ProcessInstruction::Runtime {
                operation: siox::ir::ProcessRuntimeOp::Call(_),
                ..
            }
        )));
    let wave = binary.with_extension("vcd");
    let output = Command::new(&binary).arg("-o").arg(&wave).output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout.matches("AB").count(),
        1,
        "arguments repeated or reordered: {stdout}"
    );
    let trace = std::fs::read_to_string(&wave).unwrap();
    assert!(
        trace.contains("#4000000"),
        "missing delayed-write timestamp: {trace}"
    );
    let _ = std::fs::remove_file(binary);
    let _ = std::fs::remove_file(wave);
}

#[cfg(feature = "llvm")]
#[test]
fn recursive_procedure_fails_before_argument_or_callee_effects() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let source = r#"module recursive_procedure;
        extern "C" { fn putchar(value: integer) -> integer; }
        fn recurse(target: integer, key: integer) {
            target = key;
            if key > 0 { recurse(target, key); }
        }
        #[test] entity Test {}
        impl Test { let target: integer = 0; recurse(target, putchar(64)); }
    "#;
    let binary =
        std::env::temp_dir().join(format!("siox_recursive_procedure_{}", std::process::id()));
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory("/virtual/recursive_procedure.siox", source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let output = Command::new(&binary).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains('@'),
        "foreign argument executed before unsupported call: {stdout}"
    );
    assert!(stdout.contains("direct Process IR lowering is incomplete"));
    let _ = std::fs::remove_file(binary);
}

#[test]
fn testbench_method_call_runs_via_native_cli() {
    let siox = env!("CARGO_BIN_EXE_sioxc");
    // Run from the repo root so `./std` resolves.
    let root = env!("CARGO_MANIFEST_DIR");
    let fixture = "tests/fixtures/method_test.siox";
    let out = Command::new(siox)
        .current_dir(root)
        .args(["--test", fixture, "--std", "std", "-o"])
        .arg(std::env::temp_dir().join(format!("siox_method_cli_{}", std::process::id())))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "sioxc --test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn testbench_method_call_runs_native() {
    if std::process::Command::new("clang")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: clang not found");
        return;
    }
    let siox = env!("CARGO_BIN_EXE_sioxc");
    let root = env!("CARGO_MANIFEST_DIR");
    let fixture = "tests/fixtures/method_test.siox";
    let bin = std::env::temp_dir().join(format!("siox_method_{}", std::process::id()));
    // Build the standalone native test binary (struct-local + method inline).
    let build = Command::new(siox)
        .current_dir(root)
        .args([
            "--test",
            fixture,
            "--std",
            "std",
            "-o",
            bin.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "native build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    // The binary runs the testbench and exits 0 on PASS.
    let run = Command::new(&bin).status().unwrap();
    assert!(run.success(), "native simulator returned {:?}", run.code());
    let _ = std::fs::remove_file(&bin);
}
