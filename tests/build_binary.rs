//! `siox build` produces a runnable native simulator binary (stage B5.1).
//! Only meaningful with the `llvm` feature + a clang toolchain.

use std::process::Command;

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

/// Decode an FST with the same upstream libfst reader that GTKWave uses. This
/// checks the complete block/hierarchy encoding, not a file signature.
fn decode_fst(fst: &std::path::Path) -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let decoder = fst.with_extension("fst-dump");
    let build = Command::new("clang")
        .arg(root.join("tests/fixtures/fst_dump.c"))
        .arg(root.join("third_party/libfst/fstapi.c"))
        .arg(root.join("third_party/libfst/fastlz.c"))
        .arg(root.join("third_party/libfst/lz4.c"))
        .arg("-I")
        .arg(root.join("third_party/libfst"))
        .args(["-O2", "-lz", "-o"])
        .arg(&decoder)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "libfst test decoder failed to build:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let decoded = Command::new(&decoder).arg(fst).output().unwrap();
    assert!(
        decoded.status.success(),
        "libfst rejected {}:\n{}",
        fst.display(),
        String::from_utf8_lossy(&decoded.stderr)
    );
    let _ = std::fs::remove_file(decoder);
    String::from_utf8(decoded.stdout).expect("libfst produced non-UTF-8 VCD")
}

fn waveform_times(trace: &str) -> Vec<u64> {
    trace
        .lines()
        .filter_map(|line| line.strip_prefix('#'))
        .map(|time| time.parse().expect("malformed waveform timestamp"))
        .collect()
}

