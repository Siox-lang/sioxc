//! Source metadata must describe the direct LLVM pipeline and real storage.
#![cfg(feature = "cli")]

use std::process::{Command, Output};

fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "{}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn expression_inline_calls_keep_source_frames_and_value_bindings() {
    let directory = std::env::temp_dir().join(format!("siox_dwarf_values_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("values.siox");
    let text = r#"module debug_values;
use std::bits::unsigned;
use std::primitive::string;
struct Payload { pub tag: integer, pub gain: real, pub wide: unsigned[81] }
fn inspect_payload(value: Payload) -> integer {
    let copy: Payload = value;
    return copy.tag + integer(copy.gain); // expression payload return stop
}
fn count(value: string) -> integer {
    let copy: string = value;
    return copy'length; // expression string return stop
}
fn echo_text(value: string) -> string {
    let copy: string = value;
    return copy; // borrowed string return stop
}
fn echo(value: Logic) -> Logic {
    return value; // packed element return stop
}
#[inline(never)]
fn bump(value: integer) -> integer {
    let local: integer = value + 1;
    return local + 1; // value return stop
}
fn outer(value: integer) -> integer {
    let local: integer = 100;
    let computed: integer = bump(value); // nested value call
    return computed + local; // nested value caller stop
}
#[test] entity T {}
impl T {
    stimulus: process {
        let start: integer = 5;
        let first: integer = bump(start); // first value call
        let second: integer = bump(start + 1); let third: integer = bump(start + 2); // paired value calls
        let nested: integer = outer(start + 3); // outer value call
        let conditional: integer = if start > 0 { bump(start + 4) } else { bump(99) }; // selected value call
        assert!(first == 7 and second == 8 and third == 9 and nested == 110 and conditional == 11, "value caller stop");
        let sample: unsigned[8] = 130;
        let bit: Logic = sample[7];
        let echoed: Logic = echo(sample[7]);
        let tap: Logic = sample[7] xor sample[5];
        assert!(bit == '1' and echoed == '1' and tap == '1', "packed element arguments retain source discriminants");
        let text: string = read<string>("argument.txt");
        let borrowed: string = echo_text(text); // expression borrowed string call
        let runtime_length: integer = count(borrowed); // expression runtime string call
        let literal_length: integer = count("μ\0🙂"); // expression literal string call
        let empty_length: integer = count(""); // expression empty string call
        let packet: Payload = { .tag = 9, .gain = 1.5, .wide = 18446744073709551617 };
        let payload_result: integer = inspect_payload(packet); // expression payload call
        assert!(runtime_length == 3, "runtime expression string length");
        assert!(borrowed == text, "borrowed expression string result");
        assert!(borrowed[0] == 'β' and integer(borrowed[1]) == 0, "borrowed expression string indexing");
        assert!(borrowed == "β\0🙂", "borrowed expression string content");
        assert!(literal_length == 3, "literal expression string length");
        assert!(empty_length == 0, "empty expression string length");
        assert!(payload_result == 10, "expression aggregate result");
    }
}"#;
    std::fs::write(&source, text).unwrap();
    std::fs::write(directory.join("argument.txt"), "β\0🙂").unwrap();
    let debug = directory.join("debug.bin");
    let ordinary = directory.join("ordinary.bin");
    for (binary, debugging) in [(&debug, true), (&ordinary, false)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
        command.args(["--std", "std", "--test"]);
        if debugging {
            command.arg("--debug");
        }
        success(command.arg(&source).arg("-o").arg(binary).output().unwrap());
        success(
            Command::new(binary)
                .current_dir(&directory)
                .arg("-o")
                .arg(binary.with_extension("vcd"))
                .output()
                .unwrap(),
        );
    }
    assert_eq!(
        std::fs::read(debug.with_extension("vcd")).unwrap(),
        std::fs::read(ordinary.with_extension("vcd")).unwrap()
    );
    if Command::new("llvm-dwarfdump")
        .arg("--version")
        .output()
        .is_ok()
    {
        success(
            Command::new("llvm-dwarfdump")
                .arg("--verify")
                .arg(&debug)
                .output()
                .unwrap(),
        );
    }
    if Command::new("gdb").arg("--version").output().is_ok() {
        let line = |marker| text.lines().position(|line| line.contains(marker)).unwrap() + 1;
        let python = format!(
            r#"import gdb
stop = gdb.Breakpoint({source:?} + ':{return_line}')
echo_stop = gdb.Breakpoint({source:?} + ':{echo_return_line}')
string_stop = gdb.Breakpoint({source:?} + ':{string_return_line}')
borrowed_stop = gdb.Breakpoint({source:?} + ':{borrowed_return_line}')
payload_stop = gdb.Breakpoint({source:?} + ':{payload_return_line}')
assert not stop.pending, 'expression-only callee has no compiled source location'
gdb.execute('run')
for expected, caller_line in [(5, {first_line}), (6, {paired_line}), (7, {paired_line}), (8, {nested_line}), (9, {selected_line})]:
    frame = gdb.selected_frame()
    assert frame.name() == 'debug_values::bump', frame.name()
    assert int(gdb.parse_and_eval('value')) == expected
    assert int(gdb.parse_and_eval('local')) == expected + 1
    parent = frame.older()
    assert parent.find_sal().line == caller_line, (parent.find_sal().line, caller_line)
    parent.select()
    if expected == 8:
        assert parent.name() == 'debug_values::outer', parent.name()
        assert int(gdb.parse_and_eval('value')) == 8
        assert int(gdb.parse_and_eval('local')) == 100
        assert parent.older().name() == 'T::stimulus'
        assert parent.older().find_sal().line == {outer_line}
    else:
        assert parent.name() == 'T::stimulus', parent.name()
        assert int(gdb.parse_and_eval('start')) == 5
    frame.select()
    if expected == 8:
        outer_stop = gdb.Breakpoint({source:?} + ':{outer_return_line}')
    gdb.execute('continue')
    if expected == 8:
        assert gdb.selected_frame().name() == 'debug_values::outer'
        assert int(gdb.parse_and_eval('value')) == 8
        assert int(gdb.parse_and_eval('local')) == 100
        assert int(gdb.parse_and_eval('computed')) == 10
        outer_stop.delete()
        gdb.execute('continue')
stop.delete()
assert gdb.selected_frame().name() == 'debug_values::echo'
actual = int(gdb.parse_and_eval('value'))
assert 'value =' in gdb.execute('info args', to_string=True)
gdb.selected_frame().older().select()
assert actual == int(gdb.parse_and_eval('bit'))
echo_stop.delete()
gdb.execute('continue')
def check_string(first):
    for name in ['value', 'copy']:
        view = gdb.parse_and_eval(name)
        assert int(view['length']) == (0 if first is None else 3)
        assert int(view['byte_length']) == (0 if first is None else 7)
        if first is None:
            assert int(view['data']) == 0 and int(view['utf8']) == 0
        else:
            assert [int(view['data'][i]) for i in range(3)] == [first, 0, 0x1f642]
            assert int(view['utf8'][2]) == 0
assert gdb.selected_frame().name() == 'debug_values::echo_text'
assert gdb.selected_frame().older().find_sal().line == {borrowed_call_line}
check_string(0x3b2)
borrowed_stop.delete()
gdb.execute('continue')
for first, caller_line in [(0x3b2, {runtime_string_line}), (0x3bc, {literal_string_line}), (None, {empty_string_line})]:
    actual = gdb.selected_frame()
    assert actual.name() == 'debug_values::count', actual.name()
    assert actual.older().find_sal().line == caller_line
    assert 'value =' in gdb.execute('info args', to_string=True)
    check_string(first)
    gdb.execute('continue')
string_stop.delete()
assert gdb.selected_frame().name() == 'debug_values::inspect_payload'
assert gdb.selected_frame().older().find_sal().line == {payload_call_line}
for name in ['value', 'copy']:
    view = gdb.parse_and_eval(name)
    assert int(view['tag']) == 9 and float(view['gain']) == 1.5
    assert int(view['wide']) == (1 << 64) + 1
payload_stop.delete()
gdb.execute('continue')
assert int(gdb.parse_and_eval('$_exitcode')) == 0
"#,
            source = source.to_string_lossy(),
            return_line = line("value return stop"),
            echo_return_line = line("packed element return stop"),
            string_return_line = line("expression string return stop"),
            borrowed_return_line = line("borrowed string return stop"),
            borrowed_call_line = line("expression borrowed string call"),
            payload_return_line = line("expression payload return stop"),
            runtime_string_line = line("expression runtime string call"),
            literal_string_line = line("expression literal string call"),
            empty_string_line = line("expression empty string call"),
            payload_call_line = line("expression payload call"),
            first_line = line("first value call"),
            paired_line = line("paired value calls"),
            nested_line = line("nested value call"),
            outer_line = line("outer value call"),
            outer_return_line = line("nested value caller stop"),
            selected_line = line("selected value call")
        );
        success(
            Command::new("gdb")
                .args(["-nx", "-q", "--batch"])
                .arg(&debug)
                .args([
                    "-ex",
                    "set disable-randomization off",
                    "-ex",
                    "set breakpoint pending off",
                    "-ex",
                    &format!("cd {}", directory.display()),
                    "-ex",
                    &format!("python {python}"),
                ])
                .output()
                .unwrap(),
        );
    } else {
        eprintln!("GDB unavailable: expression native execution checked, debugger probe skipped");
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn expression_branches_preserve_shadowing_and_selected_source_stops() {
    let directory =
        std::env::temp_dir().join(format!("siox_dwarf_branches_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let text = r#"module debug_branches;
struct Pair { pub first: integer, pub second: integer }
#[inline(never)]
fn choose(value: integer) -> integer {
    let local: integer = 100;
    if value > 0 {
        let local: integer = value + 10;
        if value > 3 {
            let local: integer = value + 30;
            return local; // branch nested return
        }
        return local; // branch inner return
    }
    return local; // branch outer return
}
#[inline(never)]
fn alias(value: integer) -> integer {
    if value > 0 {
        return value; // alias true return
    }
    return value; // alias false return
}
#[inline(never)]
fn typed(value: integer) -> integer {
    let local: integer = value;
    return local + 1; // declared integer return
}
#[inline(never)]
fn increment(value: integer) -> integer {
    return value + 1; // shared alias increment return
}
#[inline(always)]
fn reuse(value: integer) -> integer {
    let local: integer = increment(value);
    let first: integer = if value > 0 { local } else { 0 };
    let second: integer = if value < 9 { local } else { 0 };
    return first + second;
}
fn choose_pair(value: integer) -> Pair {
    if value > 0 {
        let local: Pair = { .first = value, .second = value + 1 };
        return local; // pair true return
    }
    let local: Pair = { .first = value, .second = 100 };
    return local; // pair false return
}
fn choose_match(value: Logic) -> integer {
    let local: integer = 100;
    match value {
        '1' => {
            let local: integer = 7;
            return local; // match true return
        }
        _ => {
            return local; // match false return
        }
    }
}
#[test] entity T {}
impl T {
    stimulus: process {
        let input: integer = 5;
        let inner_input: integer = input - 3;
        let negative_input: integer = 0 - input;
        let selected: integer = choose(input); // branch true call
        let inner: integer = choose(inner_input); // branch inner call
        let fallback: integer = choose(negative_input); // branch false call
        let positive: integer = alias(input); // alias true call
        let negative: integer = alias(negative_input); // alias false call
        let first: Pair = choose_pair(input); // pair true call
        let second: Pair = choose_pair(0 - input); // pair false call
        let flag: Logic = '1';
        let matched: integer = choose_match(flag); // match true call
        let unmatched: integer = choose_match(not flag); // match false call
        let narrow: integer<-8..7> = -3;
        let narrow_result: integer = typed(narrow); // declared integer call
        let next_result: integer = typed(0 - narrow); // declared integer second call
        let both: integer = reuse(input);
        let second_only: integer = reuse(negative_input);
        let first_only: integer = reuse(20);
        let unselected: integer = if input < 0 { reuse(input) } else { 0 };
        assert!(selected == 35 and inner == 12 and fallback == 100, "branch results");
        assert!(positive == 5 and negative == -5, "same operand returns");
        assert!(first.first == 5 and first.second == 6 and second.first == -5 and second.second == 100, "aggregate selection results");
        assert!(matched == 7 and unmatched == 100, "match results");
        assert!(narrow_result == -2 and next_result == 4, "shared declared integer results");
        assert!(both == 12 and second_only == -4 and first_only == 21, "shared alias results");
        assert!(unselected == 0, "inactive source call must not run");
    }
}"#;
    for (increment, reuse) in [
        ("never", "always"),
        ("always", "always"),
        ("never", "never"),
        ("always", "never"),
    ] {
        let source = directory.join(format!("branches_{increment}_{reuse}.siox"));
        let text = text
            .replace(
                "#[inline(never)]\nfn increment",
                &format!("#[inline({increment})]\nfn increment"),
            )
            .replace(
                "#[inline(always)]\nfn reuse",
                &format!("#[inline({reuse})]\nfn reuse"),
            );
        std::fs::write(&source, &text).unwrap();
        let debug = directory.join("debug.bin");
        let ordinary = directory.join("ordinary.bin");
        for (binary, debugging) in [(&debug, true), (&ordinary, false)] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
            command.args(["--std", "std", "--test"]);
            if debugging {
                command.arg("--debug");
            }
            success(command.arg(&source).arg("-o").arg(binary).output().unwrap());
            success(
                Command::new(binary)
                    .arg("-o")
                    .arg(binary.with_extension("vcd"))
                    .output()
                    .unwrap(),
            );
        }
        assert_eq!(
            std::fs::read(debug.with_extension("vcd")).unwrap(),
            std::fs::read(ordinary.with_extension("vcd")).unwrap()
        );
        if Command::new("llvm-dwarfdump")
            .arg("--version")
            .output()
            .is_ok()
        {
            success(
                Command::new("llvm-dwarfdump")
                    .arg("--verify")
                    .arg(&debug)
                    .output()
                    .unwrap(),
            );
        }
        if Command::new("nm").arg("--version").output().is_ok() {
            let symbols = success(Command::new("nm").arg(&debug).output().unwrap());
            let symbols = String::from_utf8_lossy(&symbols.stdout);
            for function in ["choose", "alias"] {
                assert_eq!(
                    symbols
                        .lines()
                        .filter(|line| {
                            line.split_whitespace().last().is_some_and(|name| {
                                name.starts_with("sx.fn.")
                                    && name.ends_with(&format!(".{function}"))
                            })
                        })
                        .count(),
                    1,
                    "repeated calls must share one native source body: {symbols}"
                );
            }
        }
        if Command::new("gdb").arg("--version").output().is_ok() {
            let line = |marker| text.lines().position(|line| line.contains(marker)).unwrap() + 1;
            let python = format!(
                r#"import gdb
calls, failures = [], []
class CountCalls(gdb.Breakpoint):
    def stop(self):
        try:
            assert gdb.selected_frame().name() == 'debug_branches::increment'
            calls.append(int(gdb.parse_and_eval('value')))
        except Exception as error:
            failures.append(str(error))
        return False
counter = CountCalls({source:?} + ':{increment_return}')
stops = [
    ('choose', 5, 35, {nested_line}, {true_line}),
    ('choose', 2, 12, {inner_line}, {inner_call_line}),
    ('choose', -5, 100, {outer_line}, {false_line}),
    ('alias', 5, None, {alias_true_return}, {alias_true_call}),
    ('alias', -5, None, {alias_false_return}, {alias_false_call}),
    ('choose_pair', 5, (5, 6), {pair_true_return}, {pair_true_call}),
    ('choose_pair', -5, (-5, 100), {pair_false_return}, {pair_false_call}),
    ('choose_match', None, 7, {match_true_return}, {match_true_call}),
    ('choose_match', None, 100, {match_false_return}, {match_false_call}),
    ('typed', -3, -3, {typed_return}, {typed_call}),
    ('typed', 3, 3, {typed_return}, {typed_second_call}),
]
breakpoints = [gdb.Breakpoint({source:?} + ':' + str(stop[3])) for stop in stops]
gdb.execute('run')
for function, expected, local, return_line, call_line in stops:
    actual = gdb.selected_frame()
    assert actual.name() == 'debug_branches::' + function, actual.name()
    assert actual.find_sal().line == return_line, (actual.find_sal().line, return_line)
    if expected is not None:
        assert int(gdb.parse_and_eval('value')) == expected, str(gdb.parse_and_eval('value'))
    if isinstance(local, tuple):
        assert int(gdb.parse_and_eval('local.first')) == local[0], gdb.execute('info locals', to_string=True)
        assert int(gdb.parse_and_eval('local.second')) == local[1], gdb.execute('info locals', to_string=True)
    elif local is not None:
        assert int(gdb.parse_and_eval('local')) == local, gdb.execute('info locals', to_string=True)
    if function == 'typed':
        assert int(gdb.parse_and_eval('sizeof(value)')) == 8, gdb.execute('ptype value', to_string=True)
        assert int(gdb.parse_and_eval('sizeof(local)')) == 8, gdb.execute('ptype local', to_string=True)
    assert actual.older().name() == 'T::stimulus'
    assert actual.older().find_sal().line == call_line
    gdb.execute('continue')
assert int(gdb.parse_and_eval('$_exitcode')) == 0, 'unselected branch produced an extra stop'
assert not failures, failures
assert calls == [5, -5, 20], ('one source call per reused local', calls)
"#,
                source = source.to_string_lossy(),
                nested_line = line("branch nested return"),
                inner_line = line("branch inner return"),
                outer_line = line("branch outer return"),
                true_line = line("branch true call"),
                inner_call_line = line("branch inner call"),
                false_line = line("branch false call"),
                alias_true_return = line("alias true return"),
                alias_true_call = line("alias true call"),
                alias_false_return = line("alias false return"),
                alias_false_call = line("alias false call"),
                pair_true_return = line("pair true return"),
                pair_true_call = line("pair true call"),
                pair_false_return = line("pair false return"),
                pair_false_call = line("pair false call"),
                match_true_return = line("match true return"),
                match_true_call = line("match true call"),
                match_false_return = line("match false return"),
                match_false_call = line("match false call"),
                typed_return = line("declared integer return"),
                typed_call = line("declared integer call"),
                typed_second_call = line("declared integer second call"),
                increment_return = line("shared alias increment return"),
            );
            success(
                Command::new("gdb")
                    .args(["-nx", "-q", "--batch"])
                    .arg(&debug)
                    .args([
                        "-ex",
                        "set disable-randomization off",
                        "-ex",
                        "set breakpoint pending off",
                        "-ex",
                        &format!("python {python}"),
                    ])
                    .output()
                    .unwrap(),
            );
        } else {
            eprintln!("GDB unavailable: branch native execution checked, debugger probe skipped");
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn hardware_expression_calls_keep_source_frames() {
    let directory =
        std::env::temp_dir().join(format!("siox_dwarf_hardware_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("hardware.siox");
    let text = r#"module debug_hardware;
use std::bits::{unsigned, signed};
fn bump(value: integer) -> integer {
    let local: integer = value + 1;
    return local + 1; // hardware leaf return
}
fn outer(value: integer) -> integer {
    let local: integer = 100;
    let computed: integer = bump(value); // hardware nested call
    return computed + local; // hardware outer return
}
fn choose(value: integer) -> integer {
    let local: integer = value + 1;
    if value > 0 {
        let local: integer = value + 2;
        return local; // hardware selected return
    }
    return local + 20; // hardware fallback return
}
fn inspect(value: unsigned[81], gain: real, negative: signed[81]) -> integer {
    let wide: unsigned[81] = value;
    let floating: real = gain;
    let signed_value: signed[81] = negative;
    return integer(floating) + if wide == 18446744073709551617 and signed_value == -2 { 0 } else { 100 }; // hardware typed return
}
fn sample(value: integer) -> integer {
    let local: integer = value + 10;
    return local; // hardware event return
}
fn twice(value: integer) -> integer { return value + value; }
entity Narrow { value: integer<-8..7> in, result: integer out }
impl Narrow { result = twice(twice(twice(value))); }
entity Dut {
    value: integer in,
    result: integer out,
    selected: integer out
}
impl Dut {
    result = outer(value); // hardware source assignment
    selected = choose(value); // hardware selected assignment
}
entity Typed {
    value: unsigned[81] in,
    gain: real in,
    negative: signed[81] in,
    result: integer out
}
impl Typed {
    result = inspect(value, gain, negative); // hardware typed assignment
}
entity Register { clk: Bit in, value: integer in, result: integer out }
impl Register {
    if clk.rising() {
        result = sample(value); // hardware event assignment
    }
}
#[test] entity T {}
impl T {
    let input: integer = 5;
    let dut: Dut = { .value = input };
    let other: Dut = { .value = 10 };
    let typed: Typed = { .value = 18446744073709551617, .gain = 1.5, .negative = -2 };
    let small: integer<-8..7> = -3;
    let narrow: Narrow = { .value = small };
    let clock: Bit = '0';
    let reg: Register = { .clk = clock, .value = input };
    stimulus: process {
        await 1ns;
        assert!(dut.result == 107 and dut.selected == 7, "first hardware result");
        assert!(typed.result == 1 and other.result == 112 and other.selected == 12, "typed and shared helper results");
        assert!(narrow.result == -24, "nested arguments retain consumer widths and signs");
        input = 6;
        small = 3;
        clock = '1';
        await 1ns;
        assert!(dut.result == 108 and dut.selected == 8, "updated hardware result");
        assert!(narrow.result == 24, "nested formats reevaluate after input changes");
        assert!(reg.result == 16, "event call result");
        input = -6;
        clock = '0';
        await 1ns;
        assert!(dut.result == 96 and dut.selected == 15, "fallback hardware result");
        clock = '1';
        await 1ns;
        assert!(reg.result == 4, "updated event call result");
    }
}"#;
    std::fs::write(&source, text).unwrap();
    let debug = directory.join("debug.bin");
    let ordinary = directory.join("ordinary.bin");
    for (binary, debugging) in [(&debug, true), (&ordinary, false)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
        command.args(["--std", "std", "--test"]);
        if debugging {
            command.arg("--debug");
        }
        success(command.arg(&source).arg("-o").arg(binary).output().unwrap());
        success(
            Command::new(binary)
                .arg("-o")
                .arg(binary.with_extension("vcd"))
                .output()
                .unwrap(),
        );
    }
    assert_eq!(
        std::fs::read(debug.with_extension("vcd")).unwrap(),
        std::fs::read(ordinary.with_extension("vcd")).unwrap()
    );
    if Command::new("llvm-dwarfdump")
        .arg("--version")
        .output()
        .is_ok()
    {
        success(
            Command::new("llvm-dwarfdump")
                .arg("--verify")
                .arg(&debug)
                .output()
                .unwrap(),
        );
    }
    if Command::new("gdb").arg("--version").output().is_ok() {
        let line = |marker| text.lines().position(|line| line.contains(marker)).unwrap() + 1;
        let python = format!(
            r#"import gdb
seen = set()
owners = set()
events = set()
typed_seen = False
failures = []
class Check(gdb.Breakpoint):
    def stop(self):
        try:
            self.check()
        except Exception as error:
            failures.append(str(error))
        return False
class Selection(Check):
    def check(self):
        frame = gdb.selected_frame()
        assert frame.name() == 'debug_hardware::choose', frame.name()
        value = int(gdb.parse_and_eval('value'))
        expected_line = {selected_return} if value > 0 else {fallback_return}
        assert frame.find_sal().line == expected_line, (value, frame.find_sal().line, expected_line)
        local = int(gdb.parse_and_eval('local'))
        assert local == value + (2 if value > 0 else 1), (value, local, gdb.execute('ptype local', to_string=True))
        assert frame.older().find_sal().line == {selected_assignment}
        owner = frame.older().name().removesuffix('(void)')
        assert owner in ['T::dut::<hardware:T.dut.selected>', 'T::other::<hardware:T.other.selected>'], owner
        if value == 10:
            assert owner == 'T::other::<hardware:T.other.selected>', owner
        elif value != 0:
            assert owner == 'T::dut::<hardware:T.dut.selected>', owner
        owners.add(owner)
        seen.add(value)
        return False
Selection({source:?} + ':{selected_return}')
Selection({source:?} + ':{fallback_return}')
class Typed(Check):
    def check(self):
        global typed_seen
        frame = gdb.selected_frame()
        assert frame.name() == 'debug_hardware::inspect', frame.name()
        assert str(frame.function().type.target()) == 'integer', frame.function().type
        assert frame.older().find_sal().line == {typed_assignment}
        assert frame.older().name().removesuffix('(void)') == 'T::typed::<hardware:T.typed.result>', frame.older().name()
        value = int(gdb.parse_and_eval('value'))
        gain = float(gdb.parse_and_eval('gain'))
        negative = int(gdb.parse_and_eval('negative'))
        assert value in [0, (1 << 64) + 1], value
        assert gain in [0, 1.5], gain
        assert negative in [0, -2], negative
        assert int(gdb.parse_and_eval('wide')) == value
        assert float(gdb.parse_and_eval('floating')) == gain
        assert int(gdb.parse_and_eval('signed_value')) == negative
        typed_seen |= value == (1 << 64) + 1 and gain == 1.5 and negative == -2
        return False
class Event(Check):
    def check(self):
        frame = gdb.selected_frame()
        assert frame.name() == 'debug_hardware::sample', frame.name()
        assert frame.older().find_sal().line == {event_assignment}
        assert frame.older().name().removesuffix('(void)') == 'T::reg::<hardware:T.reg.result>', frame.older().name()
        value = int(gdb.parse_and_eval('value'))
        assert int(gdb.parse_and_eval('local')) == value + 10
        events.add(value)
        return False
Typed({source:?} + ':{typed_return}')
Event({source:?} + ':{event_return}')
leaf = gdb.Breakpoint({source:?} + ':{leaf_return}')
outer = gdb.Breakpoint({source:?} + ':{outer_return}')
gdb.execute('run')
frame = gdb.selected_frame()
assert frame.name() == 'debug_hardware::bump', frame.name()
assert frame.find_sal().line == {leaf_return}
assert int(gdb.parse_and_eval('value')) == 5
assert int(gdb.parse_and_eval('local')) == 6
assert frame.older().name() == 'debug_hardware::outer', frame.older().name()
assert frame.older().find_sal().line == {nested_call}
leaf.delete()
gdb.execute('continue')
frame = gdb.selected_frame()
assert frame.name() == 'debug_hardware::outer', frame.name()
assert frame.find_sal().line == {outer_return}
assert int(gdb.parse_and_eval('computed')) == 7
assert int(gdb.parse_and_eval('local')) == 100
assert frame.older().find_sal().line == {assignment}, frame.older().find_sal().line
outer.delete()
gdb.execute('continue')
assert int(gdb.parse_and_eval('$_exitcode')) == 0
assert not failures, failures
assert owners == {{'T::dut::<hardware:T.dut.selected>', 'T::other::<hardware:T.other.selected>'}}, owners
assert {{5, 6, -6, 10}} <= seen <= {{0, 5, 6, -6, 10}}, seen
assert events == {{6, -6}}, events
assert typed_seen
"#,
            source = source.to_string_lossy(),
            leaf_return = line("hardware leaf return"),
            outer_return = line("hardware outer return"),
            nested_call = line("hardware nested call"),
            assignment = line("hardware source assignment"),
            selected_return = line("hardware selected return"),
            fallback_return = line("hardware fallback return"),
            selected_assignment = line("hardware selected assignment"),
            typed_return = line("hardware typed return"),
            typed_assignment = line("hardware typed assignment"),
            event_return = line("hardware event return"),
            event_assignment = line("hardware event assignment"),
        );
        success(
            Command::new("gdb")
                .args(["-nx", "-q", "--batch"])
                .arg(&debug)
                .args([
                    "-ex",
                    "set disable-randomization off",
                    "-ex",
                    "set breakpoint pending off",
                    "-ex",
                    &format!("python {python}"),
                ])
                .output()
                .unwrap(),
        );
    } else {
        eprintln!("GDB unavailable: hardware native execution checked, debugger probe skipped");
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn repeated_inline_calls_keep_locals_and_source_frames_across_await() {
    let directory = std::env::temp_dir().join(format!("siox_dwarf_calls_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("calls.siox");
    let text = r#"module debug_calls;
use std::bits::unsigned;
use std::primitive::string;
struct Payload { pub tag: integer, pub gain: real, pub wide: unsigned[81] }
impl Payload {
    pub fn inspect(self) {
        self.tag = self.tag + 1;
        await 1ns;
        assert!(self.tag == 10, "receiver parameter stop");
    }
}
fn leaf(value: integer) {
    let local: integer = value;
    if value > 0 {
        let local: integer = 42;
        await 1ns;
        assert!(local == 42, "leaf inner stop");
    }
    await 1ns;
    assert!(local == value, "leaf stop");
}
fn outer(value: integer) {
    let local: integer = 100;
    leaf(value); // nested call
    assert!(local == 100, "outer stop");
}
fn choose(value: integer) -> integer {
    let local: integer = value;
    await 1ns;
    if value > 0 { return local; } // choose stop
    return 0;
}
fn mutate(value: integer) {
    value = value + 1;
    await 1ns;
    assert!(value == 11, "alias parameter stop");
}
fn check_payload(value: Payload) {
    await 1ns;
    assert!(value.tag == 9 and value.gain == 1.5, "payload parameter stop");
}
fn check_real(value: real) {
    await 1ns;
    assert!(value == 1.5, "real parameter stop");
}
fn check_wide(value: unsigned[81]) {
    await 1ns;
    assert!(value == 18446744073709551617, "wide parameter stop");
}
fn check_string(value: string) {
    await 1ns;
    assert!(value'length == 3, "string parameter stop");
}
fn check_empty(value: string) {
    await 1ns;
    assert!(value'length == 0, "empty parameter stop");
}
#[test] entity T {}
impl T {
    stimulus: process {
        let local: integer = 10;
        let cells: integer[2] = [0, 10];
        let index: integer = 1;
        let packet: Payload = { .tag = 9, .gain = 1.5, .wide = 18446744073709551617 };
        let text: string = read<string>("argument.txt");
        leaf(1); // first call
        leaf(2); // second call
        leaf(6); leaf(7); // paired calls
        outer(3); // outer call
        let chosen: integer = choose(4); // first value call
        let chosen_again: integer = choose(5); // second value call
        mutate(local);
        mutate(cells[index]);
        check_payload(packet);
        check_real(packet.gain);
        check_wide(packet.wide);
        check_string(text);
        check_string("β\0🙂");
        check_empty("");
        packet.inspect();
        assert!(local == 11 and cells[1] == 11 and chosen == 4 and chosen_again == 5, "caller stop");
    }
}"#;
    std::fs::write(&source, text).unwrap();
    std::fs::write(directory.join("argument.txt"), "β\0🙂").unwrap();
    let debug = directory.join("debug.bin");
    let normal = directory.join("normal.bin");
    for (binary, debugging) in [(&debug, true), (&normal, false)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
        command.args(["--std", "std", "--test"]);
        if debugging {
            command.arg("--debug");
        }
        success(command.arg(&source).arg("-o").arg(binary).output().unwrap());
        success(
            Command::new(binary)
                .arg("-o")
                .arg(binary.with_extension("vcd"))
                .output()
                .unwrap(),
        );
    }
    assert_eq!(
        std::fs::read(debug.with_extension("vcd")).unwrap(),
        std::fs::read(normal.with_extension("vcd")).unwrap()
    );
    if Command::new("llvm-dwarfdump")
        .arg("--version")
        .output()
        .is_ok()
    {
        success(
            Command::new("llvm-dwarfdump")
                .arg("--verify")
                .arg(&debug)
                .output()
                .unwrap(),
        );
    }
    if Command::new("gdb").arg("--version").output().is_ok() {
        let line = |marker| text.lines().position(|line| line.contains(marker)).unwrap() + 1;
        let python = format!(
            r#"import gdb
inner = gdb.Breakpoint({source:?} + ':{inner_line}')
leaf = gdb.Breakpoint({source:?} + ':{leaf_line}')
outer = gdb.Breakpoint({source:?} + ':{outer_line}')
choose = gdb.Breakpoint({source:?} + ':{choose_line}')
alias = gdb.Breakpoint({source:?} + ':{alias_line}')
payload = gdb.Breakpoint({source:?} + ':{payload_line}')
real = gdb.Breakpoint({source:?} + ':{real_line}')
wide = gdb.Breakpoint({source:?} + ':{wide_line}')
string = gdb.Breakpoint({source:?} + ':{string_line}')
empty = gdb.Breakpoint({source:?} + ':{empty_line}')
receiver = gdb.Breakpoint({source:?} + ':{receiver_line}')
caller = gdb.Breakpoint({source:?} + ':{caller_line}')
gdb.execute('run')
def frame(name, local):
    actual = gdb.selected_frame()
    assert actual.name() == name, (actual.name(), name)
    assert int(gdb.parse_and_eval('local')) == local, (name, str(gdb.parse_and_eval('local')), local)
    return actual
for expected, call_line in [(1, {first_line}), (2, {second_line}), (6, {paired_line}), (7, {paired_line}), (3, {nested_line})]:
    frame('debug_calls::leaf', 42)
    assert int(gdb.parse_and_eval('value')) == expected
    assert 'value =' in gdb.execute('info args', to_string=True)
    gdb.execute('continue')
    actual = frame('debug_calls::leaf', expected)
    assert int(gdb.parse_and_eval('value')) == expected
    parent = actual.older()
    assert parent.find_sal().line == call_line, (parent.find_sal().line, call_line)
    parent.select()
    if expected == 3:
        frame('debug_calls::outer', 100)
        assert int(gdb.parse_and_eval('value')) == 3
        assert parent.older().name() == 'T::stimulus'
        assert parent.older().find_sal().line == {outer_call_line}
    else:
        frame('T::stimulus', 10)
    actual.select()
    gdb.execute('continue')
frame('debug_calls::outer', 100)
assert int(gdb.parse_and_eval('value')) == 3
outer.delete()
inner.delete()
leaf.delete()
gdb.execute('continue')
for expected, call_line in [(4, {first_value_line}), (5, {second_value_line})]:
    actual = frame('debug_calls::choose', expected)
    assert int(gdb.parse_and_eval('value')) == expected
    assert actual.older().name() == 'T::stimulus'
    assert actual.older().find_sal().line == call_line
    gdb.execute('continue')
for _ in range(2):
    assert gdb.selected_frame().name() == 'debug_calls::mutate'
    assert int(gdb.parse_and_eval('value')) == 11
    assert 'value = 11' in gdb.execute('info args', to_string=True)
    gdb.execute('continue')
alias.delete()
assert gdb.selected_frame().name() == 'debug_calls::check_payload'
view = gdb.parse_and_eval('value')
assert int(view['tag']) == 9 and float(view['gain']) == 1.5
assert int(view['wide']) == (1 << 64) + 1
payload.delete()
gdb.execute('continue')
assert gdb.selected_frame().name() == 'debug_calls::check_real'
assert float(gdb.parse_and_eval('value')) == 1.5
real.delete()
gdb.execute('continue')
assert gdb.selected_frame().name() == 'debug_calls::check_wide'
assert int(gdb.parse_and_eval('value')) == (1 << 64) + 1
wide.delete()
gdb.execute('continue')
for _ in range(2):
    assert gdb.selected_frame().name() == 'debug_calls::check_string'
    view = gdb.parse_and_eval('value')
    assert int(view['length']) == 3 and int(view['byte_length']) == 7
    assert [int(view['data'][i]) for i in range(3)] == [ord(c) for c in 'β\x00🙂']
    assert bytes(int(view['utf8'][i]) for i in range(7)) == 'β\x00🙂'.encode()
    gdb.execute('continue')
string.delete()
assert gdb.selected_frame().name() == 'debug_calls::check_empty'
view = gdb.parse_and_eval('value')
assert int(view['length']) == int(view['byte_length']) == 0
assert int(view['data']) == int(view['utf8']) == 0
assert 'value =' in gdb.execute('info args', to_string=True)
empty.delete()
gdb.execute('continue')
assert gdb.selected_frame().name().endswith('::inspect')
view = gdb.parse_and_eval('self')
assert int(view['tag']) == 10 and float(view['gain']) == 1.5
assert int(view['wide']) == (1 << 64) + 1
assert 'self =' in gdb.execute('info args', to_string=True)
receiver.delete()
gdb.execute('continue')
frame('T::stimulus', 11)
assert int(gdb.parse_and_eval('chosen')) == 4
assert int(gdb.parse_and_eval('chosen_again')) == 5
caller.delete()
choose.delete()
gdb.execute('continue')
assert int(gdb.parse_and_eval('$_exitcode')) == 0
"#,
            source = source.to_string_lossy(),
            leaf_line = line("leaf stop"),
            inner_line = line("leaf inner stop"),
            outer_line = line("outer stop"),
            choose_line = line("choose stop"),
            alias_line = line("alias parameter stop"),
            payload_line = line("payload parameter stop"),
            real_line = line("real parameter stop"),
            wide_line = line("wide parameter stop"),
            string_line = line("string parameter stop"),
            empty_line = line("empty parameter stop"),
            receiver_line = line("receiver parameter stop"),
            caller_line = line("caller stop"),
            first_line = line("first call"),
            second_line = line("second call"),
            paired_line = line("paired calls"),
            nested_line = line("nested call"),
            outer_call_line = line("outer call"),
            first_value_line = line("first value call"),
            second_value_line = line("second value call")
        );
        success(
            Command::new("gdb")
                .args(["-nx", "-q", "--batch"])
                .arg(&debug)
                .args([
                    "-ex",
                    "set disable-randomization off",
                    "-ex",
                    "set breakpoint pending off",
                    "-ex",
                    &format!("python {python}"),
                ])
                .output()
                .unwrap(),
        );
    } else {
        eprintln!("GDB unavailable: inline native/metadata checked, debugger probe skipped");
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn direct_dwarf_supports_source_steps_and_persistent_values() {
    let directory = std::env::temp_dir().join(format!("siox_dwarf_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("debug.siox");
    let mut text = r#"module debug;
use std::bits::{unsigned, signed};
use std::primitive::string;
enum Mode { Idle = 3, Active = 7 }
struct Packet { pub tag: unsigned[3], pub data: unsigned[128], pub gain: real }
struct LogicPacket { pub tag: unsigned[3], pub gain: real, pub data: unsigned[4] }
entity LogicProbe { states: unsigned[9] in, ascending: Logic[-2..0] in, echo: unsigned[9] out }
impl LogicProbe { echo = states; }
entity Probe { a: unsigned[3] in, b: unsigned[5] in, y: unsigned[8] out, negative: signed[81] in }
impl Probe { y = unsigned[8](a) + unsigned[8](b); }
#[test] entity T {}
impl T {
    let text: string = read<string>("message.txt");
    let alias: string = text;
    let empty: string = read<string>("empty.txt");
    let a: unsigned[3] = 5;
    let b: unsigned[5] = 17;
    let y: unsigned[8];
    let wide_negative: signed[81] = -2;
    let probe: Probe = { .a = a, .b = b, .y = y, .negative = wide_negative };
    let count: integer = 7;
    let negative: signed[3] = -2;
    let mode: Mode = Mode::Active;
    let packet: Packet = { .tag = 5, .data = 18446744073709551616, .gain = 1.5 };
    let words: unsigned[128][2] = [18446744073709551616, 9];
    let labels: unsigned[3][3..2] = [5, 6];
    let bits: unsigned[3][-1..0] = [2, 7];
    let states: unsigned[9] = "01XZUWLH-";
    let ascending: Logic[-2..0] = ['0', 'X', '1'];
    let logic_packet: LogicPacket = { .tag = 5, .gain = 2.5, .data = "1XZ0" };
    let logic_matrix: unsigned[4][3..2] = ["1XZ0", "0ZX1"];
    let logic_probe: LogicProbe = { .states = states, .ascending = ascending };
    stimulus: process {
        let local_text: string = read<string>("short.txt");
        let local_states: unsigned[9] = states;
        let local_packet: LogicPacket = logic_packet;
        let local: integer = 3;
        local = local + 1;
        count = count + local;
        await 1ns;
        assert!(count == 11 and local == 4, "persistent locals");
        local = local + 1;
        assert!(local == 5 and y == 22, "source stepping");
        if local == 5 {
            let local: integer = 99;
            await 1ns;
            assert!(local == 99, "shadow inner");
        }
        assert!(local == 5, "shadow restored");
        states = "ZX10UWLH-";
        local_states = states;
        await 1ns;
        assert!(states[8] == 'Z' and states[7] == 'X', "logic updates");
        text = read<string>("replacement.txt");
        local_text = text;
        await 1ns;
        assert!(text'length == 2 and local_text'length == 2 and local_text[0] == 'β' and local_text == "β🙂", "string updates");
        for i in 0..1 {
            await 1ns;
            assert!(i >= 0, "loop index");
        }
    }
}
#[test] entity Reset {}
impl Reset {
    let text: string = read<string>("short.txt"); // reset init
    process {
        await 1ns;
        assert!(text'length == 1, "reset strings");
    }
}"#
    .to_owned();
    let widths = [65, 81, 127, 129];
    let mut ports = String::new();
    let mut declarations = String::new();
    let mut bindings = String::new();
    let mut locals = String::new();
    for width in widths {
        ports.push_str(&format!(
            "u{width}: unsigned[{width}] in, s{width}: signed[{width}] in,"
        ));
        declarations.push_str(&format!(
            "    let u{width}: unsigned[{width}] = (unsigned[{width}](1) << {}) + 3;\n    let s{width}: signed[{width}] = -2;\n", width - 1,
        ));
        bindings.push_str(&format!(".u{width} = u{width}, .s{width} = s{width},"));
        locals.push_str(&format!(
            "        let local_u{width}: unsigned[{width}] = u{width};\n        let local_s{width}: signed[{width}] = s{width};\n",
        ));
    }
    text = text.replace(
        "#[test] entity T",
        &format!("entity WideProbe {{ {ports} }}\nimpl WideProbe {{}}\n#[test] entity T"),
    );
    text = text.replace("    stimulus: process {", &format!("{declarations}    let wide_probe: WideProbe = {{ {bindings} }};\n    stimulus: process {{\n{locals}"));
    // Ordinary objects/LLVM text describe one structural root; the native
    // test artifact below includes both roots to exercise runtime cleanup.
    std::fs::write(&source, text.split("#[test] entity Reset").next().unwrap()).unwrap();
    let compile = |extra: &[&str], path: &std::path::Path| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
        command
            .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
            .args(extra)
            .arg(&source);
        if !path.as_os_str().is_empty() {
            command.arg("-o").arg(path);
        }
        success(command.output().unwrap())
    };
    let ir = compile(&["--emit", "llvm-ir", "--debug"], std::path::Path::new(""));
    let ir = String::from_utf8(ir.stdout).unwrap();
    for marker in [
        "!DICompileUnit",
        "!DISubprogram",
        "!DILocation",
        "!DIGlobalVariableExpression",
    ] {
        assert!(ir.contains(marker), "missing {marker}");
    }
    let object = directory.join("debug.o");
    compile(&["--emit", "object", "--debug"], &object);
    std::fs::write(&source, &text).unwrap();
    let debug = directory.join("debug.bin");
    compile(&["--test", "--debug"], &debug);
    let normal = directory.join("normal.bin");
    compile(&["--test"], &normal);
    // Dynamic IO must remain runtime-owned, even with source debugging.
    for (name, contents) in [
        ("message.txt", "Aé\0🦀"),
        ("empty.txt", ""),
        ("short.txt", "Z"),
        ("replacement.txt", "β🙂"),
    ] {
        std::fs::write(directory.join(name), contents).unwrap();
    }
    let baseline = directory.join("normal.vcd");
    let debug_wave = directory.join("debug.vcd");
    success(
        Command::new(&normal)
            .arg("-o")
            .arg(&baseline)
            .output()
            .unwrap(),
    );
    success(
        Command::new(&debug)
            .arg("-o")
            .arg(&debug_wave)
            .output()
            .unwrap(),
    );
    assert_eq!(
        std::fs::read(&baseline).unwrap(),
        std::fs::read(&debug_wave).unwrap()
    );
    if Command::new("llvm-dwarfdump")
        .arg("--version")
        .output()
        .is_ok()
    {
        for artifact in [&object, &debug] {
            success(
                Command::new("llvm-dwarfdump")
                    .arg("--verify")
                    .arg(artifact)
                    .output()
                    .unwrap(),
            );
        }
    }
    if Command::new("gdb").arg("--version").output().is_ok() {
        let line = text
            .lines()
            .position(|line| line.contains("persistent locals"))
            .unwrap()
            + 1;
        let python = format!(
            r#"import gdb
bp = gdb.Breakpoint({source:?} + ':{line}')
uninitialized = gdb.Breakpoint({source:?} + ':{initial_string_line}')
entry = gdb.Breakpoint('T::stimulus')
gdb.execute('run')
assert gdb.selected_frame().find_sal().line == {initial_string_line}
initial = gdb.parse_and_eval('T::text')
assert int(initial['length']) == int(initial['byte_length']) == 0
assert int(initial['data']) == int(initial['utf8']) == 0
uninitialized.delete()
gdb.execute('continue')
assert gdb.selected_frame().name() == 'T::stimulus'
entry.delete()
gdb.execute('continue')
frame = gdb.selected_frame()
assert frame.find_sal().line == {line}
assert frame.name() == 'T::stimulus', frame.name()
def value(expr): return gdb.parse_and_eval(expr)
def string(expr, expected):
    view = value(expr)
    assert int(view['length']) == len(expected), (expr, str(view))
    assert [int(view['data'][i]) for i in range(len(expected))] == [ord(c) for c in expected]
    raw = bytes(int(view['utf8'][i]) for i in range(int(view['byte_length'])))
    assert raw == expected.encode('utf-8'), (expr, raw)
for expr in ['T::text', 'T::alias']: string(expr, 'Aé\x00🦀')
string('T::empty', '')
string('local_text', 'Z')
def symbols(v, labels):
    return ''.join(str(v[i] if v.type.code == gdb.TYPE_CODE_ARRAY else v['[' + str(i) + ']']).strip("'") for i in labels)
for expr in ['T::states$logic', 'T::logic_probe::states$logic', 'T::logic_probe::echo$logic', 'local_states$logic']:
    assert symbols(value(expr), range(8, -1, -1)) == '01XZUWLH-', (expr, str(value(expr)))
assert symbols(value('T::ascending'), [-2, -1, 0]) == '0X1'
assert ''.join(str(value("'T::logic_probe::ascending[" + str(i) + "]'")).strip("'") for i in [-2, -1, 0]) == '0X1'
for expr in ['T::logic_packet$logic', 'local_packet$logic']:
    packet = value(expr)
    assert int(packet['tag'][2]) == 1
    assert float(packet['gain']) == 2.5
    assert symbols(packet['data'], range(3, -1, -1)) == '1XZ0'
matrix = value('T::logic_matrix$logic')
assert symbols(matrix['[3]'], range(3, -1, -1)) == '1XZ0'
assert symbols(matrix['[2]'], range(3, -1, -1)) == '0ZX1'
for width in [65, 81, 127, 129]:
    for owner in ['T::', 'T::wide_probe::', 'local_']:
        assert int(value(owner + 'u' + str(width))) == (1 << (width - 1)) + 3
        assert int(value(owner + 's' + str(width))) == -2
for expr, expected in [('local', 4), ('T::count', 11), ('T::negative', -2), ('T::mode', 7), ('T::a', 5), ('T::b', 17), ('T::probe::a', 5), ('T::probe::b', 17), ('T::probe::y', 22), ('T::packet.tag', 5), ('T::packet.data', 1 << 64), ('T::words[0]', 1 << 64), ('T::words[1]', 9)]:
    actual = int(value(expr))
    assert actual == expected, (expr, actual, expected)
assert float(value('T::packet.gain')) == 1.5
assert int(value('T::wide_negative')) == -2
assert int(value('T::probe::negative')) == -2
assert 'Active' in str(value('T::mode'))
assert int(value('T::labels')['[3]']) == 5
assert int(value('T::labels')['[2]']) == 6
assert int(value('T::bits[-1]')) == 2
assert int(value('T::bits[0]')) == 7
gdb.execute('next')
assert gdb.selected_frame().find_sal().line == {next_line}
assert int(value('local')) == 4
gdb.execute('next')
assert int(value('local')) == 5
bp.delete()
inner = gdb.Breakpoint({source:?} + ':{inner_line}')
outer = gdb.Breakpoint({source:?} + ':{outer_line}')
gdb.execute('continue')
assert gdb.selected_frame().find_sal().line == {inner_line}
assert int(value('local')) == 99
inner.delete()
gdb.execute('continue')
assert gdb.selected_frame().find_sal().line == {outer_line}
assert int(value('local')) == 5
outer.delete()
updated = gdb.Breakpoint({source:?} + ':{update_line}')
gdb.execute('continue')
for expr in ['T::states$logic', 'T::logic_probe::states$logic', 'T::logic_probe::echo$logic', 'local_states$logic']:
    assert symbols(value(expr), range(8, -1, -1)) == 'ZX10UWLH-'
updated.delete()
strings = gdb.Breakpoint({source:?} + ':{string_line}')
gdb.execute('continue')
string('T::text', 'β🙂')
string('local_text', 'β🙂')
string('T::alias', 'Aé\x00🦀')
strings.delete()
loop = gdb.Breakpoint({source:?} + ':{loop_line}')
for expected in [0, 1]:
    gdb.execute('continue')
    assert gdb.selected_frame().find_sal().line == {loop_line}
    assert int(value('i')) == expected
loop.delete()
reset_init = gdb.Breakpoint({source:?} + ':{reset_init_line}')
gdb.execute('continue')
for expr in ['T::text', 'T::alias', 'Reset::text']:
    view = value(expr)
    assert int(view['length']) == int(view['byte_length']) == 0
    assert int(view['data']) == int(view['utf8']) == 0
reset_init.delete()
reset = gdb.Breakpoint({source:?} + ':{reset_line}')
gdb.execute('continue')
string('Reset::text', 'Z')
assert int(value('T::text')['data']) == 0
assert int(value('T::alias')['utf8']) == 0
reset.delete()
gdb.execute('continue')
assert int(value('$_exitcode')) == 0
"#,
            source = source.to_string_lossy(),
            next_line = line + 1,
            inner_line = text
                .lines()
                .position(|line| line.contains("shadow inner"))
                .unwrap()
                + 1,
            outer_line = text
                .lines()
                .position(|line| line.contains("shadow restored"))
                .unwrap()
                + 1,
            loop_line = text
                .lines()
                .position(|line| line.contains("loop index"))
                .unwrap()
                + 1,
            update_line = text
                .lines()
                .position(|line| line.contains("logic updates"))
                .unwrap()
                + 1,
            initial_string_line = text
                .lines()
                .position(|line| line.contains("let text: string"))
                .unwrap()
                + 1,
            string_line = text
                .lines()
                .position(|line| line.contains("string updates"))
                .unwrap()
                + 1,
            reset_init_line = text
                .lines()
                .position(|line| line.contains("reset init"))
                .unwrap()
                + 1,
            reset_line = text
                .lines()
                .position(|line| line.contains("reset strings"))
                .unwrap()
                + 1,
        );
        success(
            Command::new("gdb")
                .args(["-nx", "-q", "--batch"])
                .arg(&debug)
                .args([
                    "-ex",
                    "set disable-randomization off",
                    "-ex",
                    "set breakpoint pending off",
                    "-ex",
                    &format!("python {python}"),
                ])
                .output()
                .unwrap(),
        );
    } else {
        eprintln!("GDB unavailable: source execution/metadata checked, debugger probe skipped");
    }
    std::fs::remove_dir_all(directory).unwrap();
}
