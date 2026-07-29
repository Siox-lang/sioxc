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
             using std::math::{{sqrt, pow, floor, PI}};
             const HALF_PI: real = PI / 2.0;
             const BYTE_RANGE: range = 7..0;
             fn half(value: real) -> real {{ return value / 2.0; }}
             fn one() -> real {{ return 1; }}
             enum Symbol {{ 'α', 'β' }}
             struct CharacterBox {{ value: Char }}
             struct RealBox {{ value: real }}
             struct WideBox {{ value: unsigned[128] }}
             struct NarrowWideBox {{ value: unsigned[80] }}
             struct WidePair {{ value: unsigned[128], character: Char }}
             impl RealBox {{
                 fn half(self) -> real {{ return self.value / 2.0; }}
             }}
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
