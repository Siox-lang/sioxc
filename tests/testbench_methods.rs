//! `recv.method(args)` in testbench stimulus (spec 3.20): the runner inlines
//! the impl method's body, so a struct-typed testbench local can drive a DUT
//! through a method result. Runs the native fixture via the CLI.

use std::process::Command;

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