#[cfg(unix)]
#[test]
fn native_output_path_does_not_need_to_be_utf8() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }

    let mut filename = format!("siox_non_utf8_{}", std::process::id()).into_bytes();
    filename.push(0xff);
    let output = std::env::temp_dir().join(std::ffi::OsString::from_vec(filename));
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["--test", "tests/fixtures/counter_test.siox", "-o"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "non-UTF-8 output path failed:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let run = Command::new(&output).output().unwrap();
    assert!(
        run.status.success(),
        "binary at non-UTF-8 path failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_file(output);
}

#[test]
fn native_local_names_are_isolated_from_each_other_and_the_harness() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }

    let stem = format!("siox_native_names_{}", std::process::id());
    let directory = std::env::temp_dir().join(&stem);
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("names.siox");
    let output = directory.join("names-test");
    std::fs::write(
        &source,
        r#"module native_names;
           using std::text::string;
           struct Left { c: unsigned[8] }
           struct Right { b_c: unsigned[8] }
           #[test] entity FlattenedNames {}
           impl FlattenedNames {
               let a_b: Left = { .c = 11 };
               let a: Right = { .b_c = 22 };
               assert!(a_b.c == 11 and a.b_c == 22,
                       "flattened paths stay distinct");
           }
           #[test] entity HarnessName {}
           impl HarnessName {
               let g_io_failed: unsigned[1] = 0;
               let missing: string = read<string>("not-there.txt");
           }"#,
    )
    .unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .arg("--test")
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "native name fixture failed to build:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let run = Command::new(&output).output().unwrap();
    let report =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(!run.status.success(), "missing fixture passed:\n{report}");
    assert!(
        report.contains("native_names::FlattenedNames ... ok"),
        "flattened-name test did not pass independently:\n{report}"
    );
    assert!(
        report.contains("native_names::HarnessName ... FAILED")
            && report.contains("read<string>")
            && report.contains("not-there.txt"),
        "harness helper was shadowed or failure was unclear:\n{report}"
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn test_no_run_builds_a_runnable_binary() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let siox = env!("CARGO_BIN_EXE_sioxc");
    let out = std::env::temp_dir().join(format!("siox_counter_{}", std::process::id()));
    let vcd = out.with_extension("vcd");
    let fst = out.with_extension("fst");

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
        .arg("--vcd")
        .arg(&vcd)
        .arg("--fst")
        .arg(&fst)
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
    let fst_trace = decode_fst(&fst);
    assert!(fst_trace.contains("$scope module CounterTest $end"));
    assert!(fst_trace.contains("$scope module dut $end"));
    assert!(fst_trace.contains("b00001010"));
    assert_eq!(
        waveform_times(&fst_trace),
        waveform_times(&trace),
        "FST and VCD must sample the same scheduler-side change points"
    );

    let fst_equals = out.with_extension("equals.fst");
    let equals = Command::new(&out)
        .arg("examples::counter_test::CounterTest")
        .arg(format!("--fst={}", fst_equals.display()))
        .output()
        .unwrap();
    assert!(
        equals.status.success(),
        "--fst=<path> or filtering failed:\n{}{}",
        String::from_utf8_lossy(&equals.stdout),
        String::from_utf8_lossy(&equals.stderr)
    );
    assert!(decode_fst(&fst_equals).contains("#105000000"));

    let missing = Command::new(&out).arg("--fst").output().unwrap();
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--fst requires a path"));

    let same_path = out.with_extension("same.wave");
    let same = Command::new(&out)
        .arg("--vcd")
        .arg(&same_path)
        .arg("--fst")
        .arg(&same_path)
        .output()
        .unwrap();
    assert_eq!(same.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&same.stderr)
        .contains("VCD and FST outputs must use different paths"));
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&vcd);
    let _ = std::fs::remove_file(&fst);
    let _ = std::fs::remove_file(&fst_equals);
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
fn native_range_checks_each_clock_settle() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let stem = format!("siox_transient_range_{}", std::process::id());
    let source = std::env::temp_dir().join(format!("{stem}.siox"));
    let out = std::env::temp_dir().join(&stem);
    std::fs::write(
        &source,
        "module transient_range;
         entity PulseRange {
             clk: Bit in,
             bad: integer<0..3> in,
             n: integer<0..1> out
         }
         impl PulseRange {
             let value: integer<0..1> = 0;
             if clk.rising() { value = bad; }
             if clk.falling() { value = 0; }
             n = value;
         }
         #[test] entity RangeTest {}
         impl RangeTest {
             let clk: Bit = '0';
             let bad: integer<0..3> = 2;
             let n: integer<0..1>;
             let dut: PulseRange = { .clk = clk, .bad = bad, .n = n };
             clk = not clk after 1ns;
             await 2ns;
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
    assert!(status.success(), "range fixture failed to compile");
    let run = Command::new(&out).output().unwrap();
    assert!(
        !run.status.success(),
        "a transient out-of-range clock state was silently missed"
    );
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(output.contains("left its range 0..1"), "{output}");
    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(out);
}

#[test]
fn native_range_checks_external_stimulus_before_truncation() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let stem = format!("siox_input_range_{}", std::process::id());
    let source = std::env::temp_dir().join(format!("{stem}.siox"));
    let out = std::env::temp_dir().join(&stem);
    std::fs::write(
        &source,
        "module input_range;
         entity InputRange {
             x: integer<0..1> in,
             y: integer<0..1> out
         }
         impl InputRange { y = x; }
         #[test] entity RangeTest {}
         impl RangeTest {
             let wide: integer<0..3> = 2;
             let x: integer<0..1>;
             let y: integer<0..1>;
             let dut: InputRange = { .x = x, .y = y };
             x = wide;
             await 1ns;
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
    assert!(status.success(), "input range fixture failed to compile");
    let run = Command::new(&out).output().unwrap();
    assert!(
        !run.status.success(),
        "an external out-of-range value was truncated before validation"
    );
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(output.contains("left its range 0..1"), "{output}");
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
    let fst = out.with_extension("fst");
    std::fs::write(
        &source,
        "module vcd_values;
         using std::logic::Logic;
         using std::bits::unsigned;
         enum State { Idle, Run }
         entity Values {
             scalar: Logic out,
             bus: unsigned[4] out,
             state: State out,
             wide: unsigned[192] out,
             real_value: real out
         }
         impl Values {
             scalar = 'Z';
             bus = \"1X0Z\";
             state = State::Run;
             wide = 6277101735386680763835789423207666416102355444464034512895;
             real_value = 2.5;
         }
         #[test] entity WaveTest {}
         impl WaveTest {
             let scalar: Logic;
             let bus: unsigned[4];
             let state: State;
             let wide: unsigned[192];
             let real_value: real;
             let dut: Values = {
                 .scalar = scalar,
                 .bus = bus,
                 .state = state,
                 .wide = wide,
                 .real_value = real_value,
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
        .arg("--vcd")
        .arg(&vcd)
        .arg("--fst")
        .arg(&fst)
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
    let wide_bits = format!("b{} ", "1".repeat(192));
    assert!(
        trace.contains(&wide_bits),
        "wide VCD value was truncated:\n{trace}"
    );
    assert!(trace.contains("r2.5 "), "real VCD value was lost:\n{trace}");
    let fst_trace = decode_fst(&fst);
    assert!(fst_trace.contains('z'), "FST lost Logic 'Z':\n{fst_trace}");
    assert!(
        fst_trace.contains("b1x0z "),
        "FST lost vector metavalues:\n{fst_trace}"
    );
    assert!(
        fst_trace.contains("sRun "),
        "FST lost the symbolic enum value:\n{fst_trace}"
    );
    assert!(
        fst_trace.contains(&wide_bits),
        "FST truncated a multiword value:\n{fst_trace}"
    );
    assert!(
        fst_trace.contains("r2.5 "),
        "FST lost a real value:\n{fst_trace}"
    );
    assert_eq!(waveform_times(&fst_trace), waveform_times(&trace));
    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(out);
    let _ = std::fs::remove_file(vcd);
    let _ = std::fs::remove_file(fst);
}

#[test]
fn native_fst_keeps_multiple_tests_on_one_monotonic_timeline() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let stem = format!("siox_fst_multitest_{}", std::process::id());
    let source = std::env::temp_dir().join(format!("{stem}.siox"));
    let out = std::env::temp_dir().join(&stem);
    let vcd = out.with_extension("vcd");
    let fst = out.with_extension("fst");
    std::fs::write(
        &source,
        "module fst_multitest;
         entity Echo { x: Bit in, y: Bit out }
         impl Echo { y = x; }
         #[test] entity First {}
         impl First {
             let x: Bit = '0';
             let y: Bit;
             let dut: Echo = { .x = x, .y = y };
             x = '1';
             await 2fs;
         }
         #[test] entity Second {}
         impl Second {
             let x: Bit = '0';
             let y: Bit;
             let dut: Echo = { .x = x, .y = y };
             x = '1';
             await 3fs;
         }",
    )
    .unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .arg("--test")
        .arg(&source)
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "multi-test FST fixture failed to build:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&out)
        .arg("--vcd")
        .arg(&vcd)
        .arg("--fst")
        .arg(&fst)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "multi-test FST fixture failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let vcd_trace = std::fs::read_to_string(&vcd).unwrap();
    let fst_trace = decode_fst(&fst);
    let times = waveform_times(&fst_trace);
    assert_eq!(times, waveform_times(&vcd_trace));
    assert_eq!(times.first(), Some(&0));
    assert!(
        times.windows(2).all(|window| window[0] < window[1]),
        "multi-test FST timeline was not strictly monotonic: {times:?}"
    );
    assert!(fst_trace.contains("$scope module First $end"));
    assert!(fst_trace.contains("$scope module Second $end"));

    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(out);
    let _ = std::fs::remove_file(vcd);
    let _ = std::fs::remove_file(fst);
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
         entity Units { t: time out, f: frequency out }
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
             y: Logic[-9223372036854775807..9223372036854775807] out,
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
         #[top] entity E { y: unsigned[4294967295] out, }
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
             using std::math::{{sqrt, pow, floor, PI}};
             const HALF_PI: real = PI / 2.0;
             const BYTE_RANGE: range = 7..0;
             const NEGATIVE: integer = -8;
             fn half(value: real) -> real {{ return value / 2.0; }}
             fn one() -> real {{ return 1; }}
             enum Symbol {{ 'α', 'β' }}
             struct CharacterBox {{ value: Char }}
             struct RealBox {{ value: real }}
             struct WideBox {{ value: unsigned[128] }}
             struct NarrowWideBox {{ value: unsigned[80] }}
             struct WidePair {{ value: unsigned[128], character: Char }}
             struct NestedBox {{
                 character: CharacterBox,
                 values: unsigned[8][2],
                 word: string[3]
             }}
             impl RealBox {{
                 fn half(self) -> real {{ return self.value / 2.0; }}
             }}
             entity SignedSource {{ value: integer<-10..10> out, }}
             impl SignedSource {{ value = -3; }}
             #[test] entity T {{}}
             impl T {{
                 let wide: unsigned[128] = {wide};
                 let character: Char = 'λ';
                 let text: string = \"hé🙂\";
                 let copied_text: string = text;
                 let empty: string = \"\";
                 let other_empty: string = \"\";
                 let mutable_text: string = \"old\";
                 let scalar_character: Char = 'λ';
                 let symbol: Symbol = 'α';
                 let boxed: CharacterBox = {{ .value = 'λ' }};
                 let real_value: real = 1.5;
                 let signed_value: integer = -8;
                 let connected_signed: integer<-10..10>;
                 let signed_source: SignedSource = {{ .value = connected_signed }};
                 let real_box: RealBox = {{ .value = 3.5 }};
                 let math_result: real = sqrt(9.0);
                 let wide_box: WideBox = {{ .value = {wide} }};
                 let narrow_wide_box: NarrowWideBox = {{ .value = 1208925819614629174706175 }};
                 let wide_values: unsigned[128][2] = [{wide}, 1];
                 let copied_values: unsigned[128][2] = wide_values;
                 let matrix: unsigned[128][2][2] = [[1, 2], [3, {wide}]];
                 let copied_matrix: unsigned[128][2][2] = matrix;
                 let pairs: WidePair[2] = [
                     {{ .value = 5, .character = 'λ' }},
                     {{ .value = {wide}, .character = 'β' }}
                 ];
                 let words: string[3][2] = [\"abc\", \"def\"];
                 let nested_box: NestedBox = {{
                     .character = CharacterBox {{ .value = 'λ' }},
                     .values = [7, 8],
                     .word = \"box\"
                 }};
                 let copied_nested_box: NestedBox = nested_box;
                 let ranged_wide: unsigned[127..0] = {wide};
                 let descending_bits: Bit[3..0] = \"1010\";
                 let named_bits: Bit[BYTE_RANGE] = \"11001010\";
                 let one_count: unsigned[4] = 0;
                 mutable_text = copied_text;
                 print!(\"wide {{}} char {{}}\", wide, character);
                 print!(\"strings {{}} {{}} <{{}}>\", \"literal\", text, empty);
                 assert!(empty == \"\", \"empty string vs literal\");
                 assert!(empty == other_empty, \"empty string locals\");
                 assert!(\"constant\" == \"constant\", \"string literals compare\");
                 assert!(\"left\" != \"right\", \"different string literals compare\");
                 assert!(copied_text == text, \"string initializer copy\");
                 assert!(mutable_text == \"hé🙂\", \"string local copy assignment\");
                 mutable_text = \"new\";
                 assert!(mutable_text == \"new\", \"string literal assignment\");
                 assert!(scalar_character == 'λ', \"local Char comparison\");
                 scalar_character = 'β';
                 assert!(scalar_character == 'β', \"local Char assignment\");
                 text[0] = 'H';
                 assert!(text[0] == 'H', \"string element Char assignment\");
                 assert!(boxed.value == 'λ', \"struct Char field initializer\");
                 boxed.value = 'β';
                 assert!(boxed.value == 'β', \"struct Char field assignment\");
                 symbol = 'β';
                 assert!(symbol == 'β', \"local enum symbol assignment\");
                 print!(\"reals {{}} {{}}\", real_value, real_box.value);
                 print!(\"signed integer {{}}\", signed_value);
                 print!(\"signed values {{}} {{}}\", NEGATIVE, connected_signed);
                 assert!(NEGATIVE / 3 == -2, \"signed constant arithmetic\");
                 assert!(connected_signed < 0, \"connected signed comparison\");
                 assert!(connected_signed / 2 == -1, \"connected signed division\");
                 print!(\"real comparison {{}}\", real_value == 1.5);
                 real_value = real_value + 0.75;
                 assert!(real_value == 2.25, \"real local arithmetic {{}}\", real_value);
                 real_value = -real_value;
                 assert!(real_value == -2.25, \"real local negation {{}}\", real_value);
                 real_value = if 1 == 1 {{ 4.5 }} else {{ 9.5 }};
                 assert!(real_value == 4.5, \"real conditional {{}}\", real_value);
                 real_value = match 1 {{ 1 => 5.5, _ => 9.5 }};
                 assert!(real_value == 5.5, \"real match {{}}\", real_value);
                 real_box.value = real_box.value + 0.5;
                 assert!(real_box.value == 4.0, \"real struct field {{}}\", real_box.value);
                 assert!(math_result == 3.0, \"native extern sqrt {{}}\", math_result);
                 math_result = pow(2.0, 3.0);
                 assert!(math_result == 8.0, \"native extern pow {{}}\", math_result);
                 print!(\"extern floor {{}}\", floor(3.75));
                 assert!(HALF_PI > 1.5, \"named real constant {{}}\", HALF_PI);
                 assert!(half(8.0) == 4.0, \"real function parameter/return {{}}\", half(8.0));
                 assert!(one() == 1.0, \"integer literal real return {{}}\", one());
                 assert!(real_box.half() == 2.0, \"real method return {{}}\", real_box.half());
                 assert!(wide_box.value == {wide}, \"wide struct field {{}}\", wide_box.value);
                 narrow_wide_box.value = narrow_wide_box.value + 1;
                 assert!(narrow_wide_box.value == 0, \"80-bit field wraps {{}}\", narrow_wide_box.value);
                 assert!(wide_values[0] == {wide}, \"wide array element {{}}\", wide_values[0]);
                 assert!(copied_values[0] == wide_values[0], \"wide array copy\");
                 wide_values = [3, 4];
                 assert!(wide_values[1] == 4, \"wide array literal reassignment\");
                 assert!(matrix[1][1] == {wide}, \"nested wide array\");
                 assert!(copied_matrix[1][1] == matrix[1][1], \"nested array copy\");
                 matrix[0][1] = 9;
                 assert!(matrix[0][1] == 9, \"nested array mutation\");
                 assert!(pairs[1].value == {wide}, \"array of structs\");
                 assert!(pairs[0].character == 'λ', \"array struct Char field\");
                 words[1] = \"xyz\";
                 assert!(words[0] == \"abc\", \"string array first\");
                 assert!(words[1] == \"xyz\", \"string array assignment\");
                 assert!(nested_box.character.value == 'λ', \"nested struct initializer\");
                 assert!(nested_box.values[1] == 8, \"array struct field initializer\");
                 assert!(nested_box.word == \"box\", \"string struct field initializer\");
                 assert!(copied_nested_box.character.value == 'λ', \"nested struct copy\");
                 assert!(copied_nested_box.values[0] == 7, \"array field copy\");
                 assert!(copied_nested_box.word == \"box\", \"string field copy\");
                 assert!(ranged_wide == {wide}, \"ranged wide local\");
                 assert!(ranged_wide'length == 128, \"ranged wide length\");
                 assert!(ranged_wide'left == 127, \"ranged wide left\");
                 assert!(ranged_wide'right == 0, \"ranged wide right\");
                 assert!(ranged_wide'ascending == false, \"descending wide range\");
                 assert!(descending_bits[3] == '1', \"descending array left element\");
                 assert!(descending_bits[0] == '0', \"descending array right element\");
                 assert!(descending_bits'left == 3, \"descending array left\");
                 assert!(descending_bits'right == 0, \"descending array right\");
                 assert!(named_bits[7] == '1', \"named range left element\");
                 assert!(named_bits[0] == '0', \"named range right element\");
                 for bit in descending_bits {{
                     if bit == '1' {{ one_count = one_count + 1; }}
                 }}
                 assert!(one_count == 2, \"ranged array iteration {{}}\", one_count);
                 warn!(false, \"{long} {{}}\", wide);
                 warn!(false, \"string argument {{}}\", text);
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
        stdout.contains("strings literal hé🙂 <>"),
        "string arguments were not formatted as Unicode text:\n{stdout}"
    );
    assert!(
        stdout.contains("reals 1.5 3.5"),
        "real locals or fields were not formatted as floating-point values:\n{stdout}"
    );
    assert!(
        stdout.contains("real comparison 1"),
        "a real comparison result was incorrectly formatted as a real:\n{stdout}"
    );
    assert!(
        stdout.contains("signed integer -8"),
        "signed integer formatting lost its sign:\n{stdout}"
    );
    assert!(
        stdout.contains("signed values -8 -3"),
        "signed constants or constrained signals formatted as unsigned:\n{stdout}"
    );
    assert!(
        stdout.contains("extern floor 3"),
        "an extern real return was not formatted correctly:\n{stdout}"
    );
    assert!(
        stderr.contains(&format!("{long} {wide}")),
        "warning buffer truncated the formatted message:\n{stderr}"
    );
    assert!(
        stderr.contains("string argument Hé🙂"),
        "warning did not format its string argument:\n{stderr}"
    );

    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(output);
}

#[test]
fn native_string_reassignment_rejects_storage_length_mismatch() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let stem = format!("siox_string_shape_{}", std::process::id());
    let source = std::env::temp_dir().join(format!("{stem}.siox"));
    let output = std::env::temp_dir().join(&stem);
    std::fs::write(
        &source,
        "module string_shape;
         #[test] entity T {}
         impl T {
             let text: string = \"abc\";
             text = \"x\";
         }",
    )
    .unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "--test",
            source.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        !build.status.success(),
        "mismatched string storage was accepted"
    );
    assert!(
        stderr.contains("error[E-P003]: cannot assign Char[1] to Char[3]"),
        "{stderr}"
    );
    assert!(
        stderr.contains("later stages skipped"),
        "the mismatch should be rejected before native code generation:\n{stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");

    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(output);
}

#[test]
fn numeric_match_ranges_preserve_signed_and_real_domains() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let stem = format!("siox_numeric_match_{}", std::process::id());
    let source = std::env::temp_dir().join(format!("{stem}.siox"));
    let output = std::env::temp_dir().join(&stem);
    std::fs::write(
        &source,
        r#"module numeric_match;
         using std::bits::signed;
         using std::bits::unsigned;
         entity NumericMatch {
             int_value: integer in,
             signed_value: signed[8] in,
             real_value: real in,
             int_hit: Bit out,
             descending_hit: Bit out,
             signed_hit: Bit out,
             real_hit: Bit out
         }
         impl NumericMatch {
             int_hit = match int_value { -4..-1 => '1', _ => '0' };
             descending_hit = match int_value { -1..-4 => '1', _ => '0' };
             signed_hit = match signed_value { -4..-1 => '1', _ => '0' };
             real_hit = match real_value { -2..1 => '1', _ => '0' };
         }
         #[test] entity NumericMatchTest {}
         impl NumericMatchTest {
             let int_value: integer = -2;
             let signed_value: signed[8] = -3;
             let real_value: real = -1.5;
             let int_hit: Bit;
             let descending_hit: Bit;
             let signed_hit: Bit;
             let real_hit: Bit;
             let dut: NumericMatch = {
                 .int_value = int_value,
                 .signed_value = signed_value,
                 .real_value = real_value,
                 .int_hit = int_hit,
                 .descending_hit = descending_hit,
                 .signed_hit = signed_hit,
                 .real_hit = real_hit
             };
             let local_int_hit: Bit = match int_value { -4..-1 => '1', _ => '0' };
             let local_descending_hit: Bit = match int_value { -1..-4 => '1', _ => '0' };
             let local_signed_hit: Bit = match signed_value { -4..-1 => '1', _ => '0' };
             let local_real_hit: Bit = match real_value { -2..1 => '1', _ => '0' };
             let wide_value: unsigned[128] = 1267650600228229401496703205376;
             let wide_statement_hit: Bit = '1';
             match wide_value { 0 => { wide_statement_hit = '0'; }, _ => {} }
             let wide_expression_hit: Bit = match wide_value { 0 => '0', _ => '1' };
             assert!(int_hit == '1', "hardware integer negative range");
             assert!(descending_hit == '1', "hardware descending range");
             assert!(signed_hit == '1', "hardware signed negative range");
             assert!(real_hit == '1', "hardware real range");
             assert!(local_int_hit == '1', "testbench integer negative range");
             assert!(local_descending_hit == '1', "testbench descending range");
             assert!(local_signed_hit == '1', "testbench signed negative range");
             assert!(local_real_hit == '1', "testbench real range");
             assert!(wide_statement_hit == '1', "wide statement scrutinee is not truncated");
             assert!(wide_expression_hit == '1', "wide expression scrutinee is not truncated");
             int_value = 2;
             signed_value = 3;
             real_value = 1.5;
             await 1fs;
             assert!(int_hit == '0', "hardware integer range miss");
             assert!(descending_hit == '0', "hardware descending range miss");
             assert!(signed_hit == '0', "hardware signed range miss");
             assert!(real_hit == '0', "hardware real range miss");
         }"#,
    )
    .unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
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
        "numeric-match fixture failed to compile:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&output).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success(),
        "numeric-match fixture failed:\n{stdout}{stderr}"
    );

    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(output);
}

#[test]
fn hardware_block_locals_are_scoped_immediate_values() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let stem = format!("siox_hardware_block_locals_{}", std::process::id());
    let source = std::env::temp_dir().join(format!("{stem}.siox"));
    let output = std::env::temp_dir().join(&stem);
    std::fs::write(
        &source,
        r#"module hardware_block_locals;
         using std::bits::unsigned;
         struct Pair { first: unsigned[8], second: unsigned[8] }
         entity LocalHardware {
             clk: Bit in,
             select: Bit in,
             a: unsigned[8] in,
             b: unsigned[8] in,
             comb: unsigned[8] out,
             chosen: unsigned[8] out,
             nested: unsigned[8] out,
             aggregate: unsigned[8] out,
             indexed: unsigned[8] out,
             sliced: unsigned[8] out,
             left: unsigned[8] out,
             right: unsigned[8] out
         }
         impl LocalHardware {
             let x: unsigned[8] = 10;
             let y: unsigned[8] = 20;
             if select == '1' {
                 let value: unsigned[8] = a;
                 value = value + 1;
                 comb = value + 1;
             } else {
                 let a: unsigned[8] = b;
                 comb = a + 2;
             }
             if select == '1' {
                 let adjusted: unsigned[8] = a;
                 if b == 7 {
                     adjusted = adjusted + 2;
                 }
                 nested = adjusted;
             } else {
                 nested = b;
             }
             match select {
                 '0' => {
                     let value: unsigned[8] = b;
                     chosen = value + 3;
                 }
                 _ => {
                     let value: unsigned[8] = a;
                     chosen = value + 4;
                 }
             }
             if 1 == 1 {
                 let pair: Pair = Pair { .first = a, .second = b };
                 pair.first = pair.second + 1;
                 aggregate = pair.first;
                 let table: unsigned[8][2] = [a, b];
                 table[0] = table[1] + 2;
                 let dynamic_index: unsigned[1] = unsigned[1](select);
                 indexed = table[dynamic_index];
                 let word: unsigned[8] = a;
                 word[3..0] = b[3..0];
                 sliced = word;
             }
             if clk.rising() {
                 let temporary: unsigned[8] = x;
                 x = y;
                 y = temporary;
             }
             left = x;
             right = y;
         }
         #[test] entity BlockLocalHardwareTest {}
         impl BlockLocalHardwareTest {
             let clk: Bit = '0';
             let select: Bit = '1';
             let a: unsigned[8] = 250;
             let b: unsigned[8] = 7;
             let comb: unsigned[8];
             let chosen: unsigned[8];
             let nested: unsigned[8];
             let aggregate: unsigned[8];
             let indexed: unsigned[8];
             let sliced: unsigned[8];
             let left: unsigned[8];
             let right: unsigned[8];
             let dut: LocalHardware = {
                 .clk = clk,
                 .select = select,
                 .a = a,
                 .b = b,
                 .comb = comb,
                 .chosen = chosen,
                 .nested = nested,
                 .aggregate = aggregate,
                 .indexed = indexed,
                 .sliced = sliced,
                 .left = left,
                 .right = right
             };
             await 1fs;
             assert!(comb == 252, "if-local reassignment is immediate");
             assert!(chosen == 254, "match-arm local reaches its assignment");
             assert!(nested == 252, "nested branch mutates its outer local");
             assert!(aggregate == 8, "struct local field assignment is immediate");
             assert!(indexed == 7, "array local supports a dynamic read");
             assert!(sliced == 247, "packed local supports a slice assignment");
             assert!(left == 10 and right == 20, "register initial values");
             select = '0';
             await 1fs;
             assert!(comb == 9, "else-local shadows the then-local");
             assert!(chosen == 10, "other match arm has its own scope");
             assert!(nested == 7, "nested local update keeps its branch condition");
             assert!(indexed == 9, "array element assignment feeds dynamic selection");
             clk = '1';
             await 1fs;
             assert!(left == 20 and right == 10,
                     "local reads immediately while signal writes are next-state");
         }"#,
    )
    .unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
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
        "block-local fixture failed to compile:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&output).output().unwrap();
    assert!(
        run.status.success(),
        "block-local fixture failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(output);
}

#[test]
fn nested_generic_type_arguments_build_and_run() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let stem = format!("siox_nested_generic_types_{}", std::process::id());
    let source = std::env::temp_dir().join(format!("{stem}.siox"));
    let output = std::env::temp_dir().join(&stem);
    std::fs::write(
        &source,
        r#"module nested_generic_types;
         using std::bits::unsigned;
         struct Box<T> { value: T }
         entity Pass<T> { input: T in, output: T out }
         impl<T> Pass<T> { output = input; }
         entity NestedUse {
             input: unsigned[8] in,
             output: unsigned[8] out
         }
         impl NestedUse {
             let source: Box<Box<unsigned[8]>>;
             source = Box<Box<unsigned[8]>> {
                 .value = Box<unsigned[8]> { .value = input }
             };
             let passed: Box<Box<unsigned[8]>>;
             let pass: Pass<Box<Box<unsigned[8]>>> = {
                 .input = source,
                 .output = passed
             };
             output = passed.value.value;
         }
         #[test] entity NestedGenericTest {}
         impl NestedGenericTest {
             let input: unsigned[8] = 37;
             let output: unsigned[8];
             let dut: NestedUse = { .input = input, .output = output };
             await 1fs;
             assert!(output == 37,
                     "nested type arguments survive entity specialization");
         }"#,
    )
    .unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
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
        "nested-generic fixture failed to compile:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&output).output().unwrap();
    assert!(
        run.status.success(),
        "nested-generic fixture failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(output);
}
