//! `siox build` produces a runnable native simulator binary (stage B5.1).
//! Only meaningful with the `llvm` feature + a clang toolchain.

use std::process::Command;

#[test]
fn test_no_run_builds_a_runnable_binary() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let siox = env!("CARGO_BIN_EXE_sioxc");
    let out = std::env::temp_dir().join(format!("siox_counter_{}", std::process::id()));
    let vcd = out.with_extension("vcd");

    // Build from the repo root so `./std` resolves. The counter fixture lives
    // in-tree (the runnable `.siox` corpus moved to the siox-tests repo, but a
    // self-contained fixture keeps this integration test independent).
    let root = env!("CARGO_MANIFEST_DIR");
    let fixture = "tests/fixtures/counter_test.siox";
    let status = Command::new(siox)
        .current_dir(root)
        .args(["--test", fixture, "-o", out.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success(), "sioxc --test failed");
    assert!(out.exists(), "no binary produced");

    // The binary runs the testbench and exits 0 on PASS.
    let run = Command::new(&out)
        .args(["--vcd", vcd.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(run.success(), "native simulator returned {:?}", run.code());
    let trace = std::fs::read_to_string(&vcd).expect("native executable did not write VCD");
    assert!(trace.contains("$timescale 1fs $end"));
    assert!(trace.contains("$scope module CounterTest $end"));
    assert!(trace.contains("$scope module dut $end"));
    let signal_id = |name: &str| {
        trace
            .lines()
            .find(|line| line.starts_with("$var ") && line.split_whitespace().nth(4) == Some(name))
            .and_then(|line| line.split_whitespace().nth(3))
            .expect("signal missing from VCD declarations")
    };
    let clk_id = signal_id("clk");
    let count_id = signal_id("count");
    assert!(trace.contains("#0\n$dumpvars"));
    assert!(trace.contains(&format!("#5000000\n1{clk_id}")));
    assert!(trace.contains(&format!("#105000000\n1{clk_id}\nb00001010 {count_id}")));
    assert_eq!(
        trace.matches("#10000000\n").count(),
        1,
        "same-time changes must share one timestamp"
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn native_testbench_exchanges_more_than_two_words() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let stem = format!("siox_wide_{}", std::process::id());
    let source = std::env::temp_dir().join(format!("{stem}.siox"));
    let out = std::env::temp_dir().join(&stem);
    std::fs::write(
        &source,
        "module wide_test;
         using std::bits::unsigned;
         #[test] entity WideTest {}
         impl WideTest {
             let value: unsigned[192];
             value = 1020847100762815390427017310442723737601;
             assert!(value == 1020847100762815390427017310442723737601,
                     \"all three words survive\");
         }",
    )
    .unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .args([
            "--test",
            source.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "wide native build failed");
    let run = Command::new(&out).status().unwrap();
    assert!(run.success(), "wide native test returned {:?}", run.code());
    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(out);
}

#[test]
fn native_vcd_preserves_logic_metavalues_and_enum_symbols() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let stem = format!("siox_vcd_values_{}", std::process::id());
    let source = std::env::temp_dir().join(format!("{stem}.siox"));
    let out = std::env::temp_dir().join(&stem);
    let vcd = out.with_extension("vcd");
    std::fs::write(
        &source,
        "module vcd_values;
         using std::logic::Logic;
         using std::bits::unsigned;
         enum State { Idle, Run }
         entity Values {
             out scalar: Logic;
             out bus: unsigned[4];
             out state: State;
         }
         impl Values {
             scalar = 'Z';
             bus = \"1X0Z\";
             state = State::Run;
         }
         #[test] entity WaveTest {}
         impl WaveTest {
             let scalar: Logic;
             let bus: unsigned[4];
             let state: State;
             let dut: Values = {
                 .scalar = scalar,
                 .bus = bus,
                 .state = state,
             };
             assert!(scalar == 'Z', \"scalar setup\");
         }",
    )
    .unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .args([
            "--test",
            source.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "VCD value fixture failed to compile");
    let run = Command::new(&out)
        .args(["--vcd", vcd.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(run.success(), "VCD value fixture failed to run");
    let trace = std::fs::read_to_string(&vcd).unwrap();
    assert!(
        trace.contains("zv"),
        "Logic 'Z' was not emitted as z:\n{trace}"
    );
    assert!(
        trace.contains("b1x0z "),
        "Logic vector metavalues were not preserved:\n{trace}"
    );
    assert!(
        trace.contains("sRun "),
        "enum symbol was not preserved:\n{trace}"
    );
    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(out);
    let _ = std::fs::remove_file(vcd);
}

#[test]
fn nominal_time_and_real_frequency_run_natively() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let stem = format!("siox_units_{}", std::process::id());
    let source = std::env::temp_dir().join(format!("{stem}.siox"));
    let out = std::env::temp_dir().join(&stem);
    let vcd = out.with_extension("vcd");
    std::fs::write(
        &source,
        "module units;
         entity Units { out t: time; out f: frequency; }
         impl Units { t = 2ns; f = 2.5MHz; }
         #[test] entity UnitTest {}
         impl UnitTest {
             let t: time;
             let f: frequency;
             let dut: Units = { .t = t, .f = f, };
             assert!(t == 2ns, \"integer-backed time\");
             assert!(f == 2.5MHz, \"real-backed frequency\");
         }",
    )
    .unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .args([
            "--test",
            source.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "time/frequency fixture failed to compile");
    let run = Command::new(&out)
        .args(["--vcd", vcd.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(run.success(), "time/frequency fixture failed to run");
    let trace = std::fs::read_to_string(&vcd).unwrap();
    assert!(
        trace.lines().any(|line| line.starts_with("$var real 64")),
        "frequency did not retain real representation:\n{trace}"
    );
    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(out);
    let _ = std::fs::remove_file(vcd);
}

#[test]
fn extreme_layouts_fail_cleanly_instead_of_panicking() {
    let root = env!("CARGO_MANIFEST_DIR");
    let stem = format!("siox_extreme_layout_{}", std::process::id());
    let source = std::env::temp_dir().join(format!("{stem}.siox"));
    let object = std::env::temp_dir().join(format!("{stem}.o"));

    std::fs::write(
        &source,
        "module extreme_range;
         #[top] entity E {
             out y: Logic[-9223372036854775807..9223372036854775807];
         }
         impl E {}",
    )
    .unwrap();
    let range = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .args([source.to_str().unwrap(), "--emit", "ir"])
        .output()
        .unwrap();
    let range_stderr = String::from_utf8_lossy(&range.stderr);
    assert!(!range.status.success(), "impossible range was accepted");
    assert!(range_stderr.contains("range contains"), "{range_stderr}");
    assert!(!range_stderr.contains("panicked"), "{range_stderr}");

    std::fs::write(
        &source,
        "module extreme_width;
         #[top] entity E { out y: unsigned[4294967295]; }
         impl E { y = 0; }",
    )
    .unwrap();
    let width = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .args([
            source.to_str().unwrap(),
            "--emit",
            "object",
            "-o",
            object.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let width_stderr = String::from_utf8_lossy(&width.stderr);
    assert!(
        !width.status.success(),
        "unsupported LLVM width was accepted"
    );
    assert!(
        width_stderr.contains("LLVM backend supports integer values"),
        "{width_stderr}"
    );
    assert!(!width_stderr.contains("panicked"), "{width_stderr}");

    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(object);
}

#[test]
fn overflowing_native_duration_is_a_build_error() {
    let root = env!("CARGO_MANIFEST_DIR");
    let stem = format!("siox_duration_overflow_{}", std::process::id());
    let source = std::env::temp_dir().join(format!("{stem}.siox"));
    let output = std::env::temp_dir().join(&stem);
    std::fs::write(
        &source,
        "module duration_overflow;
         #[test] entity T {}
         impl T { await 18446744073709551615ns; }",
    )
    .unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .args([
            "--test",
            source.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        !result.status.success(),
        "overflowing duration was accepted"
    );
    assert!(stderr.contains("exceeds the native"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");

    std::fs::write(
        &source,
        "module duration_overflow;
         #[test] entity T {}
         impl T { await 18446744073709551616fs; }",
    )
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .args([
            "--test",
            source.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        !result.status.success(),
        "oversized duration value was accepted"
    );
    assert!(stderr.contains("does not fit"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");

    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(output);
}

#[test]
fn native_formatting_preserves_wide_unicode_and_long_messages() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let stem = format!("siox_wide_format_{}", std::process::id());
    let source = std::env::temp_dir().join(format!("{stem}.siox"));
    let output = std::env::temp_dir().join(&stem);
    let wide = "340282366920938463463374607431768211455";
    let long = format!("{}END", "x".repeat(600));
    std::fs::write(
        &source,
        format!(
            "module wide_format;
             #[test] entity T {{}}
             impl T {{
                 let wide: unsigned[128] = {wide};
                 let character: Char = 'λ';
                 print!(\"wide {{}} char {{}}\", wide, character);
                 warn!(false, \"{long} {{}}\", wide);
             }}"
        ),
    )
    .unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .args([
            "--test",
            source.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "wide formatting fixture failed to compile:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&output).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "formatting test failed:\n{stdout}");
    assert!(
        stdout.contains(&format!("wide {wide} char λ")),
        "formatted output was truncated or mis-encoded:\n{stdout}"
    );
    assert!(
        stderr.contains(&format!("{long} {wide}")),
        "warning buffer truncated the formatted message:\n{stderr}"
    );

    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(output);
}
