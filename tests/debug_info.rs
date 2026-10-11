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
fn reuse_text(value: string, mask: integer) -> integer {
    if mask == 0 { return 0; }
    let saved: string = echo_text(value); // reused string assignment
    let first: integer = if mask > 0 { count(saved) } else { 0 }; // first reused string consumer
    let second: integer = if mask < 9 { count(saved) } else { 0 }; // second reused string consumer
    return first + second;
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
        let folded_literal: integer = count(echo_text("μ\0🙂")); // folded literal string call
        let folded_empty: integer = count(echo_text("")); // folded empty string call
        let folded_nested: integer = count(echo_text(echo_text("μ\0🙂"))); // nested folded string call
        let selected_literal: integer = if start > 0 { count(echo_text("μ\0🙂")) } else { count(echo_text("untaken")) }; // selected folded string call
        let selected_empty: integer = if start < 0 { count(echo_text("")) } else { 17 };
        let inactive_length: integer = if start < 0 { echo_text("")'length } else { 19 }; // inactive folded length
        let reused_literal: integer = reuse_text("μ\0🙂", 5); // reused literal string call
        let reused_empty: integer = reuse_text("", 5); // reused empty string call
        let first_only: integer = reuse_text("", 10); // first-only empty string call
        let second_only: integer = reuse_text("", -5); // second-only empty string call
        let neither: integer = reuse_text("", 0);
        print!("{}", echo_text("μ\0🙂")); // formatted literal string call
        print!("{}", echo_text("")); // formatted empty string call
        warn!(false, "{}", echo_text("μ\0🙂")); // formatted warning string call
        warn!(true, "{}", echo_text(""));
        assert!(true, "{}", echo_text(""));
        let packet: Payload = { .tag = 9, .gain = 1.5, .wide = 18446744073709551617 };
        let payload_result: integer = inspect_payload(packet); // expression payload call
        assert!(runtime_length == 3, "runtime expression string length");
        assert!(borrowed == text, "borrowed expression string result");
        assert!(borrowed[0] == 'β' and integer(borrowed[1]) == 0, "borrowed expression string indexing");
        assert!(borrowed == "β\0🙂", "borrowed expression string content");
        assert!(literal_length == 3, "literal expression string length");
        assert!(empty_length == 0, "empty expression string length");
        assert!(folded_literal == 3 and folded_empty == 0, "folded string return lengths");
        assert!(folded_nested == 3 and selected_literal == 3 and selected_empty == 17 and inactive_length == 19, "nested and selected folded string calls");
        assert!(reused_literal == 6 and reused_empty == 0, "reused string return lengths");
        assert!(first_only == 0 and second_only == 0 and neither == 0, "selected reused empty strings");
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
folded_stop = gdb.Breakpoint({source:?} + ':{borrowed_return_line}')
gdb.execute('continue')
for first, caller_line, nested in [(0x3b2, {runtime_string_line}, 0), (0x3bc, {literal_string_line}, 0), (None, {empty_string_line}, 0), (0x3bc, {folded_literal_line}, 1), (None, {folded_empty_line}, 1), (0x3bc, {folded_nested_line}, 2), (0x3bc, {selected_literal_line}, 1)]:
    for _ in range(nested):
        actual = gdb.selected_frame()
        assert actual.name() == 'debug_values::echo_text', ('folded string frame', actual.name())
        assert actual.older().name() == 'T::stimulus', actual.older().name()
        assert actual.older().find_sal().line == caller_line
        assert actual.find_sal().line == {borrowed_return_line}
        check_string(first)
        gdb.execute('continue')
    actual = gdb.selected_frame()
    assert actual.name() == 'debug_values::count', actual.name()
    assert actual.older().find_sal().line == caller_line
    assert 'value =' in gdb.execute('info args', to_string=True)
    check_string(first)
    gdb.execute('continue')
for first, caller_line, consumers in [(0x3bc, {reused_literal_line}, [{first_consumer_line}, {second_consumer_line}]), (None, {reused_empty_line}, [{first_consumer_line}, {second_consumer_line}]), (None, {first_only_line}, [{first_consumer_line}]), (None, {second_only_line}, [{second_consumer_line}])]:
    actual = gdb.selected_frame()
    assert actual.name() == 'debug_values::echo_text', ('shared literal frame', actual.name())
    assert actual.older().name() == 'debug_values::reuse_text'
    assert actual.older().find_sal().line == {reused_assignment_line}, actual.older().find_sal().line
    assert actual.older().older().find_sal().line == caller_line
    check_string(first)
    gdb.execute('continue')
    for consumer_line in consumers:
        actual = gdb.selected_frame()
        assert actual.name() == 'debug_values::count', ('repeated string boundary', actual.name())
        assert actual.older().name() == 'debug_values::reuse_text'
        assert actual.older().find_sal().line == consumer_line
        check_string(first)
        gdb.execute('continue')
for first, caller_line in [(0x3bc, {formatted_literal_line}), (None, {formatted_empty_line}), (0x3bc, {formatted_warning_line})]:
    actual = gdb.selected_frame()
    assert actual.name() == 'debug_values::echo_text', ('formatted string frame', actual.name())
    assert actual.older().name() == 'T::stimulus'
    assert actual.older().find_sal().line == caller_line
    check_string(first)
    gdb.execute('continue')
string_stop.delete()
folded_stop.delete()
assert gdb.selected_frame().name() == 'debug_values::inspect_payload', ('unexpected source stop', gdb.selected_frame().name(), gdb.selected_frame().older().find_sal().line)
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
            folded_literal_line = line("folded literal string call"),
            folded_empty_line = line("folded empty string call"),
            folded_nested_line = line("nested folded string call"),
            selected_literal_line = line("selected folded string call"),
            reused_literal_line = line("reused literal string call"),
            reused_empty_line = line("reused empty string call"),
            reused_assignment_line = line("reused string assignment"),
            first_consumer_line = line("first reused string consumer"),
            second_consumer_line = line("second reused string consumer"),
            first_only_line = line("first-only empty string call"),
            second_only_line = line("second-only empty string call"),
            formatted_literal_line = line("formatted literal string call"),
            formatted_empty_line = line("formatted empty string call"),
            formatted_warning_line = line("formatted warning string call"),
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
        let finish = format!(
            r#"import gdb
class Empty(gdb.Breakpoint):
    def stop(self):
        parent = gdb.selected_frame().older()
        return parent.name() == 'T::stimulus' and parent.find_sal().line == {caller_line}
stop = Empty({source:?} + ':{return_line}')
gdb.execute('run')
assert gdb.selected_frame().name() == 'debug_values::echo_text'
for name in ['value', 'copy']:
    view = gdb.parse_and_eval(name)
    assert int(view['length']) == 0 and int(view['byte_length']) == 0
    assert int(view['data']) == 0 and int(view['utf8']) == 0
history = gdb.execute('show values', to_string=True)
gdb.execute('finish')
assert gdb.selected_frame().name() == 'T::stimulus'
assert history == gdb.execute('show values', to_string=True), 'empty call invented a return value'
stop.delete()
gdb.execute('continue')
assert int(gdb.parse_and_eval('$_exitcode')) == 0
"#,
            source = source.to_string_lossy(),
            caller_line = line("folded empty string call"),
            return_line = line("borrowed string return stop"),
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
                    &format!("python {finish}"),
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
fn source_real_returns_match_the_debugger_finish_abi() {
    let directory = std::env::temp_dir().join(format!("siox_dwarf_return_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    for inline in ["never", "always"] {
        let source = directory.join(format!("returns_{inline}.siox"));
        let text = format!(
            r#"module debug_returns;
#[inline({inline})]
fn identity(value: real) -> real {{
    return value; // real return stop
}}
#[test] entity T {{}}
impl T {{
    stimulus: process {{
        let first: real = identity(3.5);
        assert!(first == 3.5, "first real result");
        let second: real = identity(5.25);
        assert!(second == 5.25, "second real result");
        let negative: real = identity(-2.75);
        assert!(negative == -2.75, "negative real result");
        let zero: real = identity(-0.0);
        assert!(zero == 0.0, "negative zero result");
    }}
}}
"#
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
        if Command::new("gdb").arg("--version").output().is_ok() {
            let line = text
                .lines()
                .position(|line| line.contains("real return stop"))
                .unwrap()
                + 1;
            let python = format!(
                r#"import gdb, math
stop = gdb.Breakpoint({source:?} + ':{line}')
gdb.execute('run')
for expected in [3.5, 5.25, -2.75, -0.0]:
    assert gdb.selected_frame().name() == 'debug_returns::identity'
    assert float(gdb.parse_and_eval('value')) == expected
    report = gdb.execute('finish', to_string=True)
    returned = float(gdb.history(0))
    assert returned == expected, ('real finish ABI', returned, expected, report)
    assert math.copysign(1.0, returned) == math.copysign(1.0, expected), ('real finish sign', returned, expected)
    assert gdb.selected_frame().name() == 'T::stimulus'
    gdb.execute('continue')
assert int(gdb.parse_and_eval('$_exitcode')) == 0
"#,
                source = source.to_string_lossy()
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
            eprintln!("GDB unavailable: native real return checked, finish probe skipped");
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn wide_source_returns_keep_declared_bindings() {
    let directory =
        std::env::temp_dir().join(format!("siox_dwarf_wide_return_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    for width in [81usize, 127, 128, 129, 256] {
        for inline in ["never", "always"] {
            let source = directory.join(format!("returns_{width}_{inline}.siox"));
            let signed_bits = format!("{}0{}", "1".repeat(width - 65), "1".repeat(64));
            let text = format!(
                r#"module debug_wide_returns;
use std::bits::{{signed, unsigned}};
#[inline({inline})]
fn signed_identity(value: signed[{width}]) -> signed[{width}] {{
    let copy: signed[{width}] = value;
    return copy; // signed wide return stop
}}
#[inline({inline})]
fn unsigned_identity(value: unsigned[{width}]) -> unsigned[{width}] {{
    let copy: unsigned[{width}] = value;
    return copy; // unsigned wide return stop
}}
#[inline({inline})]
fn low_byte(value: unsigned[{width}]) -> unsigned[8] {{
    let copy: unsigned[{width}] = value;
    return unsigned[8](copy); // narrow callee return stop
}}
#[inline({inline})]
fn generic_identity<T>(value: T) -> T {{
    let copy: T = value;
    return copy; // generic wide return stop
}}
#[test] entity T {{}}
impl T {{
    stimulus: process {{
        let negative: signed[{width}] = signed_identity(-2);
        assert!(negative == -2, "negative wide result");
        let large_negative: signed[{width}] = signed_identity(-18446744073709551617);
        assert!(large_negative == -18446744073709551617, "large negative wide result");
        let signed_zero: signed[{width}] = signed_identity(0);
        assert!(signed_zero == 0, "signed zero result");
        let positive: signed[{width}] = signed_identity(18446744073709551617);
        assert!(positive == 18446744073709551617, "positive wide result");
        let large: unsigned[{width}] = unsigned_identity(1208925819614629174706177);
        assert!(large == 1208925819614629174706177, "large unsigned result");
        let unsigned_zero: unsigned[{width}] = unsigned_identity(0);
        assert!(unsigned_zero == 0, "unsigned zero result");
        let narrow: unsigned[8] = unsigned[8](unsigned_identity(1208925819614629174706177));
        assert!(narrow == 1, "narrow consumer result");
        let wider: unsigned[193] = unsigned[193](unsigned_identity(18446744073709551617));
        assert!(wider == 18446744073709551617, "wider consumer result");
        let low: unsigned[8] = low_byte(1208925819614629174706177);
        assert!(low == 1, "narrow callee result");
        let widened_low: unsigned[193] = unsigned[193](low_byte(1208925819614629174706177));
        assert!(widened_low == 1, "wide private payload retains the narrow declared result");
        let generic_signed: signed[{width}] = generic_identity(signed[{width}](0b{signed_bits}));
        assert!(generic_signed == large_negative, "generic signed result");
        let generic_unsigned: unsigned[{width}] = generic_identity(unsigned[{width}](1208925819614629174706177));
        assert!(generic_unsigned == large, "generic unsigned result");
    }}
}}
"#
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
            if Command::new("gdb").arg("--version").output().is_ok() {
                let line =
                    |marker| text.lines().position(|line| line.contains(marker)).unwrap() + 1;
                let python = format!(
                    r#"import gdb
# Unsupported native integer classifiers must not see the source return ABI.
# Wide source frames expose complete actual results through the existing
# read-only snapshot; narrowed consumers leave omitted result bits unavailable.
stops = [gdb.Breakpoint({source:?} + ':' + str(line)) for line in [{signed_line}, {unsigned_line}, {low_line}, {generic_line}]]
gdb.execute('run')
for name, expected, complete in [('signed_identity', -2, True), ('signed_identity', -(1 << 64) - 1, True), ('signed_identity', 0, True), ('signed_identity', (1 << 64) + 1, True), ('unsigned_identity', (1 << 80) + 1, True), ('unsigned_identity', 0, True), ('unsigned_identity', (1 << 80) + 1, False), ('unsigned_identity', (1 << 64) + 1, True), ('low_byte', (1 << 80) + 1, True), ('low_byte', (1 << 80) + 1, True), ('generic_identity', -(1 << 64) - 1, True), ('generic_identity', (1 << 80) + 1, True)]:
    assert gdb.selected_frame().name().split('(')[0] == 'debug_wide_returns::' + name
    for binding in ['value', 'copy']:
        actual = gdb.parse_and_eval(binding)
        assert int(actual) == expected, ('wide source binding', name, binding, actual, expected)
        assert actual.type.sizeof == ({width} + 7) // 8
    if name == 'low_byte' and gdb.selected_frame().type() == gdb.NORMAL_FRAME:
        gdb.execute('finish')
        returned = gdb.history(0)
        assert int(returned) == 1 and returned.type.sizeof == 1, ('narrow finish', returned)
    else:
        frame = gdb.selected_frame()
        assert frame.type() == gdb.INLINE_FRAME, frame.type()
        return_size = 1 if name == 'low_byte' else ({width} + 7) // 8
        returned_expected = (expected & 255) if name == 'low_byte' else expected
        assert frame.function().type.target().sizeof == return_size
        try:
            previous = frame.read_var('$return')
            assert previous.is_optimized_out or previous.is_unavailable, str(previous)
        except (gdb.error, ValueError):
            pass
        if complete:
            for _ in range(200):
                try:
                    returned = gdb.selected_frame().read_var('$return')
                    if not returned.is_optimized_out and int(returned) == returned_expected: break
                except (gdb.error, ValueError):
                    pass
                gdb.execute('stepi', to_string=True)
            else:
                raise AssertionError(('complete wide result unavailable', name, expected))
            assert returned.type.sizeof == return_size, returned.type
        else:
            try:
                returned = frame.read_var('$return')
                assert returned.is_optimized_out or returned.is_unavailable, str(returned)
            except (gdb.error, ValueError):
                pass
        assert 'Value returned is' not in gdb.execute('finish', to_string=True)
    assert gdb.selected_frame().name() == 'T::stimulus'
    gdb.execute('continue')
assert int(gdb.parse_and_eval('$_exitcode')) == 0
"#,
                    source = source.to_string_lossy(),
                    signed_line = line("signed wide return stop"),
                    unsigned_line = line("unsigned wide return stop"),
                    low_line = line("narrow callee return stop"),
                    generic_line = line("generic wide return stop"),
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
                eprintln!("GDB unavailable: native wide returns checked, binding probe skipped");
            }
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn mixed_aggregate_returns_match_the_debugger_finish_abi() {
    let directory =
        std::env::temp_dir().join(format!("siox_dwarf_mixed_return_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("mixed.siox");
    let text = r#"module debug_mixed_returns;
use std::bits::unsigned;
struct Pair { pub gain: real, pub tag: integer }
struct Reverse { pub tag: integer, pub gain: real }
struct Nested { pub pair: Pair }
struct Reals { pub values: real[2] }
struct Gain { pub gain: real }
struct Head { pub marker: Char, pub gain: real }
struct Tail { pub gain: real, pub marker: Char }
struct NestedHead { pub marker: Char, pub inner: Gain }
struct CharTail { pub gain: real, pub markers: Char[2] }
struct SmallHead { pub tag: unsigned[8], pub gain: real }
struct TinyHead { pub tag: unsigned[3], pub gain: real }
struct SmallTail { pub gain: real, pub first: unsigned[8], pub second: unsigned[8] }
struct SubPair { pub first: unsigned[3], pub second: signed[3] }
struct SubTail { pub gain: real, pub first: unsigned[3], pub second: signed[3] }
struct SubNested { pub gain: real, pub pair: SubPair }
struct SubArray { pub values: unsigned[3][3] }
fn make_pair(tag: integer, gain: real) -> Pair {
    return Pair { .gain = gain, .tag = tag }; // mixed pair return stop
}
fn make_reverse(tag: integer, gain: real) -> Reverse {
    return Reverse { .tag = tag, .gain = gain }; // reversed pair return stop
}
fn make_nested(tag: integer, gain: real) -> Nested {
    return Nested { .pair = Pair { .gain = gain, .tag = tag } }; // nested pair return stop
}
fn make_reals(gain: real) -> Reals {
    return Reals { .values = [gain, -gain] }; // real array return stop
}
fn make_gain(gain: real) -> Gain {
    return Gain { .gain = gain }; // real wrapper return stop
}
fn make_head(marker: Char, gain: real) -> Head {
    return Head { .marker = marker, .gain = gain }; // char head return stop
}
fn make_tail(marker: Char, gain: real) -> Tail {
    return Tail { .gain = gain, .marker = marker }; // char tail return stop
}
fn make_nested_head(marker: Char, gain: real) -> NestedHead {
    return NestedHead { .marker = marker, .inner = Gain { .gain = gain } }; // nested char head return stop
}
fn make_chars(marker: Char, gain: real) -> CharTail {
    return CharTail { .gain = gain, .markers = [marker, marker] }; // char array tail return stop
}
fn make_small_head(tag: unsigned[8], gain: real) -> SmallHead {
    return SmallHead { .tag = tag, .gain = gain }; // small head return stop
}
fn make_tiny_head(tag: unsigned[3], gain: real) -> TinyHead {
    return TinyHead { .tag = tag, .gain = gain }; // tiny head return stop
}
fn make_small_tail(first: unsigned[8], second: unsigned[8], gain: real) -> SmallTail {
    return SmallTail { .gain = gain, .first = first, .second = second }; // small tail return stop
}
fn make_subpair(first: unsigned[3], second: signed[3]) -> SubPair {
    return SubPair { .first = first, .second = second }; // subbyte pair return stop
}
fn make_subtail(first: unsigned[3], second: signed[3], gain: real) -> SubTail {
    return SubTail { .gain = gain, .first = first, .second = second }; // subbyte tail return stop
}
fn make_subnested(first: unsigned[3], second: signed[3], gain: real) -> SubNested {
    return SubNested { .gain = gain, .pair = SubPair { .first = first, .second = second } }; // subbyte nested return stop
}
fn make_subarray(first: unsigned[3]) -> SubArray {
    return SubArray { .values = [first, first + 1, first + 2] }; // subbyte array return stop
}
fn consume_pair(tag: integer, gain: real) -> real {
    let pair: Pair = make_pair(tag, gain);
    return pair.gain + real(pair.tag);
}
fn consume_reverse(tag: integer, gain: real) -> real {
    let pair: Reverse = make_reverse(tag, gain);
    return pair.gain + real(pair.tag);
}
fn consume_nested(tag: integer, gain: real) -> real {
    let nested: Nested = make_nested(tag, gain);
    return nested.pair.gain + real(nested.pair.tag);
}
fn consume_reals(gain: real) -> real {
    let reals: Reals = make_reals(gain);
    return reals.values[0] + reals.values[1];
}
fn consume_gain(gain: real) -> real {
    let single: Gain = make_gain(gain);
    return single.gain;
}
fn consume_head(marker: Char, gain: real) -> real {
    let value: Head = make_head(marker, gain);
    return value.gain + real(integer(value.marker));
}
fn consume_tail(marker: Char, gain: real) -> real {
    let value: Tail = make_tail(marker, gain);
    return value.gain + real(integer(value.marker));
}
fn consume_nested_head(marker: Char, gain: real) -> real {
    let value: NestedHead = make_nested_head(marker, gain);
    return value.inner.gain + real(integer(value.marker));
}
fn consume_chars(marker: Char, gain: real) -> real {
    let value: CharTail = make_chars(marker, gain);
    return value.gain + real(integer(value.markers[0])) + real(integer(value.markers[1]));
}
fn consume_small_head(tag: integer, gain: real) -> real {
    let value: SmallHead = make_small_head(unsigned[8](tag), gain);
    return value.gain + real(integer(value.tag));
}
fn consume_tiny_head(tag: integer, gain: real) -> real {
    let value: TinyHead = make_tiny_head(unsigned[3](tag), gain);
    return value.gain + real(integer(value.tag));
}
fn consume_small_tail(tag: integer, gain: real) -> real {
    let value: SmallTail = make_small_tail(unsigned[8](tag), unsigned[8](tag + 1), gain);
    return value.gain + real(integer(value.first)) + real(integer(value.second));
}
fn consume_subpair(tag: integer) -> integer {
    let value: SubPair = make_subpair(unsigned[3](tag), signed[3](tag));
    return integer(value.first) + integer(value.second);
}
fn consume_subtail(tag: integer, gain: real) -> real {
    let value: SubTail = make_subtail(unsigned[3](tag), signed[3](tag), gain);
    return value.gain + real(integer(value.first)) + real(integer(value.second));
}
fn consume_subnested(tag: integer, gain: real) -> real {
    let value: SubNested = make_subnested(unsigned[3](tag), signed[3](tag), gain);
    return value.gain + real(integer(value.pair.first)) + real(integer(value.pair.second));
}
fn consume_subarray(tag: integer) -> integer {
    let value: SubArray = make_subarray(unsigned[3](tag));
    return if tag > 0 {
        integer(value.values[0]) + integer(value.values[1]) + integer(value.values[2])
    } else {
        integer(value.values[2]) + integer(value.values[1]) + integer(value.values[0])
    };
}
entity Probe { tag: integer in, gain: real in, marker: Char in, pair: real out, reverse: real out, nested: real out, reals: real out, single: real out, head: real out, tail: real out, nested_head: real out, chars: real out, small_head: real out, small_tail: real out, tiny_head: real out, subpair: integer out, subtail: real out, subnested: real out, subarray: integer out }
impl Probe {
    pair = consume_pair(tag, gain);
    reverse = consume_reverse(tag, gain);
    nested = consume_nested(tag, gain);
    reals = consume_reals(gain);
    single = consume_gain(gain);
    head = consume_head(marker, gain);
    tail = consume_tail(marker, gain);
    nested_head = consume_nested_head(marker, gain);
    chars = consume_chars(marker, gain);
    small_head = consume_small_head(tag, gain);
    tiny_head = consume_tiny_head(tag, gain);
    small_tail = consume_small_tail(tag, gain);
    subpair = consume_subpair(tag);
    subtail = consume_subtail(tag, gain);
    subnested = consume_subnested(tag, gain);
    subarray = consume_subarray(tag);
}
#[test] entity T {}
impl T {
    let tag: integer = 5;
    let gain: real = 1.5;
    let marker: Char = 'λ';
    let probe: Probe = { .tag = tag, .gain = gain, .marker = marker };
    stimulus: process {
        await 1ns;
        assert!(probe.pair == 6.5 and probe.reverse == 6.5 and probe.nested == 6.5, "positive mixed result");
        assert!(probe.reals == 0.0 and probe.single == 1.5, "positive real aggregate");
        assert!(probe.head == 956.5 and probe.tail == 956.5 and probe.nested_head == 956.5 and probe.chars == 1911.5, "positive char aggregate");
        assert!(probe.small_head == 6.5 and probe.small_tail == 12.5, "positive subword aggregate");
        assert!(probe.tiny_head == 6.5, "positive normalized subword aggregate");
        assert!(probe.subpair == 2 and probe.subtail == 3.5 and probe.subnested == 3.5 and probe.subarray == 18, "positive subbyte register aggregates");
        tag = -7;
        gain = -2.5;
        marker = 'β';
        await 1ns;
        assert!(probe.pair == -9.5 and probe.reverse == -9.5 and probe.nested == -9.5, "negative mixed result");
        assert!(probe.reals == 0.0 and probe.single == -2.5, "negative real aggregate");
        assert!(probe.head == 943.5 and probe.tail == 943.5 and probe.nested_head == 943.5 and probe.chars == 1889.5, "negative char aggregate");
        assert!(probe.small_head == 246.5 and probe.small_tail == 496.5, "negative subword aggregate");
        assert!(probe.tiny_head == -1.5, "negative normalized subword aggregate");
        assert!(probe.subpair == 2 and probe.subtail == -0.5 and probe.subnested == -0.5 and probe.subarray == 6, "negative subbyte register aggregates");
        tag = 0;
        gain = -0.0;
        await 1ns;
        assert!(probe.pair == 0.0 and probe.reverse == 0.0 and probe.nested == 0.0, "zero mixed result");
        assert!(probe.reals == 0.0 and probe.single == 0.0, "zero real aggregate");
        assert!(probe.head == 946.0 and probe.tail == 946.0 and probe.nested_head == 946.0 and probe.chars == 1892.0, "zero char aggregate");
        assert!(probe.small_head == 0.0 and probe.small_tail == 1.0, "zero subword aggregate");
        assert!(probe.tiny_head == 0.0, "zero normalized subword aggregate");
        assert!(probe.subpair == 0 and probe.subtail == 0.0 and probe.subnested == 0.0 and probe.subarray == 3, "zero subbyte register aggregates");
    }
}
"#;
    for inline in ["never", "always"] {
        let text = text.replace("fn make_", &format!("#[inline({inline})]\nfn make_"));
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
        if Command::new("gdb").arg("--version").output().is_ok() {
            let line = |marker| text.lines().position(|line| line.contains(marker)).unwrap() + 1;
            let python = format!(
                r#"import gdb, math
operator_types = set()
operator_failures = []
class OperatorTypes(gdb.Breakpoint):
    def stop(self):
        try:
            frame = gdb.newest_frame()
            types = [frame.read_var(name).type for name in ['self', 'rhs']]
            assert all(ty.code == gdb.TYPE_CODE_INT and ty.sizeof == 1 for ty in types), types
            assert int(frame.read_var('rhs')) in [1, 2]
            assert int(frame.read_var('self')) == int(frame.older().read_var('first'))
            # Contextual literal operands have a width but no source labels.
            # Their companion exposes physical LSB-first positions explicitly.
            rhs = frame.read_var('rhs$logic')
            assert rhs.type.code == gdb.TYPE_CODE_STRUCT and rhs.type.sizeof == 3, str(rhs.type)
            assert 'bit positions' in rhs.type.name, rhs.type.name
            bits = rhs['bits']
            assert bits.type.range() == (0, 2), str(bits.type)
            number = int(frame.read_var('rhs'))
            assert ''.join(str(bits[i]).strip("'") for i in range(3)) == ''.join(str((number >> i) & 1) for i in range(3)), str(rhs)
            # GDB canonicalizes the C++ spelling of the DWARF base name unsigned[N].
            operator_types.add(tuple(ty.name.replace('unsigned int [', 'unsigned[') for ty in types))
        except Exception as error:
            operator_failures.append(str(error))
        return False
operator_stop = OperatorTypes('std::bits::add')
stops = [gdb.Breakpoint({source:?} + ':' + str(line)) for line in [{pair_line}, {reverse_line}, {nested_line}, {reals_line}, {gain_line}, {head_line}, {tail_line}, {nested_head_line}, {chars_line}, {small_head_line}, {small_tail_line}, {tiny_head_line}, {subpair_line}, {subtail_line}, {subnested_line}, {subarray_line}]]
seen = {{name: set() for name in ['make_pair', 'make_reverse', 'make_nested', 'make_reals', 'make_gain', 'make_head', 'make_tail', 'make_nested_head', 'make_chars', 'make_small_head', 'make_small_tail', 'make_tiny_head', 'make_subpair', 'make_subtail', 'make_subnested', 'make_subarray']}}
gdb.execute('run')
while True:
    frame = gdb.selected_frame()
    name = frame.name().split('::')[-1]
    assert name in seen, frame.name()
    gain = None if name in ['make_subpair', 'make_subarray'] else float(frame.read_var('gain'))
    sign = None if gain is None else math.copysign(1.0, gain)
    mixed = name in ['make_pair', 'make_reverse', 'make_nested']
    tag = int(frame.read_var('tag')) if mixed else None
    if name in ['make_head', 'make_tail', 'make_nested_head', 'make_chars']: tag = int(frame.read_var('marker'))
    if name in ['make_small_head', 'make_tiny_head']: tag = int(frame.read_var('tag'))
    if name == 'make_small_tail': tag = (int(frame.read_var('first')), int(frame.read_var('second')))
    if name in ['make_subpair', 'make_subtail', 'make_subnested']: tag = (int(frame.read_var('first')), int(frame.read_var('second')))
    if name == 'make_subarray': tag = int(frame.read_var('first'))
    seen[name].add((tag, gain, sign))
    gdb.execute('finish')
    result = gdb.history(0)
    sizes = {{'make_gain': 8, 'make_head': 12, 'make_tail': 12, 'make_nested_head': 12, 'make_small_head': 9, 'make_small_tail': 10, 'make_tiny_head': 9, 'make_subpair': 2, 'make_subtail': 10, 'make_subnested': 10, 'make_subarray': 3}}
    assert result.type.sizeof == sizes.get(name, 16), str(result.type)
    if name == 'make_nested': result = result['pair']
    if mixed: assert int(result['tag']) == tag, (name, str(result), tag)
    if name in ['make_head', 'make_tail', 'make_nested_head']: assert int(result['marker']) == tag, (name, str(result), tag)
    if name == 'make_nested_head': result = result['inner']
    if name == 'make_chars': assert [int(result['markers'][i]) for i in range(2)] == [tag, tag], (name, str(result), tag)
    if name in ['make_small_head', 'make_tiny_head']: assert int(result['tag']) == tag, (name, str(result), tag)
    if name == 'make_small_tail': assert (int(result['first']), int(result['second'])) == tag, (name, str(result), tag)
    if name in ['make_subpair', 'make_subtail', 'make_subnested']:
        pair = result['pair'] if name == 'make_subnested' else result
        assert (int(pair['first']), int(pair['second'])) == tag, (name, str(result), tag)
    if name == 'make_subarray': assert [int(result['values'][i]) for i in range(3)] == [(tag + i) & 7 for i in range(3)], (name, str(result), tag)
    gains = [] if gain is None else [float(result['values'][i]) for i in range(2)] if name == 'make_reals' else [float(result['gain'])]
    expected_gains = [] if gain is None else [gain, -gain] if name == 'make_reals' else [gain]
    for actual_gain, expected_gain in zip(gains, expected_gains):
        assert actual_gain == expected_gain and math.copysign(1.0, actual_gain) == math.copysign(1.0, expected_gain), (name, str(result), expected_gain)
    gdb.execute('continue')
    if gdb.selected_thread() is None: break
assert int(gdb.parse_and_eval('$_exitcode')) == 0
assert operator_types == {{('unsigned[3]', 'unsigned[3]')}}, operator_types
assert not operator_failures, operator_failures
# Independent signal writes can expose intermediate delta-cycle inputs.
# Check every actual invocation above, and require all settled cases below.
for name, calls in seen.items():
    expected = [(5, 1.5, 1.0), (-7, -2.5, -1.0), (0, -0.0, -1.0)]
    if name in ['make_reals', 'make_gain']: expected = [(None, gain, sign) for _, gain, sign in expected]
    if name in ['make_head', 'make_tail', 'make_nested_head', 'make_chars']: expected = [(955, 1.5, 1.0), (946, -2.5, -1.0), (946, -0.0, -1.0)]
    if name == 'make_small_head': expected = [(5, 1.5, 1.0), (249, -2.5, -1.0), (0, -0.0, -1.0)]
    if name == 'make_tiny_head': expected = [(5, 1.5, 1.0), (1, -2.5, -1.0), (0, -0.0, -1.0)]
    if name == 'make_small_tail': expected = [((5, 6), 1.5, 1.0), ((249, 250), -2.5, -1.0), ((0, 1), -0.0, -1.0)]
    if name in ['make_subpair', 'make_subtail', 'make_subnested']: expected = [((5, -3), 1.5, 1.0), ((1, 1), -2.5, -1.0), ((0, 0), -0.0, -1.0)]
    if name == 'make_subpair': expected = [(tag, None, None) for tag, _, _ in expected]
    if name == 'make_subarray': expected = [(5, None, None), (1, None, None), (0, None, None)]
    assert set(expected).issubset(calls), (name, calls)
"#,
                source = source.to_string_lossy(),
                pair_line = line("mixed pair return stop"),
                reverse_line = line("reversed pair return stop"),
                nested_line = line("nested pair return stop"),
                reals_line = line("real array return stop"),
                gain_line = line("real wrapper return stop"),
                head_line = line("// char head return stop"),
                tail_line = line("char tail return stop"),
                nested_head_line = line("nested char head return stop"),
                chars_line = line("char array tail return stop"),
                small_head_line = line("small head return stop"),
                small_tail_line = line("small tail return stop"),
                tiny_head_line = line("tiny head return stop"),
                subpair_line = line("subbyte pair return stop"),
                subtail_line = line("subbyte tail return stop"),
                subnested_line = line("subbyte nested return stop"),
                subarray_line = line("subbyte array return stop")
            );
            success(
                Command::new("gdb")
                    .args(["-nx", "-q", "--batch"])
                    .arg(&debug)
                    .args([
                        "-ex",
                        "set disable-randomization off",
                        "-ex",
                        &format!("python {python}"),
                    ])
                    .output()
                    .unwrap(),
            );
        } else {
            eprintln!("GDB unavailable: native mixed returns checked, finish probe skipped");
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn large_aggregate_returns_match_the_debugger_finish_abi() {
    let directory =
        std::env::temp_dir().join(format!("siox_dwarf_memory_return_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("memory.siox");
    let first = (1u128 << 80) + 13;
    let second = (1u128 << 79) + 9;
    let text = r#"module debug_memory_returns;
use std::bits::{unsigned, signed};
struct Triple { pub left: integer, pub gain: real, pub right: integer }
struct Wide { pub data: unsigned[81], pub gain: real }
struct Signed { pub data: signed[81], pub gain: real }
struct Nested { pub body: Signed, pub gains: real[2..0] }
#[inline(__INLINE__)]
fn make_triple(left: integer, gain: real, right: integer) -> Triple {
    return Triple { .left = left, .gain = gain, .right = right }; // three word return stop
}
#[inline(__INLINE__)]
fn make_wide(data: unsigned[81], gain: real) -> Wide {
    return Wide { .data = data, .gain = gain }; // normalized wide return stop
}
#[inline(__INLINE__)]
fn make_nested(data: signed[81], gain: real) -> Nested {
    return Nested { .body = Signed { .data = data, .gain = gain }, .gains = [gain, -gain, gain] }; // nested memory return stop
}
fn consume_triple(left: integer, gain: real, right: integer) -> real {
    let value: Triple = make_triple(left, gain, right);
    return real(value.left) + value.gain + real(value.right);
}
fn consume_wide(data: unsigned[81], gain: real) -> real {
    let value: Wide = make_wide(data, gain);
    return (if value.data == data { 1.0 } else { -1.0 }) * value.gain;
}
fn consume_nested(data: signed[81], gain: real) -> real {
    let value: Nested = make_nested(data, gain);
    return (if value.body.data == data { 1.0 } else { -1.0 }) * (value.body.gain + value.gains[2] + value.gains[1] + value.gains[0]);
}
entity Probe { left: integer in, gain: real in, right: integer in, data: unsigned[81] in, signed_data: signed[81] in, triple: real out, wide: real out, nested: real out }
impl Probe {
    triple = consume_triple(left, gain, right);
    wide = consume_wide(data, gain);
    nested = consume_nested(signed_data, gain);
}
#[test] entity T {}
impl T {
    let left: integer = 5;
    let gain: real = 1.5;
    let right: integer = 13;
    let data: unsigned[81] = unsigned[81](0b__FIRST__);
    let signed_data: signed[81] = signed[81](5);
    let probe: Probe = { .left = left, .gain = gain, .right = right, .data = data, .signed_data = signed_data };
    stimulus: process {
        await 1ns;
        assert!(probe.triple == 19.5 and probe.wide == 1.5 and probe.nested == 3.0, "positive memory result");
        left = -7;
        gain = -2.5;
        right = 9;
        data = unsigned[81](0b__SECOND__);
        signed_data = signed[81](0b__SIGNED__);
        await 1ns;
        assert!(probe.triple == -0.5 and probe.wide == -2.5 and probe.nested == -5.0, "negative memory result");
        left = 0;
        gain = -0.0;
        right = 0;
        data = unsigned[81](0);
        signed_data = signed[81](0);
        await 1ns;
        assert!(probe.triple == 0.0 and probe.wide == 0.0 and probe.nested == 0.0, "zero memory result");
    }
}
"#;
    for inline in ["never", "always"] {
        let text = text
            .replace("__INLINE__", inline)
            .replace("__FIRST__", &format!("{first:081b}"))
            .replace("__SECOND__", &format!("{second:081b}"))
            .replace("__SIGNED__", &format!("{:081b}", (1u128 << 81) - 7));
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
        if Command::new("gdb").arg("--version").output().is_ok() {
            let line = |marker| text.lines().position(|line| line.contains(marker)).unwrap() + 1;
            let python = format!(
                r#"import gdb, math
stops = [gdb.Breakpoint({source:?} + ':' + str(line)) for line in [{triple_line}, {wide_line}, {nested_line}]]
seen = {{'make_triple': set(), 'make_wide': set(), 'make_nested': set()}}
gdb.execute('run')
while True:
    frame = gdb.selected_frame()
    name = frame.name().split('::')[-1]
    assert name in seen, frame.name()
    gain = float(frame.read_var('gain'))
    sign = math.copysign(1.0, gain)
    if name == 'make_triple':
        left, right = int(frame.read_var('left')), int(frame.read_var('right'))
        seen[name].add((left, right, gain, sign))
    else:
        data = int(frame.read_var('data'))
        seen[name].add((data, gain, sign))
    gdb.execute('finish')
    result = gdb.history(0)
    assert result.type.sizeof == {{'make_triple': 24, 'make_wide': 19, 'make_nested': 43}}[name], str(result.type)
    if name == 'make_triple':
        assert int(result['left']) == left and int(result['right']) == right, (name, str(result), left, right)
    elif name == 'make_wide':
        assert int(result['data']) == data, (name, str(result), data)
    else:
        assert int(result['body']['data']) == data, (name, str(result), data)
        for index, expected in [(2, gain), (1, -gain), (0, gain)]:
            actual = float(result['gains']['[' + str(index) + ']'])
            assert actual == expected and math.copysign(1.0, actual) == math.copysign(1.0, expected), (index, actual, expected)
    actual_gain = float(result['body']['gain'] if name == 'make_nested' else result['gain'])
    assert actual_gain == gain and math.copysign(1.0, actual_gain) == sign, (name, str(result), gain, sign)
    gdb.execute('continue')
    if gdb.selected_thread() is None: break
assert int(gdb.parse_and_eval('$_exitcode')) == 0
assert set([(5, 13, 1.5, 1.0), (-7, 9, -2.5, -1.0), (0, 0, -0.0, -1.0)]).issubset(seen['make_triple']), seen
assert set([({first}, 1.5, 1.0), ({second}, -2.5, -1.0), (0, -0.0, -1.0)]).issubset(seen['make_wide']), seen
assert set([(5, 1.5, 1.0), (-7, -2.5, -1.0), (0, -0.0, -1.0)]).issubset(seen['make_nested']), seen
"#,
                source = source.to_string_lossy(),
                triple_line = line("three word return stop"),
                wide_line = line("normalized wide return stop"),
                nested_line = line("nested memory return stop"),
            );
            success(
                Command::new("gdb")
                    .args(["-nx", "-q", "--batch"])
                    .arg(&debug)
                    .args([
                        "-ex",
                        "set disable-randomization off",
                        "-ex",
                        &format!("python {python}"),
                    ])
                    .output()
                    .unwrap(),
            );
        } else {
            eprintln!("GDB unavailable: native memory returns checked, finish probe skipped");
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn source_string_returns_match_the_debugger_finish_abi() {
    let directory =
        std::env::temp_dir().join(format!("siox_dwarf_string_return_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("strings.siox");
    std::fs::write(directory.join("a.txt"), "β\0🙂").unwrap();
    std::fs::write(directory.join("b.txt"), "λ\0🙂").unwrap();
    let text = r#"module debug_string_returns;
use std::primitive::string;
#[inline(__INLINE__)]
fn echo_text(value: string) -> string {
    let copy: string = value;
    return copy; // source string return stop
}
#[test] entity A {}
impl A {
    stimulus: process {
        let runtime: string = read<string>("a.txt");
        let first: string = echo_text(runtime);
        await 1ns;
        let second: string = echo_text(first);
        let literal: integer = echo_text("μ\0🙂")'length;
        let single: integer = echo_text("μ")'length;
        let empty: integer = echo_text("")'length;
        assert!(first == runtime and second == runtime and second'length == 3, "runtime string return after await");
        assert!(literal == 3 and single == 1 and empty == 0, "literal/empty string returns");
    }
}
#[test] entity B {}
impl B {
    stimulus: process {
        let runtime: string = read<string>("b.txt");
        let borrowed: string = echo_text(runtime);
        let literal: integer = echo_text("μ\0🙂")'length;
        let single: integer = echo_text("μ")'length;
        let empty: integer = echo_text("")'length;
        assert!(borrowed == runtime and borrowed'length == 3, "next root string return");
        assert!(literal == 3 and single == 1 and empty == 0, "next root literal/empty returns");
    }
}
"#;
    for inline in ["never", "always"] {
        let text = text.replace("__INLINE__", inline);
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
            let line = text
                .lines()
                .position(|line| line.contains("source string return stop"))
                .unwrap()
                + 1;
            let python = format!(
                r#"import gdb
stop = gdb.Breakpoint({source:?} + ':{line}')
seen = {{(0x3b2, 0, 0x1f642): 0, (0x3bb, 0, 0x1f642): 0, (0x3bc, 0, 0x1f642): 0, (0x3bc,): 0, (): 0}}
def contents(view):
    length = int(view['length'])
    byte_length = int(view['byte_length'])
    assert length in (0, 1, 3) and byte_length == {{0: 0, 1: 2, 3: 7}}[length], str(view)
    points = tuple(int(view['data'][i]) for i in range(length))
    if length:
        if length == 3: assert int(view['utf8'][2]) == 0, str(view)
        actual = bytes(int(view['utf8'][i]) for i in range(byte_length))
        assert actual == ''.join(chr(point) for point in points).encode('utf-8'), actual
    return points
def last():
    try: return str(gdb.history(0))
    except gdb.error: return None
gdb.execute('run')
while True:
    frame = gdb.selected_frame()
    assert frame.name() == 'debug_string_returns::echo_text', frame.name()
    expected = contents(frame.read_var('value'))
    assert contents(frame.read_var('copy')) == expected
    seen[expected] += 1
    previous = last()
    gdb.execute('finish')
    if expected:
        result = gdb.history(0)
        assert result.type.sizeof == 32 and contents(result) == expected, str(result)
    else:
        assert last() == previous, 'empty specialization fabricated a return value'
    gdb.execute('continue')
    if gdb.selected_thread() is None: break
assert int(gdb.parse_and_eval('$_exitcode')) == 0
assert seen == {{(0x3b2, 0, 0x1f642): 2, (0x3bb, 0, 0x1f642): 1, (0x3bc, 0, 0x1f642): 2, (0x3bc,): 2, (): 2}}, seen
"#,
                source = source.to_string_lossy()
            );
            success(
                Command::new("gdb")
                    .current_dir(&directory)
                    .args(["-nx", "-q", "--batch"])
                    .arg(&debug)
                    .args([
                        "-ex",
                        "set disable-randomization off",
                        "-ex",
                        &format!("python {python}"),
                    ])
                    .output()
                    .unwrap(),
            );
        } else {
            eprintln!("GDB unavailable: native string returns checked, finish probe skipped");
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn packed_expression_bindings_expose_logic_companions() {
    let directory = std::env::temp_dir().join(format!(
        "siox_dwarf_expression_logic_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("companions.siox");
    let text = r#"module debug_expression_logic;
use std::bits::unsigned;
struct Packet { pub tag: integer, pub data: unsigned[9], pub tail: unsigned[3], pub matrix: unsigned[3][-1..-2] }
fn tap(value: unsigned[9]) -> Logic {
    let copy: unsigned[9] = value;
    let result: Logic = copy[6];
    return result; // expression logic return stop
}
fn tap_packet(value: Packet) -> Logic {
    let copy: Packet = value;
    let result: Logic = copy.data[6];
    return result; // aggregate logic return stop
}
fn tap_tag(value: Packet) -> integer {
    let copy: Packet = value;
    return copy.tag; // partial hardware logic return stop
}
fn value_only(value: unsigned[9]) -> integer {
    let copy: unsigned[9] = value;
    return integer(copy); // unavailable logic return stop
}
fn tap_wide(value: unsigned[81]) -> Logic {
    let copy: unsigned[81] = value;
    let result: Logic = copy[78];
    return result; // wide logic return stop
}
fn echo_packed(value: unsigned[9]) -> unsigned[9] {
    let copy: unsigned[9] = value;
    return copy; // numeric packed return stop
}
fn nested_packed(value: unsigned[9]) -> Logic {
    let returned: unsigned[9] = echo_packed(value);
    let result: Logic = returned[6];
    return result; // nested packed return stop
}
fn selected(value: unsigned[9], take: integer) -> Logic {
    if take > 0 {
        let copy: unsigned[9] = value;
        let result: Logic = copy[6];
        return result; // selected logic return stop
    }
    return '0'; // unselected logic return stop
}
entity Probe { value: Packet in, seen: integer out }
impl Probe { seen = tap_tag(value); }
#[test] entity T {}
impl T {
    let packet: Packet = { .tag = 7, .data = "01XZUWLH-", .tail = "Z1X", .matrix = ["Z10", "H-L"] };
    let seen: integer;
    let probe: Probe = { .value = packet, .seen = seen };
    stimulus: process {
        let states: unsigned[9] = "01XZUWLH-";
        let unknown: Logic = tap(states);
        assert!(unknown == 'X', "unknown expression result");
        let clean: unsigned[9] = "101010101";
        let known: Logic = tap(clean);
        assert!(known == '1', "known expression result");
        let nested_unknown: Logic = nested_packed(states);
        let nested_known: Logic = nested_packed(clean);
        assert!(nested_unknown == 'X' and nested_known == '1', "packed return metadata transport");
        let packet_result: Logic = tap_packet(packet);
        assert!(packet_result == 'X', "aggregate expression result");
        let number: integer = value_only(states);
        assert!(number == 130, "value-only expression result");
        let wide: unsigned[81] = "WIDE_STATES";
        let wide_result: Logic = tap_wide(wide);
        assert!(wide_result == 'X', "multiword expression result");
        let taken: Logic = selected(states, 1);
        let skipped: Logic = selected(states, 0);
        assert!(taken == 'X' and skipped == '0', "selected expression result");
        await 1ns;
        assert!(seen == 7, "hardware expression result");
    }
}
"#;
    let text = text.replace("WIDE_STATES", &"01XZUWLH-".repeat(9));
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
    if Command::new("gdb").arg("--version").output().is_ok() {
        let line = |marker| text.lines().position(|line| line.contains(marker)).unwrap() + 1;
        let python = format!(
            r#"import gdb
bp = gdb.Breakpoint({source:?} + ':{line}')
aggregate = gdb.Breakpoint({source:?} + ':{aggregate_line}')
partial = gdb.Breakpoint({source:?} + ':{partial_line}')
unavailable = gdb.Breakpoint({source:?} + ':{unavailable_line}')
wide = gdb.Breakpoint({source:?} + ':{wide_line}')
selected = gdb.Breakpoint({source:?} + ':{selected_line}')
unselected = gdb.Breakpoint({source:?} + ':{unselected_line}')
echo_packed = gdb.Breakpoint({source:?} + ':{echo_packed_line}')
nested_packed = gdb.Breakpoint({source:?} + ':{nested_packed_line}')
gdb.execute('run')
counts = {{'tap': 0, 'packet': 0, 'hardware': 0, 'unavailable': 0, 'wide': 0, 'selected': 0, 'unselected': 0, 'echo_packed': 0, 'nested_packed': 0}}
def symbols(view, labels=range(8, -1, -1)):
    return ''.join(str(view[i] if view.type.code == gdb.TYPE_CODE_ARRAY else view['[' + str(i) + ']']).strip("'") for i in labels)
def missing(frame, name):
    try:
        view = frame.read_var(name)
        assert view.is_optimized_out, ('missing metadata reported as available', name, str(view))
    except gdb.MemoryError:
        pass
while True:
    frame = gdb.selected_frame()
    if frame.name() == 'debug_expression_logic::tap':
        expected = ['01XZUWLH-', '101010101'][counts['tap']]
        counts['tap'] += 1
        for name in ['value$logic', 'copy$logic']:
            assert symbols(frame.read_var(name)) == expected, (name, str(frame.read_var(name)))
        assert str(frame.read_var('result')).strip("'") == expected[2]
    elif frame.name() == 'debug_expression_logic::tap_packet':
        assert frame.name() == 'debug_expression_logic::tap_packet'
        counts['packet'] += 1
        for name in ['value$logic', 'copy$logic']:
            view = frame.read_var(name)
            assert symbols(view['data']) == '01XZUWLH-', (name, str(view))
            assert int(view['tag']) == 7
            tail = view['tail']
            assert symbols(tail, range(2, -1, -1)) == 'Z1X'
            assert symbols(view['matrix']['[-1]'], range(2, -1, -1)) == 'Z10'
            assert symbols(view['matrix']['[-2]'], range(2, -1, -1)) == 'H-L'
    elif frame.name() == 'debug_expression_logic::tap_tag':
        counts['hardware'] += 1
        for name in ['value$logic', 'copy$logic']:
            view = frame.read_var(name)
            assert int(view['tag']) == 7
            assert view['data'].is_optimized_out and view['tail'].is_optimized_out and view['matrix'].is_optimized_out, (name, str(view))
    elif frame.name() == 'debug_expression_logic::value_only':
        assert frame.name() == 'debug_expression_logic::value_only'
        counts['unavailable'] += 1
        for name in ['value$logic', 'copy$logic']: missing(frame, name)
    elif frame.name() == 'debug_expression_logic::tap_wide':
        counts['wide'] += 1
        for name in ['value$logic', 'copy$logic']:
            view = frame.read_var(name)
            assert symbols(view, range(80, -1, -1)) == '01XZUWLH-' * 9
            assert view.type.sizeof == 81
        assert str(frame.read_var('result')).strip("'") == 'X'
    elif frame.name() == 'debug_expression_logic::echo_packed':
        expected = [130, 341][counts['echo_packed']]
        counts['echo_packed'] += 1
        for name in ['value', 'copy']: assert int(frame.read_var(name)) == expected
        # Only numeric bits execute inside this frame. Metadata is consumed
        # later in the caller; do not manufacture it here for inspection.
        for name in ['value$logic', 'copy$logic']: missing(frame, name)
        gdb.execute('finish', to_string=True)
        assert int(gdb.history(0)) == expected
    elif frame.name() == 'debug_expression_logic::nested_packed':
        expected = ['01XZUWLH-', '101010101'][counts['nested_packed']]
        counts['nested_packed'] += 1
        for name in ['value$logic', 'returned$logic']:
            assert symbols(frame.read_var(name)) == expected
        gdb.execute('finish', to_string=True)
        assert str(gdb.history(0)).strip("'") == expected[2]
    else:
        assert frame.name() == 'debug_expression_logic::selected'
        if frame.find_sal().line == {selected_line}:
            counts['selected'] += 1
            for name in ['value$logic', 'copy$logic']:
                assert symbols(frame.read_var(name)) == '01XZUWLH-'
        else:
            assert frame.find_sal().line == {unselected_line}
            counts['unselected'] += 1
            missing(frame, 'value$logic')
    gdb.execute('continue')
    if gdb.selected_thread() is None: break
assert int(gdb.parse_and_eval('$_exitcode')) == 0
assert counts['tap'] == 2 and counts['packet'] == 1 and counts['unavailable'] == 1 and counts['hardware'] >= 1, counts
assert counts['wide'] == counts['selected'] == counts['unselected'] == 1, counts
assert counts['echo_packed'] == counts['nested_packed'] == 2, counts
"#,
            source = source.to_string_lossy(),
            line = line("expression logic return stop"),
            aggregate_line = line("aggregate logic return stop"),
            partial_line = line("partial hardware logic return stop"),
            unavailable_line = line("unavailable logic return stop"),
            wide_line = line("wide logic return stop"),
            selected_line = line("selected logic return stop"),
            unselected_line = line("unselected logic return stop"),
            echo_packed_line = line("numeric packed return stop"),
            nested_packed_line = line("nested packed return stop"),
        );
        success(
            Command::new("gdb")
                .args(["-nx", "-q", "--batch"])
                .arg(&debug)
                .args([
                    "-ex",
                    "set disable-randomization off",
                    "-ex",
                    &format!("python {python}"),
                ])
                .output()
                .unwrap(),
        );
    } else {
        eprintln!("GDB unavailable: native packed results checked, companion probe skipped");
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn conversion_and_non_enum_resolve_keep_source_signatures() {
    let directory =
        std::env::temp_dir().join(format!("siox_dwarf_signatures_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("signatures.siox");
    let text = r#"module debug_signatures;
struct Source { pub count: integer, pub gain: real }
struct Target { pub count: integer, pub gain: real }
impl From<Source> for Target {
    #[inline(HINT)]
    fn from(value: Source) -> Self {
        return Target { .count = value.count + 1, .gain = value.gain };
    }
}
fn convert(value: Source) -> real {
    let converted: Target = Target(value);
    return real(converted.count) + converted.gain;
}
entity Probe { value: Source in, result: real out }
impl Probe { result = convert(value); }
struct Number(integer);
impl Resolve for Number {
    #[inline(HINT)]
    fn resolve(self, rhs: Self) -> Self {
        let first: integer = integer(self);
        let second: integer = integer(rhs);
        return if first > second { self } else { rhs };
    }
}
entity Resolved { first: Number in, second: Number in, result: Number out }
impl Resolved { result = first; }
impl Resolved { result = second; }
#[test] entity T {}
impl T {
    let value: Source = { .count = 7, .gain = -2.5 };
    let probe: Probe = { .value = value };
    let first: Number = Number(-5);
    let second: Number = Number(3);
    let resolved: Resolved = { .first = first, .second = second };
    stimulus: process {
        await 1ns;
        assert!(probe.result == 5.5, "composite conversion positive");
        assert!(integer(resolved.result) == 3, "mixed signs");
        value = { .count = -9, .gain = 3.5 };
        first = Number(7);
        await 1ns;
        assert!(probe.result == -4.5, "composite conversion negative");
        assert!(integer(resolved.result) == 7, "both positive");
        second = Number(-9);
        await 1ns;
        assert!(integer(resolved.result) == 7, "reversed signs");
        first = Number(-11);
        await 1ns;
        assert!(integer(resolved.result) == -9, "both negative");
    }
}
"#;
    let python = r#"import gdb
stops = [gdb.Breakpoint('debug_signatures::' + name) for name in ['from', 'resolve']]
conversions, resolutions = set(), set()
gdb.execute('run')
while gdb.selected_thread() is not None:
    frame = gdb.selected_frame()
    name = frame.name()
    if name == 'debug_signatures::from':
        value = frame.read_var('value')
        assert value.type.name == 'Source', value.type
        inputs = (int(value['count']), float(value['gain']))
        conversions.add(inputs)
        assert frame.function().type.target().name == 'Target', frame.function().type
    else:
        assert name == 'debug_signatures::resolve', name
        values = [frame.read_var(parameter) for parameter in ['self', 'rhs']]
        assert all(value.type.name == 'Number' and value.type.sizeof == 8 for value in values), values
        inputs = tuple(int(value) for value in values)
        resolutions.add(tuple(sorted(inputs)))
        assert frame.function().type.target().name == 'Number', frame.function().type
    gdb.execute('finish', to_string=True)
    result = gdb.history(0)
    if name == 'debug_signatures::from':
        assert result.type.name == 'Target', result.type
        assert (int(result['count']), float(result['gain'])) == (inputs[0] + 1, inputs[1]), (inputs, str(result))
    else:
        assert result.type.name == 'Number', result.type
        assert int(result) == max(inputs), (inputs, str(result))
    gdb.execute('continue')
assert int(gdb.parse_and_eval('$_exitcode')) == 0
assert {(7, -2.5), (-9, 3.5)} <= conversions, conversions
assert {(-5, 3), (3, 7), (-9, 7), (-11, -9)} <= resolutions, resolutions
"#;
    for hint in ["always", "never"] {
        std::fs::write(&source, text.replace("HINT", hint)).unwrap();
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
            success(
                Command::new("gdb")
                    .args(["-nx", "-q", "--batch"])
                    .arg(&debug)
                    .args([
                        "-ex",
                        "set disable-randomization off",
                        "-ex",
                        &format!("python {python}"),
                    ])
                    .output()
                    .unwrap(),
            );
        } else {
            eprintln!("GDB unavailable: native signatures checked, finish probe skipped");
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn odd_sized_aggregate_returns_keep_inspectable_source_frames() {
    let directory =
        std::env::temp_dir().join(format!("siox_dwarf_odd_return_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("odd.siox");
    let text = r#"module debug_odd;
use std::bits::unsigned;
struct Odd { pub value: unsigned[WIDTH], pub tail: unsigned[8] }
#[inline(HINT)]
fn make(value: unsigned[WIDTH], tail: unsigned[8]) -> Odd {
    return Odd { .value = value, .tail = tail };
}
#[inline(HINT)]
fn consume(value: unsigned[WIDTH], tail: unsigned[8]) -> integer {
    let odd: Odd = make(value, tail);
    return integer(unsigned[8](odd.value)) + integer(odd.tail);
}
entity Probe { value: unsigned[WIDTH] in, tail: unsigned[8] in, result: integer out }
impl Probe { result = consume(value, tail); }
#[test] entity T {}
impl T {
    let value: unsigned[WIDTH] = FIRST;
    let tail: unsigned[8] = 255;
    let probe: Probe = { .value = value, .tail = tail };
    stimulus: process {
        let initial: integer = consume(value, tail);
        assert!(initial == 256, "procedural first odd return");
        await 1ns;
        assert!(probe.result == 256, "hardware first odd return");
        value = SECOND;
        tail = 17;
        await 1ns;
        let updated: integer = consume(value, tail);
        assert!(updated == 24 and probe.result == 24, "updated odd returns");
    }
}
"#;
    for width in [17, 33, 65, 81] {
        let first = (1u128 << (width - 1)) + 1;
        let second = (1u128 << (width - 2)) + 7;
        let text = text
            .replace("WIDTH", &width.to_string())
            .replace("FIRST", &first.to_string())
            .replace("SECOND", &second.to_string());
        for hint in ["always", "never"] {
            std::fs::write(&source, text.replace("HINT", hint)).unwrap();
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
                let python = format!(
                    r#"import gdb
gdb.Breakpoint('debug_odd::make')
gdb.execute('run')
seen = {{'process': [], 'hardware': []}}
while gdb.selected_thread() is not None:
    frame = gdb.selected_frame()
    assert frame.name() == 'debug_odd::make' and frame.type() == gdb.INLINE_FRAME, frame.name()
    assert frame.function().type.target().name == 'Odd', frame.function().type
    inputs = (int(frame.read_var('value')), int(frame.read_var('tail')))
    # A reference must not expose a previous invocation's return snapshot.
    try:
        result = frame.read_var('$return')
        assert result.is_optimized_out or result['value'].is_optimized_out, str(result)
    except (gdb.MemoryError, ValueError):
        pass
    for _ in range(200):
        frame = gdb.selected_frame()
        assert frame.name() == 'debug_odd::make', frame.name()
        try:
            result = frame.read_var('$return')
            if (int(result['value']), int(result['tail'])) == inputs: break
        except (gdb.error, ValueError):
            pass
        gdb.execute('stepi', to_string=True)
    else:
        raise AssertionError('whole return snapshot unavailable')
    assert result['value'].type == frame.read_var('value').type, result['value'].type
    assert result['value'].type.sizeof == ({width} + 7) // 8
    assert 'Value returned is' not in gdb.execute('finish', to_string=True)
    frame = gdb.selected_frame()
    assert frame.name() == 'debug_odd::consume', frame.name()
    owner = frame.older().name()
    # Finish stops at the return address, before the caller has necessarily
    # completed its binding. Inspect after its normal capture, not before.
    for _ in range(200):
        frame = gdb.selected_frame()
        assert frame.name() == 'debug_odd::consume', frame.name()
        try:
            result = frame.read_var('odd')
            if (int(result['value']), int(result['tail'])) == inputs: break
        except (gdb.error, ValueError):
            pass
        gdb.execute('stepi', to_string=True)
    else:
        raise AssertionError('caller binding unavailable')
    seen['hardware' if '<hardware:' in owner else 'process'].append(inputs)
    gdb.execute('continue')
assert int(gdb.parse_and_eval('$_exitcode')) == 0
assert seen['process'] == [({first}, 255), ({second}, 17)], seen
assert {{({first}, 255), ({second}, 17)}} <= set(seen['hardware']), seen
"#
                );
                success(
                    Command::new("gdb")
                        .args(["-nx", "-q", "--batch"])
                        .arg(&debug)
                        .args([
                            "-ex",
                            "set disable-randomization off",
                            "-ex",
                            &format!("python {python}"),
                        ])
                        .output()
                        .unwrap(),
                );
            } else {
                eprintln!("GDB unavailable: native odd returns checked, frame probe skipped");
            }
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn checked_and_effectful_aggregates_preserve_demand_and_order() {
    let directory = std::env::temp_dir().join(format!("siox_dwarf_ordered_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("ordered.siox");
    let text = r#"module dwarf_ordered_aggregate;
extern "C" { fn putchar(value: integer) -> integer; fn abort() -> integer; }
struct Pair { pub left: integer, pub right: integer }
struct Flags { pub left: Bool, pub right: Bool }
#[inline(HINT)]
fn effects(value: integer) -> Pair {
    return Pair { .right = putchar(value + 1), .left = putchar(value) };
}
#[inline(HINT)]
fn mixed_effects(value: integer) -> Pair {
    return Pair { .left = value, .right = putchar(value + 1) };
}
#[inline(HINT)]
fn mixed(value: integer) -> integer {
    let pair: Pair = mixed_effects(value);
    return pair.left + pair.right;
}
#[inline(HINT)]
fn forward(value: integer) -> integer {
    let pair: Pair = effects(value);
    return pair.left + pair.right;
}
#[inline(HINT)]
fn reverse(value: integer) -> integer {
    let pair: Pair = effects(value);
    return pair.right + pair.left;
}
#[inline(HINT)]
fn between(value: integer) -> integer {
    let pair: Pair = effects(value);
    return pair.left + putchar(value + 2) + pair.right;
}
#[inline(HINT)]
fn checked(value: integer[2], index: integer) -> Pair {
    return Pair {
        .right = value[1 - index],
        .left = value[index]
    };
}
#[inline(HINT)]
fn checked_total(value: integer, index: integer, enabled: Bool) -> integer {
    let pair: Pair = checked([value, value + 3], index);
    return if enabled { pair.left + pair.right } else { 0 };
}
#[inline(HINT)]
fn quotients(value: integer, divisor: integer) -> Pair {
    return Pair { .right = 10 / (divisor - 1), .left = value / divisor };
}
#[inline(HINT)]
fn quotient_total(value: integer, divisor: integer) -> integer {
    let pair: Pair = quotients(value, divisor);
    return pair.left + pair.right;
}
#[inline(HINT)]
fn flags(value: integer) -> Flags {
    return Flags { .left = value < 0, .right = abort() != 0 };
}
#[inline(HINT)]
fn flags_total(value: integer) -> Bool {
    let pair: Flags = flags(value);
    return pair.left and pair.right;
}
entity E { value: integer in, index: integer in, divisor: integer in, enabled: Bool in,
    first: integer out, second: integer out, middle: integer out, mixed_sum: integer out,
    checked_sum: integer out, quotient: integer out, flag: Bool out }
impl E {
    first = if enabled { forward(value + 65) } else { 0 };
    second = if enabled { reverse(value + 75) } else { 0 };
    middle = if enabled { between(value + 85) } else { 0 };
    mixed_sum = if enabled { mixed(value + 105) } else { 0 };
    checked_sum = if enabled { checked_total(value, index, enabled) } else { 0 };
    quotient = quotient_total(value, divisor);
    flag = flags_total(value);
}
#[test] entity T {}
impl T {
    let value: integer = 0;
    let index: integer = 0;
    let divisor: integer = 0;
    let enabled: Bool = false;
    let e: E = { .value = value, .index = index, .divisor = divisor, .enabled = enabled };
    stimulus: process {
        await 1ns;
        assert!(e.first == 0 and e.second == 0 and e.middle == 0 and e.mixed_sum == 0 and e.checked_sum == 0,
            "untaken foreign calls and checks");
        assert!(e.quotient == -10 and not e.flag, "total division and short circuit");
        enabled = true; index = 0; divisor = 1; await 1ns;
        assert!(e.first == 131 and e.second == 151 and e.middle == 258 and e.checked_sum == 3,
            "ordered aggregate results");
        assert!(e.quotient == 0 and not e.flag, "zero sibling divisor");
        assert!(e.mixed_sum == 211, "effectful sibling beside pure group leader");
        value = 10; index = 1; divisor = 2; await 1ns;
        assert!(e.first == 151 and e.second == 171 and e.middle == 288 and e.checked_sum == 23,
            "changing aggregate results: {} {} {} {}", e.first, e.second, e.middle, e.checked_sum);
        assert!(e.quotient == 15 and not e.flag, "nonzero division");
        assert!(e.mixed_sum == 231, "mixed effectful return changes with input");
    }
}"#;
    let python = r#"import gdb
from collections import Counter

gdb.execute('set disable-randomization off')
stops = [gdb.Breakpoint('dwarf_ordered_aggregate::' + name)
         for name in ['effects', 'mixed_effects', 'checked', 'quotients', 'flags']]
gdb.execute('run')
seen = []
checked_seen = []
quotient_seen = []
while gdb.selected_thread() is not None:
    frame = gdb.selected_frame()
    name = frame.name().rsplit('::', 1)[-1]
    for stop in stops:
        stop.enabled = False
    # An in-place source frame does not force its argument before its ordinary
    # first consumer. Step until those actual bits have been captured.
    for _ in range(300):
        try:
            value = int(frame.read_var('value')[0] if name == 'checked' else frame.read_var('value'))
            break
        except (gdb.error, ValueError):
            gdb.execute('nexti', to_string=True)
            frame = gdb.selected_frame()
    else:
        raise AssertionError('consumed source argument unavailable')
    assert frame.name().rsplit('::', 1)[-1] == name, frame.name()
    index = int(frame.read_var('index')) if name == 'checked' else None
    divisor = int(frame.read_var('divisor')) if name == 'quotients' else None
    caller_frame = frame.older()
    while caller_frame.name().startswith('<'):
        caller_frame = caller_frame.older()
    caller = caller_frame.name().rsplit('::', 1)[-1]
    kind = frame.type()
    seen.append((name, value, caller, kind))
    if name in ['effects', 'mixed_effects']:
        assert kind == gdb.INLINE_FRAME, (caller, kind)
    elif name == 'checked':
        assert kind == gdb.NORMAL_FRAME, kind
        checked_seen.append((value, index))
    elif name == 'quotients':
        assert kind == gdb.NORMAL_FRAME, kind
        quotient_seen.append((value, divisor))
    else:
        assert kind == gdb.INLINE_FRAME, kind
    if kind == gdb.NORMAL_FRAME:
        for stop in stops:
            stop.enabled = False
        gdb.execute('finish', to_string=True)
        result = gdb.history(0)
        if name in ['effects', 'mixed_effects']:
            expected = (value, value + 1)
        elif name == 'checked':
            expected = (value + (3 if index else 0), value + (0 if index else 3))
        elif name == 'quotients':
            expected = (0 if divisor == 0 else int(value / divisor),
                        0 if divisor == 1 else int(10 / (divisor - 1)))
        else:
            raise AssertionError((name, kind))
        assert result.type.name == 'Pair', str(result.type)
        assert tuple(int(result[field]) for field in ['left', 'right']) == expected, (name, value, str(result))
        for stop in stops:
            stop.enabled = True
    else:
        assert kind == gdb.INLINE_FRAME, kind
        assert name in ['effects', 'mixed_effects', 'flags'], (name, kind)
        for stop in stops:
            stop.enabled = False
        for _ in range(300):
            frame = gdb.selected_frame()
            assert frame.name().rsplit('::', 1)[-1] == name, frame.name()
            try:
                result = frame.read_var('$return')
                retained = [field for field in ['left', 'right'] if not result[field].is_optimized_out]
                if len(retained) == (1 if caller == 'between' or name == 'flags' else 2):
                    field = retained[0]
                    expected = 0 if name == 'flags' else value + (field == 'right')
                    if int(result[field]) == expected and all(
                            int(result[field]) == value + (field == 'right') for field in retained
                            if name != 'flags'):
                        assert result.type.name == ('Flags' if name == 'flags' else 'Pair'), str(result.type)
                        break
            except (gdb.error, ValueError):
                pass
            gdb.execute('nexti', to_string=True)
        else:
            raise AssertionError('missing retained return fragment')
        if caller == 'between':
            # GDB finish may coalesce both leaf ranges of the same inline
            # source call. Observe each fragment at its actual boundary.
            for _ in range(100):
                gdb.execute('nexti', to_string=True)
                if gdb.selected_frame().name().rsplit('::', 1)[-1] != name:
                    break
            else:
                raise AssertionError('partial source frame did not exit')
        else:
            assert 'Value returned is' not in gdb.execute('finish', to_string=True)
        for stop in stops:
            stop.enabled = True
    gdb.execute('continue', to_string=True)
assert int(gdb.parse_and_eval('$_exitcode')) == 0
assert Counter(seen) == Counter([
    ('mixed_effects', 105, 'mixed', gdb.INLINE_FRAME),
    ('mixed_effects', 115, 'mixed', gdb.INLINE_FRAME),
    ('effects', 65, 'forward', gdb.INLINE_FRAME),
    ('effects', 75, 'forward', gdb.INLINE_FRAME),
    ('effects', 75, 'reverse', gdb.INLINE_FRAME),
    ('effects', 85, 'reverse', gdb.INLINE_FRAME),
    ('effects', 85, 'between', gdb.INLINE_FRAME),
    ('effects', 85, 'between', gdb.INLINE_FRAME),
    ('effects', 95, 'between', gdb.INLINE_FRAME),
    ('effects', 95, 'between', gdb.INLINE_FRAME),
    ('checked', 0, 'checked_total', gdb.NORMAL_FRAME),
    ('checked', 10, 'checked_total', gdb.NORMAL_FRAME),
    ('checked', 10, 'checked_total', gdb.NORMAL_FRAME),
    ('quotients', 0, 'quotient_total', gdb.NORMAL_FRAME),
    ('quotients', 0, 'quotient_total', gdb.NORMAL_FRAME),
    ('quotients', 10, 'quotient_total', gdb.NORMAL_FRAME),
    ('quotients', 10, 'quotient_total', gdb.NORMAL_FRAME),
    ('flags', 0, 'flags_total', gdb.INLINE_FRAME),
    ('flags', 10, 'flags_total', gdb.INLINE_FRAME),
]), seen
assert sorted(checked_seen) == [(0, 0), (10, 0), (10, 1)], checked_seen
assert sorted(quotient_seen) == [(0, 0), (0, 1), (10, 1), (10, 2)], quotient_seen"#;
    for hint in ["always", "never"] {
        let text = text.replace("HINT", hint);
        std::fs::write(&source, &text).unwrap();
        let debug = directory.join("debug.bin");
        let ordinary = directory.join("ordinary.bin");
        let mut outputs = Vec::new();
        for (binary, debugging) in [(&debug, true), (&ordinary, false)] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
            command.args(["--std", "std", "--test"]);
            if debugging {
                command.arg("--debug");
            }
            success(command.arg(&source).arg("-o").arg(binary).output().unwrap());
            outputs.push(success(
                Command::new(binary)
                    .arg("-o")
                    .arg(binary.with_extension("vcd"))
                    .output()
                    .unwrap(),
            ));
        }
        assert_eq!(
            outputs[0].stdout, outputs[1].stdout,
            "foreign-call order/count changed"
        );
        assert_eq!(outputs[0].stderr, outputs[1].stderr);
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
            success(
                Command::new("gdb")
                    .args([
                        "-q",
                        "-batch",
                        "-ex",
                        "set disable-randomization off",
                        "-ex",
                    ])
                    .arg(format!("python exec({python:?})"))
                    .arg(&debug)
                    .output()
                    .unwrap(),
            );
        }
        // Both invalid field checks are demanded. The first diagnostic must
        // remain the left-field access, not the earlier-written right field.
        let failing = text
            .replace("let index: integer = 0;", "let index: integer = 999;")
            .replace("let enabled: Bool = false;", "let enabled: Bool = true;");
        std::fs::write(&source, failing).unwrap();
        let mut failures = Vec::new();
        for (binary, debugging) in [(&debug, true), (&ordinary, false)] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
            command.args(["--std", "std", "--test"]);
            if debugging {
                command.arg("--debug");
            }
            success(command.arg(&source).arg("-o").arg(binary).output().unwrap());
            let output = Command::new(binary).output().unwrap();
            assert!(!output.status.success(), "invalid indices must fail");
            let diagnostic = String::from_utf8_lossy(&output.stdout);
            assert!(
                diagnostic.contains("index 999 is outside declared range 0..1"),
                "{diagnostic}"
            );
            assert!(diagnostic.contains(".left = value[index]"), "{diagnostic}");
            failures.push(output);
        }
        assert_eq!(failures[0].status.code(), failures[1].status.code());
        assert_eq!(failures[0].stdout, failures[1].stdout);
        assert_eq!(failures[0].stderr, failures[1].stderr);
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn narrow_source_consumers_do_not_fabricate_binding_bits() {
    let directory =
        std::env::temp_dir().join(format!("siox_dwarf_narrow_binding_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("narrow.siox");
    let text = r#"module dwarf_narrow_binding;
use std::bits::unsigned;
#[inline(HINT)]
fn low(value: unsigned[81]) -> unsigned[8] {
    let saved: unsigned[81] = value;
    return unsigned[8](saved);
}
#[inline(HINT)]
fn both(value: unsigned[81]) -> unsigned[81] {
    let saved: unsigned[81] = value;
    let byte: unsigned[8] = unsigned[8](saved);
    return unsigned[81](byte) + saved;
}
entity Probe { value: unsigned[81] in, byte: unsigned[8] out, whole: unsigned[81] out }
impl Probe { byte = low(value + 3); whole = both(value + 3); }
#[test] entity T {}
impl T {
    let value: unsigned[81] = 1208925819614629174706181;
    let probe: Probe = { .value = value };
    stimulus: process {
        await 1ns;
        assert!(probe.byte == 8 and probe.whole == 1208925819614629174706192, "first consumer formats");
        value = 1208925819614629174706190;
        await 1ns;
        assert!(probe.byte == 17 and probe.whole == 1208925819614629174706210, "new consumer epoch");
    }
}"#;
    let python = r#"import gdb
from collections import Counter
gdb.execute('set disable-randomization off')
stop = gdb.Breakpoint('dwarf_narrow_binding::low')
gdb.execute('run')
seen = []
while gdb.selected_thread() is not None:
    frame = gdb.selected_frame()
    assert frame.name() == 'dwarf_narrow_binding::low', frame.name()
    stop.enabled = False
    for name in ['value', 'saved']:
        try:
            binding = frame.read_var(name)
            assert binding.is_optimized_out or binding.is_unavailable, (name, str(binding))
        except (gdb.error, ValueError):
            pass
    gdb.execute('finish', to_string=True)
    result = gdb.history(0)
    seen.append(int(result))
    stop.enabled = True
    gdb.execute('continue', to_string=True)
# Both retained hardware reads must report the actual low byte in each epoch.
assert Counter(seen) == Counter([8, 8, 17, 17]), seen
assert int(gdb.parse_and_eval('$_exitcode')) == 0
"#;
    let complete = r#"import gdb
from collections import Counter
gdb.execute('set disable-randomization off')
stop = gdb.Breakpoint('dwarf_narrow_binding::both')
gdb.execute('run')
seen = []
while gdb.selected_thread() is not None:
    frame = gdb.selected_frame()
    assert frame.name() == 'dwarf_narrow_binding::both', frame.name()
    stop.enabled = False
    expected = int(frame.read_var('value'))
    assert expected in [(1 << 80) + 8, (1 << 80) + 17], expected
    for name in ['value', 'saved']:
        binding = frame.read_var(name)
        assert binding.type.sizeof == 11 and int(binding) == expected, (name, str(binding))
    seen.append(expected)
    assert frame.type() == gdb.INLINE_FRAME, frame.type()
    for _ in range(300):
        frame = gdb.selected_frame()
        while frame is not None and frame.name() != 'dwarf_narrow_binding::both':
            frame = frame.older()
        assert frame is not None, 'wide source frame disappeared before completing'
        try:
            returned = frame.read_var('$return')
            if not returned.is_optimized_out and int(returned) == expected + (expected & 255): break
        except (gdb.error, ValueError):
            pass
        gdb.execute('stepi', to_string=True)
    else:
        raise AssertionError('complete hardware wide return unavailable')
    assert returned.type.sizeof == 11, returned.type
    frame.select()
    assert 'Value returned is' not in gdb.execute('finish', to_string=True)
    stop.enabled = True
    gdb.execute('continue', to_string=True)
assert Counter(seen) == Counter([(1 << 80) + 8, (1 << 80) + 17]), seen
assert int(gdb.parse_and_eval('$_exitcode')) == 0
"#;
    for hint in ["always", "never"] {
        std::fs::write(&source, text.replace("HINT", hint)).unwrap();
        let mut outputs = Vec::new();
        for debugging in [false, true] {
            let binary = directory.join(if debugging {
                "debug.bin"
            } else {
                "ordinary.bin"
            });
            let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
            command.args(["--std", "std", "--test"]);
            if debugging {
                command.arg("--debug");
            }
            success(
                command
                    .arg(&source)
                    .arg("-o")
                    .arg(&binary)
                    .output()
                    .unwrap(),
            );
            outputs.push(success(
                Command::new(&binary)
                    .arg("-o")
                    .arg(binary.with_extension("vcd"))
                    .output()
                    .unwrap(),
            ));
        }
        assert_eq!(outputs[0].stdout, outputs[1].stdout);
        assert_eq!(outputs[0].stderr, outputs[1].stderr);
        assert_eq!(
            std::fs::read(directory.join("ordinary.vcd")).unwrap(),
            std::fs::read(directory.join("debug.vcd")).unwrap()
        );
        let binary = directory.join("debug.bin");
        if Command::new("llvm-dwarfdump")
            .arg("--version")
            .output()
            .is_ok()
        {
            success(
                Command::new("llvm-dwarfdump")
                    .arg("--verify")
                    .arg(&binary)
                    .output()
                    .unwrap(),
            );
        }
        if Command::new("gdb").arg("--version").output().is_ok() {
            success(
                Command::new("gdb")
                    .args(["-q", "-batch", "-ex"])
                    .arg(format!("python exec({python:?})"))
                    .arg(&binary)
                    .output()
                    .unwrap(),
            );
            success(
                Command::new("gdb")
                    .args(["-q", "-batch", "-ex"])
                    .arg(format!("python exec({complete:?})"))
                    .arg(&binary)
                    .output()
                    .unwrap(),
            );
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn effectful_source_frames_share_actual_cached_results() {
    let directory =
        std::env::temp_dir().join(format!("siox_dwarf_effect_reuse_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("reuse.siox");
    let text = r#"module dwarf_effect_reuse;
use std::bits::unsigned;
extern "C" { fn putchar(value: integer) -> integer; }
struct Pair { pub left: integer, pub right: integer }
#[inline(HINT)]
fn effects(value: integer) -> Pair {
    return Pair { .left = putchar(value), .right = putchar(value + 1) };
}
#[inline(HINT)]
fn consume(value: integer) -> integer {
    let pair: Pair = effects(value);
    return integer(unsigned[8](pair.left)) + putchar(value + 2) + pair.left + pair.right;
}
#[inline(HINT)]
fn character(value: integer) -> integer { return putchar(value); }
#[inline(HINT)]
fn formats(value: integer) -> integer {
    let saved: integer = character(value);
    return integer(unsigned[8](saved)) + putchar(value + 2) + saved;
}
#[inline(HINT)]
fn random_value() -> integer { return randint(0, 100); }
#[inline(HINT)]
fn random_formats() -> integer {
    let saved: integer = random_value();
    return integer(unsigned[8](saved)) + saved;
}
entity E { value: integer in, enabled: Bool in, total: integer out, formatted: integer out }
impl E {
    total = if enabled { consume(value) } else { 0 };
    formatted = if enabled { formats(value + 20) } else { 0 };
}
#[test] entity T {}
impl T {
    let value: integer = 65;
    let enabled: Bool = false;
    let e: E = { .value = value, .enabled = enabled };
    stimulus: process {
        await 1ns;
        assert!(e.total == 0 and e.formatted == 0, "untaken effects");
        enabled = true; await 1ns;
        assert!(e.total == 263 and e.formatted == 257, "reused foreign results");
        value = 75; await 1ns;
        assert!(e.total == 303 and e.formatted == 287, "new source epoch");
        for index in 0..3 {
            let result: integer = formats(110 + index);
            assert!(result == 332 + 3 * index, "repeated source entry");
        }
        seed(42);
        let random: integer = random_formats();
        let next: integer = randint(0, 100);
        seed(42);
        let first: integer = randint(0, 100);
        let second: integer = randint(0, 100);
        assert!(random == first + first and next == second, "one host effect across consumer formats");
        print!("random {} {}", random, next);
    }
}"#;
    let python = r#"import gdb
from collections import Counter

gdb.execute('set disable-randomization off')
stop = gdb.Breakpoint('dwarf_effect_reuse::formats')
gdb.execute('run')
seen = []
while gdb.selected_thread() is not None:
    stop.enabled = False
    frame = gdb.selected_frame()
    assert frame.name() == 'dwarf_effect_reuse::formats', frame.name()
    if frame.type() == gdb.NORMAL_FRAME:
        # Effects already executed in the caller CFG. This source frame owns
        # only the remaining pure tail, not a full native function signature.
        assert frame.older().name() == 'T::stimulus', frame.older().name()
        value = 110 + int(frame.older().read_var('index'))
        def unavailable(name):
            try:
                binding = gdb.selected_frame().read_var(name)
                return binding.is_optimized_out or binding.is_unavailable
            except (gdb.error, ValueError):
                return True
        assert unavailable('value') and unavailable('saved')
        for _ in range(100):
            frame = gdb.selected_frame()
            assert frame.name() == 'dwarf_effect_reuse::formats', frame.name()
            assert unavailable('value'), 'pruned formal must not become a made-up zero'
            try:
                saved = frame.read_var('saved')
                if not saved.is_optimized_out and int(saved) == value:
                    assert str(saved.type) == 'integer', str(saved.type)
                    break
            except (gdb.error, ValueError):
                pass
            gdb.execute('nexti', to_string=True)
        else:
            raise AssertionError('consumed normalized local unavailable')
        gdb.execute('finish', to_string=True)
        result = gdb.history(0)
        assert str(result.type) == 'integer' and int(result) == 3 * value + 2, str(result)
        seen.append(value)
        stop.enabled = True
        gdb.execute('continue', to_string=True)
        continue
    assert frame.type() == gdb.INLINE_FRAME, frame.type()
    # No previous invocation's snapshot may be exposed before this return.
    try:
        result = frame.read_var('$return')
        assert result.is_optimized_out, str(result)
    except (gdb.error, ValueError):
        pass
    for _ in range(500):
        frame = gdb.selected_frame()
        while frame is not None and frame.name() != 'dwarf_effect_reuse::formats':
            frame = frame.older()
        if frame is None:
            # Lazy argument expressions retain their caller's source location;
            # the inline callee can have discontiguous machine ranges.
            gdb.execute('nexti', to_string=True)
            continue
        try:
            value = int(frame.read_var('value'))
            result = frame.read_var('$return')
            if not result.is_optimized_out and int(result) == 3 * value + 2:
                assert str(result.type) == 'integer', str(result.type)
                seen.append(value)
                break
        except (gdb.error, ValueError):
            pass
        gdb.execute('nexti', to_string=True)
    else:
        raise AssertionError('consumed scalar return unavailable')
    frame.select()
    assert 'Value returned is' not in gdb.execute('finish', to_string=True)
    stop.enabled = True
    gdb.execute('continue', to_string=True)
assert int(gdb.parse_and_eval('$_exitcode')) == 0
assert Counter(seen) == Counter([85, 95, 110, 111, 112, 113]), seen"#;
    let fragments = r#"import gdb
from collections import Counter

gdb.execute('set disable-randomization off')
stop = gdb.Breakpoint('dwarf_effect_reuse::effects')
gdb.execute('run')
seen = []
while gdb.selected_thread() is not None:
    stop.enabled = False
    assert gdb.selected_frame().type() == gdb.INLINE_FRAME
    for _ in range(300):
        frame = gdb.selected_frame()
        while frame is not None and frame.name() != 'dwarf_effect_reuse::effects':
            frame = frame.older()
        if frame is not None:
            try:
                result = frame.read_var('$return')
                fields = [field for field in ['left', 'right'] if not result[field].is_optimized_out]
                if len(fields) == 1:
                    field = fields[0]
                    value = int(result[field])
                    if value in ([65, 75] if field == 'left' else [66, 76]):
                        assert result.type.name == 'Pair', str(result.type)
                        seen.append((field, value))
                        break
            except (gdb.error, ValueError):
                pass
        gdb.execute('nexti', to_string=True)
    else:
        raise AssertionError('cached foreign return fragment unavailable')
    for _ in range(100):
        gdb.execute('nexti', to_string=True)
        if gdb.selected_frame().name() != 'dwarf_effect_reuse::effects':
            break
    else:
        raise AssertionError('partial frame did not exit')
    stop.enabled = True
    gdb.execute('continue', to_string=True)
assert int(gdb.parse_and_eval('$_exitcode')) == 0
assert Counter(seen) == Counter([('left', 65), ('left', 65), ('right', 66),
                                ('left', 75), ('left', 75), ('right', 76)]), seen"#;
    for hint in ["always", "never"] {
        std::fs::write(&source, text.replace("HINT", hint)).unwrap();
        let mut outputs = Vec::new();
        for debugging in [false, true] {
            let binary = directory.join(if debugging {
                "debug.bin"
            } else {
                "ordinary.bin"
            });
            let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
            command.args(["--std", "std", "--test"]);
            if debugging {
                command.arg("--debug");
            }
            success(
                command
                    .arg(&source)
                    .arg("-o")
                    .arg(&binary)
                    .output()
                    .unwrap(),
            );
            outputs.push(success(
                Command::new(&binary)
                    .arg("-o")
                    .arg(binary.with_extension("vcd"))
                    .output()
                    .unwrap(),
            ));
        }
        assert_eq!(
            outputs[0].stdout, outputs[1].stdout,
            "effect order/count changed"
        );
        assert_eq!(outputs[0].stderr, outputs[1].stderr);
        assert_eq!(
            std::fs::read(directory.join("ordinary.vcd")).unwrap(),
            std::fs::read(directory.join("debug.vcd")).unwrap()
        );
        let binary = directory.join("debug.bin");
        if Command::new("llvm-dwarfdump")
            .arg("--version")
            .output()
            .is_ok()
        {
            success(
                Command::new("llvm-dwarfdump")
                    .arg("--verify")
                    .arg(&binary)
                    .output()
                    .unwrap(),
            );
        }
        if Command::new("gdb").arg("--version").output().is_ok() {
            for python in [python, fragments] {
                success(
                    Command::new("gdb")
                        .args(["-q", "-batch", "-ex"])
                        .arg(format!("python exec({python:?})"))
                        .arg(&binary)
                        .output()
                        .unwrap(),
                );
            }
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn partial_aggregate_returns_keep_only_demanded_fragments() {
    let directory = std::env::temp_dir().join(format!("siox_dwarf_partial_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("partial.siox");
    let text = r#"module dwarf_partial;
extern "C" { fn abort() -> integer; }
struct Pair { pub left: integer, pub right: RIGHT_TYPE }
#[inline(CALLEE)]
fn partial(value: integer) -> Pair {
    return Pair { .left = value + 1, .right = RIGHT_ABORT };
}
#[inline(CALLER)]
fn consume(value: integer) -> integer {
    let pair: Pair = partial(value);
    return pair.left;
}
#[inline(CALLEE)]
fn partial_right(value: integer) -> Pair {
    return Pair { .left = abort(), .right = RIGHT_VALUE };
}
#[inline(CALLER)]
fn consume_right(value: integer) -> RIGHT_TYPE {
    let pair: Pair = partial_right(value);
    return pair.right;
}
entity Probe { value: integer in, result: integer out, other: RIGHT_TYPE out }
impl Probe {
    result = consume(value);
    other = consume_right(value);
}
#[test] entity T {}
impl T {
    let value: integer = 5;
    let probe: Probe = { .value = value };
    stimulus: process {
        await 1ns;
        assert!(probe.result == 6 and probe.other == RIGHT_POSITIVE, "positive retained fields");
        value = -9;
        await 1ns;
        assert!(probe.result == -8 and probe.other == RIGHT_NEGATIVE, "negative retained fields");
    }
}
"#;
    let python = r#"import gdb
stops = [gdb.Breakpoint('dwarf_partial::' + name) for name in ['partial', 'partial_right']]
gdb.execute('run')
seen = []
while gdb.selected_thread() is not None:
    frame = gdb.selected_frame()
    assert frame.type() == gdb.INLINE_FRAME, gdb.execute('info frame', to_string=True)
    name = frame.name()
    right = name.endswith('partial_right')
    retained, pruned = ('right', 'left') if right else ('left', 'right')
    value = int(frame.read_var('value'))
    floating = right and frame.function().type.target().fields()[1].type.code == gdb.TYPE_CODE_FLT
    number = float if floating else int
    expected = value + (2.5 if floating else 2 if right else 1)
    assert frame.function().type.target().name == 'Pair', frame.function().type
    # Stop after the ordinary result has executed. Inspection must not force
    # the abort field, or claim the retained value exists before evaluation.
    for _ in range(100):
        frame = gdb.selected_frame()
        assert frame.name() == name, frame.name()
        try:
            result = frame.read_var('$return')
            if not result[retained].is_optimized_out and number(result[retained]) == expected:
                break
        except (gdb.error, gdb.MemoryError, ValueError):
            pass
        gdb.execute('stepi', to_string=True)
    else:
        raise AssertionError('retained return fragment never became available')
    assert result[pruned].is_optimized_out, str(result)
    finished = gdb.execute('finish', to_string=True)
    # This is a source inline frame over a partial native evaluation, not a
    # whole-result ABI. GDB must not invent a return value from stale registers.
    assert 'Value returned is' not in finished, finished
    frame = gdb.selected_frame()
    assert frame.name() == 'dwarf_partial::' + ('consume_right' if right else 'consume'), frame.name()
    pair = frame.read_var('pair')
    assert number(pair[retained]) == expected, str(pair)
    assert pair[pruned].is_optimized_out, str(pair)
    seen.append((retained, value))
    gdb.execute('continue')
assert int(gdb.parse_and_eval('$_exitcode')) == 0
assert sorted(seen) == [('left', -9), ('left', 5), ('right', -9), ('right', 5)], seen
"#;
    for (ty, value, abort, positive, negative) in [
        ("integer", "value + 2", "abort()", "7", "-7"),
        ("real", "real(value) + 2.5", "real(abort())", "7.5", "-6.5"),
    ] {
        let text = text
            .replace("RIGHT_TYPE", ty)
            .replace("RIGHT_VALUE", value)
            .replace("RIGHT_ABORT", abort)
            .replace("RIGHT_POSITIVE", positive)
            .replace("RIGHT_NEGATIVE", negative);
        for callee in ["always", "never"] {
            for caller in ["always", "never"] {
                std::fs::write(
                    &source,
                    text.replace("CALLEE", callee).replace("CALLER", caller),
                )
                .unwrap();
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
                    success(
                        Command::new("gdb")
                            .args(["-nx", "-q", "--batch"])
                            .arg(&debug)
                            .args([
                                "-ex",
                                "set disable-randomization off",
                                "-ex",
                                &format!("python {python}"),
                            ])
                            .output()
                            .unwrap(),
                    );
                } else {
                    eprintln!(
                        "GDB unavailable: native partial returns checked, frame probe skipped"
                    );
                }
            }
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
use std::fixed::ufixed;
struct Nested { pub flags: unsigned[3], pub negative: signed[81] }
struct Wrapper { pub word: integer }
struct Pair { pub left: integer, pub right: integer }
fn make_pair(value: integer) -> Pair {
    let base: integer = value + 1;
    return Pair { .left = base, .right = base + 1 }; // hardware pair return
}
fn sum_pair(value: integer) -> integer {
    let pair: Pair = make_pair(value); // hardware pair assignment
    return pair.left + pair.right;
}
entity Paired { value: integer in, total: integer out }
impl Paired { total = sum_pair(value); }
fn choose_pair(value: integer) -> Pair {
    let base: integer = value + 1;
    if value > 0 {
        let local: integer = base + 1;
        if value == 6 {
            let local: integer = base + 2;
            return Pair { .left = local, .right = local + 1 }; // nested pair return
        }
        return Pair { .left = local, .right = local + 2 }; // positive pair return
    }
    return Pair { .left = base, .right = base + 10 }; // fallback pair return
}
fn sum_chosen_pair(value: integer) -> integer {
    let pair: Pair = choose_pair(value); // chosen pair assignment
    return pair.left + pair.right;
}
entity Chosen { value: integer in, total: integer out }
impl Chosen { total = sum_chosen_pair(value); }
fn independent_pair(value: integer) -> Pair {
    return Pair { .left = if value > 0 { value } else { value - 1 }, .right = if value == 6 { value + 3 } else { value + 1 } }; // independent pair return
}
fn sum_independent_pair(value: integer) -> integer {
    let pair: Pair = independent_pair(value); // independent pair assignment
    return pair.left + pair.right;
}
entity Independent { value: integer in, total: integer out }
impl Independent { total = sum_independent_pair(value); }
fn alias_pair(value: integer) -> Pair {
    let pair: Pair = { .left = value, .right = value + 1 };
    if value > 0 {
        return pair; // positive alias pair return
    }
    return Pair { .left = pair.left, .right = pair.right + 10 }; // fallback alias pair return
}
fn sum_alias_pair(value: integer) -> integer {
    let pair: Pair = alias_pair(value); // alias pair assignment
    return pair.left + pair.right;
}
entity Aliased { value: integer in, total: integer out }
impl Aliased { total = sum_alias_pair(value); }
fn match_pair(value: integer) -> Pair {
    let local: integer = value + 10;
    match value {
        6 => {
            let local: integer = value + 20;
            return Pair { .left = local, .right = local + 1 }; // matched pair return
        }
        _ => {
            return Pair { .left = local, .right = local + 1 }; // default pair return
        }
    }
}
fn sum_matched_pair(value: integer) -> integer {
    let pair: Pair = match_pair(value); // matched pair assignment
    return pair.left + pair.right;
}
entity Matched { value: integer in, total: integer out }
impl Matched { total = sum_matched_pair(value); }
fn make_masked_pair(value: integer) -> Pair {
    let base: integer = value + 1;
    return Pair { .left = base, .right = base + 1 }; // masked pair return
}
fn sum_masked_pair(value: integer) -> integer {
    let pair: Pair = make_masked_pair(value); // masked pair assignment
    return (if value == 5 or value == 6 { pair.left } else { 0 })
        + (if value == 5 or value == -6 { pair.right } else { 0 });
}
entity Masked { value: integer in, total: integer out }
impl Masked { total = sum_masked_pair(value); }
extern "C" { fn abort() -> integer; }
fn risky_pair(value: integer) -> Pair {
    return Pair { .left = value + 1, .right = abort() };
}
fn consume_risky_pair(value: integer, enabled: Bool) -> integer {
    let pair: Pair = risky_pair(value);
    return if enabled { pair.right } else { pair.left };
}
entity Risky { value: integer in, enabled: Bool in, total: integer out }
impl Risky { total = consume_risky_pair(value, enabled); }
struct Bundle {
    pub tag: integer, pub gain: real, pub wide: unsigned[81], pub unused: integer,
    pub nested: Nested, pub lanes: integer[2..1], pub samples: integer[-1..0]
}
fn inspect_bundle(value: Bundle, wrapped: Wrapper) -> integer {
    let copy: Bundle = value;
    return copy.tag + integer(copy.gain)
        + if copy.wide == 18446744073709551617 { 0 } else { 1000 }
        + if copy.nested.flags == 5 and copy.nested.negative == -2
            and copy.lanes[2] == 7 and copy.lanes[1] == 9
            and copy.samples[-1] == 41 and copy.samples[0] == 43 { 0 } else { 2000 }
        + if wrapped.word == copy.tag { 0 } else { 4000 }; // hardware aggregate return
}
entity Bundled { value: Bundle in, result: integer out }
impl Bundled { result = inspect_bundle(value, Wrapper { .word = value.tag }); } // hardware aggregate assignment
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
struct Count { pub value: integer }
impl From<integer> for Count {
    fn from(value: integer) -> Self {
        let incremented: integer = value + 1;
        return Count { .value = incremented }; // conversion formal stop
    }
}
struct Ticks(integer);
impl Suffix<"ticks", integer> for Ticks {
    fn suffix(value: integer) -> Ticks {
        let scaled: integer = value * 3;
        return Ticks(scaled); // integer suffix formal stop
    }
}
struct Scale(real);
impl Suffix<"scale", real> for Scale {
    fn suffix(value: real) -> Scale {
        let scaled: real = value * 2.0;
        return Scale(scaled); // real suffix formal stop
    }
}
fn bounded(value: integer<-8..7>) -> integer {
    let doubled: integer = value + value;
    return doubled; // constrained formal stop
}
fn generic<T>(value: T) -> T {
    let copy: T = value;
    return copy; // hardware generic formal stop
}
fn formal_result(value: integer<-8..7>) -> integer {
    let converted: Count = Count(value);
    return converted.value + bounded(generic(value)) + integer(2ticks) + integer(real(1.5scale));
}
struct Word(Logic[]);
impl Add<Word, Word> for Word {
    fn add(self, rhs: Self) -> Self {
        let sum: integer = self + rhs;
        return sum; // packed Self formal stop
    }
}
entity Formals { value: integer<-8..7> in, fixed: ufixed<8, 4> in, result: integer out, packed: integer out, resized: integer out }
impl Formals {
    result = formal_result(value);
    packed = integer(Word[5](value) + 1);
    resized = integer(ufixed<7, 2>(fixed));
}
enum Level { Low, High }
impl Resolve for Level {
    fn resolve(self, rhs: Self) -> Self {
        let first: Level = self;
        let second: Level = rhs;
        return if first == Level::High or second == Level::High { Level::High } else { Level::Low }; // resolve formal stop
    }
}
entity Resolved { first: Level in, second: Level in, result: Level out }
impl Resolved { result = first; }
impl Resolved { result = second; }
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
    let paired: Paired = { .value = input };
    let chosen: Chosen = { .value = input };
    let independent: Independent = { .value = input };
    let aliased: Aliased = { .value = input };
    let matched: Matched = { .value = input };
    let disabled: Bool = false;
    let masked: Masked = { .value = input };
    let risky: Risky = { .value = input, .enabled = disabled };
    let typed: Typed = { .value = 18446744073709551617, .gain = 1.5, .negative = -2 };
    let bundle: Bundle = {
        .tag = 5, .gain = 1.5, .wide = 18446744073709551617, .unused = 101,
        .nested = { .flags = 5, .negative = -2 }, .lanes = [7, 9], .samples = [41, 43]
    };
    let bundled: Bundled = { .value = bundle };
    let small: integer<-8..7> = -3;
    let narrow: Narrow = { .value = small };
    let fixed: ufixed<8, 4> = ufixed<8, 4>(1.5);
    let formals: Formals = { .value = small, .fixed = fixed };
    let first: Level = Level::Low;
    let second: Level = Level::High;
    let resolved: Resolved = { .first = first, .second = second };
    let clock: Bit = '0';
    let reg: Register = { .clk = clock, .value = input };
    stimulus: process {
        await 1ns;
        assert!(dut.result == 107 and dut.selected == 7, "first hardware result");
        assert!(typed.result == 1 and other.result == 112 and other.selected == 12, "typed and shared helper results");
        assert!(bundled.result == 6, "hardware aggregate input and local result");
        assert!(paired.total == 13, "whole aggregate return result");
        assert!(chosen.total == 16, "positive aggregate return branch");
        assert!(independent.total == 11 and aliased.total == 11, "independent and aliased positive fields");
        assert!(matched.total == 31, "default positive match return");
        assert!(masked.total == 13 and risky.total == 6, "selected aggregate consumers and untaken foreign leaf");
        assert!(narrow.result == -24, "nested arguments retain consumer widths and signs");
        assert!(formals.result == 1, "negative declaration-owned formal values");
        assert!(formals.packed == 30, "negative input to packed Self formal");
        assert!(formals.resized == 6, "fixed conversion retains the target format");
        assert!(resolved.result == Level::High, "second Resolve input drives High");
        input = 6;
        small = 3;
        first = Level::High;
        second = Level::Low;
        fixed = ufixed<8, 4>(2.5);
        clock = '1';
        await 1ns;
        assert!(dut.result == 108 and dut.selected == 8, "updated hardware result");
        assert!(paired.total == 15, "updated whole aggregate return result");
        assert!(chosen.total == 19, "nested aggregate return branch");
        assert!(independent.total == 15 and aliased.total == 13, "independent condition and aliased positive fields");
        assert!(matched.total == 53, "selected match return");
        assert!(masked.total == 7 and risky.total == 7, "first-only aggregate consumer");
        assert!(narrow.result == 24, "nested formats reevaluate after input changes");
        assert!(formals.result == 19, "positive declaration-owned formal values");
        assert!(formals.packed == 4, "positive input to packed Self formal");
        assert!(formals.resized == 10, "fixed conversion follows the source value");
        assert!(resolved.result == Level::High, "first Resolve input drives High");
        assert!(reg.result == 16, "event call result");
        second = Level::High;
        await 1ns;
        assert!(resolved.result == Level::High, "both Resolve inputs drive High");
        input = -6;
        clock = '0';
        await 1ns;
        assert!(dut.result == 96 and dut.selected == 15, "fallback hardware result");
        assert!(paired.total == -9, "negative whole aggregate return result");
        assert!(chosen.total == 0, "fallback aggregate return branch");
        assert!(independent.total == -12 and aliased.total == -1, "independent and aliased negative fields");
        assert!(matched.total == 9, "default negative match return");
        assert!(masked.total == -4 and risky.total == -5, "second-only aggregate consumer");
        clock = '1';
        await 1ns;
        assert!(reg.result == 4, "updated event call result");
        input = 0;
        first = Level::Low;
        second = Level::Low;
        await 1ns;
        assert!(masked.total == 0, "untaken aggregate call");
        assert!(resolved.result == Level::Low, "both Resolve inputs drive Low");
        assert!(chosen.total == 12, "zero aggregate return branch");
        assert!(independent.total == 0 and aliased.total == 11, "independent and aliased zero fields");
        assert!(matched.total == 21, "default zero match return");
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
bundle_seen = False
pair_calls = []
masked_calls = []
chosen_calls = []
independent_calls = []
alias_calls = []
match_calls = []
failures = []
class Check(gdb.Breakpoint):
    def stop(self):
        try:
            self.check()
        except Exception as error:
            failures.append((type(self).__name__, str(error), gdb.selected_frame().name(), gdb.selected_frame().find_sal().line))
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
formal_cases = {{
    {conversion_formal}: ('integer', 8, 'incremented', lambda value: value + 1, {{-3, 3}}),
    {integer_suffix_formal}: ('integer', 8, 'scaled', lambda value: value * 3, {{2}}),
    {real_suffix_formal}: ('real', 8, 'scaled', lambda value: value * 2, {{1.5}}),
    {constrained_formal}: ('integer', 1, 'doubled', lambda value: value * 2, {{-3, 3}}),
    {generic_formal}: ('integer', 1, 'copy', lambda value: value, {{-3, 3}}),
}}
formal_seen = {{line: set() for line in formal_cases}}
class Formal(Check):
    def check(self):
        frame = gdb.selected_frame()
        line = frame.find_sal().line
        name, size, local, operation, expected = formal_cases[line]
        value = frame.read_var('value')
        assert value.type.name == name and value.type.sizeof == size, (line, str(value.type), value.type.sizeof)
        cast = float if name == 'real' else int
        actual = cast(value)
        assert cast(frame.read_var(local)) == operation(actual), (line, actual, str(frame.read_var(local)))
        formal_seen[line].add(actual)
for line in formal_cases:
    Formal({source:?} + ':' + str(line))
packed_formals = set()
class PackedFormal(Check):
    def check(self):
        frame = gdb.selected_frame()
        lhs, rhs = [frame.read_var(name) for name in ['self', 'rhs']]
        assert all(value.type.name.replace(' ', '') == 'Word[5]' and value.type.sizeof == 1 and value.type.code == gdb.TYPE_CODE_INT for value in [lhs, rhs]), (str(lhs.type), str(rhs.type))
        assert int(rhs) == 1, rhs
        assert int(frame.read_var('sum')) == int(lhs) + 1
        packed_formals.add(int(lhs))
PackedFormal({source:?} + ':{packed_formal}')
resolve_calls = set()
class ResolveFormal(Check):
    def check(self):
        frame = gdb.selected_frame()
        lhs, rhs = [frame.read_var(name) for name in ['self', 'rhs']]
        assert all(value.type.name == 'Level' and value.type.code == gdb.TYPE_CODE_ENUM for value in [lhs, rhs]), (str(lhs.type), str(rhs.type))
        assert frame.function().type.target().name == 'Level', str(frame.function().type)
        assert int(frame.read_var('first')) == int(lhs)
        assert int(frame.read_var('second')) == int(rhs)
        resolve_calls.add((int(lhs), int(rhs)))
ResolveFormal({source:?} + ':{resolve_formal}')
fixed_types = set()
fixed_inputs = set()
class FixedFormal(Check):
    def check(self):
        frame = gdb.selected_frame()
        argument = frame.read_var('value').type
        result = frame.function().type.target()
        fixed_types.add((argument.name.replace(' ', ''), result.name.replace(' ', '')))
        if argument.name.replace(' ', '') == 'ufixed[8]':
            fixed_inputs.add(int(frame.read_var('value')))
FixedFormal('std::fixed::from')
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
class Aggregate(Check):
    def check(self):
        global bundle_seen
        frame = gdb.selected_frame()
        assert frame.name() == 'debug_hardware::inspect_bundle', frame.name()
        assert frame.older().find_sal().line == {bundle_assignment}
        assert frame.older().name().removesuffix('(void)') == 'T::bundled::<hardware:T.bundled.result>', frame.older().name()
        tag = int(gdb.parse_and_eval('value.tag'))
        gain = float(gdb.parse_and_eval('value.gain'))
        wide = int(gdb.parse_and_eval('value.wide'))
        assert tag in [0, 5] and gain in [0, 1.5] and wide in [0, (1 << 64) + 1], (tag, gain, wide)
        assert int(gdb.parse_and_eval('copy.tag')) == tag
        assert int(gdb.parse_and_eval('wrapped.word')) == tag
        assert int(gdb.parse_and_eval('sizeof(wrapped)')) == 8
        assert float(gdb.parse_and_eval('copy.gain')) == gain
        assert int(gdb.parse_and_eval('copy.wide')) == wide
        assert int(gdb.parse_and_eval('sizeof(value.wide)')) == 11
        assert int(gdb.parse_and_eval('sizeof(value)')) == 79
        assert gdb.parse_and_eval('value.unused').is_optimized_out
        assert gdb.parse_and_eval('copy.unused').is_optimized_out
        flags = int(gdb.parse_and_eval('value.nested.flags'))
        negative = int(gdb.parse_and_eval('value.nested.negative'))
        assert flags in [0, 5] and negative in [0, -2], (flags, negative)
        assert int(gdb.parse_and_eval('copy.nested.flags')) == flags
        assert int(gdb.parse_and_eval('copy.nested.negative')) == negative
        lanes = gdb.parse_and_eval('value.lanes')
        local_lanes = gdb.parse_and_eval('copy.lanes')
        samples = gdb.parse_and_eval('value.samples')
        local_samples = gdb.parse_and_eval('copy.samples')
        observed = [int(lanes['[2]']), int(lanes['[1]']), int(samples[-1]), int(samples[0])]
        assert all(actual in [0, expected] for actual, expected in zip(observed, [7, 9, 41, 43])), observed
        assert [int(local_lanes['[2]']), int(local_lanes['[1]']), int(local_samples[-1]), int(local_samples[0])] == observed
        bundle_seen |= tag == 5 and gain == 1.5 and wide == (1 << 64) + 1 and flags == 5 and negative == -2 and observed == [7, 9, 41, 43]
        return False
class PairCall(Check):
    def check(self):
        frame = gdb.selected_frame()
        assert frame.name() == 'debug_hardware::make_pair', frame.name()
        assert frame.older().find_sal().line == {pair_assignment}
        value = int(gdb.parse_and_eval('value'))
        assert int(gdb.parse_and_eval('base')) == value + 1
        pair_calls.append(value)
        return False
class MaskedPair(Check):
    def check(self):
        frame = gdb.selected_frame()
        assert frame.name() == 'debug_hardware::make_masked_pair', frame.name()
        assert frame.older().find_sal().line == {masked_assignment}
        value = int(gdb.parse_and_eval('value'))
        assert int(gdb.parse_and_eval('base')) == value + 1
        masked_calls.append(value)
        return False
class ChosenPair(Check):
    def check(self):
        frame = gdb.selected_frame()
        assert frame.name() == 'debug_hardware::choose_pair', frame.name()
        assert frame.older().find_sal().line == {chosen_assignment}
        value = int(gdb.parse_and_eval('value'))
        expected_line = {nested_pair_return} if value == 6 else {positive_pair_return} if value > 0 else {fallback_pair_return}
        assert frame.find_sal().line == expected_line, (value, frame.find_sal().line, expected_line)
        assert int(gdb.parse_and_eval('base')) == value + 1
        if value > 0:
            assert int(gdb.parse_and_eval('local')) == value + (3 if value == 6 else 2)
        else:
            symbol, _ = gdb.lookup_symbol('local', frame.block())
            assert symbol is None, 'branch local escaped its scope'
        chosen_calls.append(value)
        return False
class IndependentPair(Check):
    def check(self):
        frame = gdb.selected_frame()
        assert frame.name() == 'debug_hardware::independent_pair', frame.name()
        assert frame.older().find_sal().line == {independent_assignment}
        independent_calls.append(int(gdb.parse_and_eval('value')))
        return False
class AliasPair(Check):
    def check(self):
        frame = gdb.selected_frame()
        assert frame.name() == 'debug_hardware::alias_pair', frame.name()
        assert frame.older().find_sal().line == {alias_assignment}
        value = int(gdb.parse_and_eval('value'))
        assert frame.find_sal().line == ({positive_alias_return} if value > 0 else {fallback_alias_return})
        assert int(gdb.parse_and_eval('pair.left')) == value
        assert int(gdb.parse_and_eval('pair.right')) == value + 1
        alias_calls.append(value)
        return False
class MatchedPair(Check):
    def check(self):
        frame = gdb.selected_frame()
        assert frame.name() == 'debug_hardware::match_pair', frame.name()
        assert frame.older().find_sal().line == {matched_assignment}
        value = int(gdb.parse_and_eval('value'))
        assert frame.find_sal().line == ({matched_return} if value == 6 else {default_return})
        assert int(gdb.parse_and_eval('local')) == value + (20 if value == 6 else 10)
        match_calls.append(value)
        return False
Typed({source:?} + ':{typed_return}')
Event({source:?} + ':{event_return}')
Aggregate({source:?} + ':{bundle_return}')
PairCall({source:?} + ':{pair_return}')
MaskedPair({source:?} + ':{masked_return}')
ChosenPair({source:?} + ':{nested_pair_return}')
ChosenPair({source:?} + ':{positive_pair_return}')
ChosenPair({source:?} + ':{fallback_pair_return}')
IndependentPair({source:?} + ':{independent_return}')
AliasPair({source:?} + ':{positive_alias_return}')
AliasPair({source:?} + ':{fallback_alias_return}')
MatchedPair({source:?} + ':{matched_return}')
MatchedPair({source:?} + ':{default_return}')
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
for line, (_, _, _, _, expected) in formal_cases.items():
    assert expected <= formal_seen[line] <= expected | {{0}}, (line, formal_seen[line])
assert {{29, 3}} <= packed_formals <= {{0, 29, 3}}, packed_formals
assert {{(0, 0), (0, 1), (1, 0)}} <= resolve_calls, resolve_calls
assert ('ufixed[8]', 'ufixed[7]') in fixed_types, fixed_types
assert {{24, 40}} <= fixed_inputs <= {{0, 24, 40}}, fixed_inputs
assert owners == {{'T::dut::<hardware:T.dut.selected>', 'T::other::<hardware:T.other.selected>'}}, owners
assert {{5, 6, -6, 10}} <= seen <= {{0, 5, 6, -6, 10}}, seen
assert events == {{6, -6}}, events
assert typed_seen
assert bundle_seen
assert all(pair_calls.count(value) == 1 for value in [5, 6, -6]), pair_calls
assert sorted(masked_calls) == [-6, 5, 6], masked_calls
assert sorted(chosen_calls) == [-6, 0, 5, 6], chosen_calls
assert sorted(independent_calls) == [-6, 0, 5, 6], independent_calls
assert sorted(alias_calls) == [-6, 0, 5, 6], alias_calls
assert sorted(match_calls) == [-6, 0, 5, 6], match_calls
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
            bundle_return = line("hardware aggregate return"),
            bundle_assignment = line("hardware aggregate assignment"),
            pair_assignment = line("hardware pair assignment"),
            pair_return = line("hardware pair return"),
            masked_assignment = line("masked pair assignment"),
            masked_return = line("masked pair return"),
            chosen_assignment = line("chosen pair assignment"),
            nested_pair_return = line("nested pair return"),
            positive_pair_return = line("positive pair return"),
            fallback_pair_return = line("fallback pair return"),
            independent_assignment = line("independent pair assignment"),
            independent_return = line("independent pair return"),
            alias_assignment = line("alias pair assignment"),
            positive_alias_return = line("positive alias pair return"),
            fallback_alias_return = line("fallback alias pair return"),
            matched_assignment = line("matched pair assignment"),
            matched_return = line("matched pair return"),
            default_return = line("default pair return"),
            conversion_formal = line("conversion formal stop"),
            integer_suffix_formal = line("integer suffix formal stop"),
            real_suffix_formal = line("real suffix formal stop"),
            constrained_formal = line("constrained formal stop"),
            generic_formal = line("hardware generic formal stop"),
            packed_formal = line("packed Self formal stop"),
            resolve_formal = line("resolve formal stop"),
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
        for (callee, caller, markers, expected) in [
            (
                "make_pair",
                "sum_pair",
                vec!["hardware pair return"],
                vec![(5, 6, 7), (6, 7, 8), (-6, -5, -4), (0, 1, 2)],
            ),
            (
                "choose_pair",
                "sum_chosen_pair",
                vec![
                    "nested pair return",
                    "positive pair return",
                    "fallback pair return",
                ],
                vec![(5, 7, 9), (6, 9, 10), (-6, -5, 5), (0, 1, 11)],
            ),
            (
                "independent_pair",
                "sum_independent_pair",
                vec!["independent pair return"],
                vec![(5, 5, 6), (6, 6, 9), (-6, -7, -5), (0, -1, 1)],
            ),
            (
                "alias_pair",
                "sum_alias_pair",
                vec!["positive alias pair return", "fallback alias pair return"],
                vec![(5, 5, 6), (6, 6, 7), (-6, -6, 5), (0, 0, 11)],
            ),
            (
                "match_pair",
                "sum_matched_pair",
                vec!["matched pair return", "default pair return"],
                vec![(5, 15, 16), (6, 26, 27), (-6, 4, 5), (0, 10, 11)],
            ),
        ] {
            let lines = markers.into_iter().map(line).collect::<Vec<_>>();
            let finish = format!(
                r#"import gdb
stops = [gdb.Breakpoint({source:?} + ':' + str(line)) for line in {lines:?}]
gdb.execute('run')
for value, left, right in {expected:?}:
    assert gdb.selected_frame().name() == 'debug_hardware::' + {callee:?}
    assert int(gdb.parse_and_eval('value')) == value
    report = gdb.execute('finish', to_string=True)
    returned = gdb.history(0)
    assert int(returned['left']) == left, ('aggregate finish left', str(returned), report)
    assert int(returned['right']) == right, ('aggregate finish right', str(returned), report)
    assert gdb.selected_frame().name() == 'debug_hardware::' + {caller:?}
    gdb.execute('continue')
assert int(gdb.parse_and_eval('$_exitcode')) == 0
"#,
                source = source.to_string_lossy(),
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
                        &format!("python {finish}"),
                    ])
                    .output()
                    .unwrap(),
            );
        }
        let finish = format!(
            r#"import gdb
gdb.Breakpoint({source:?} + ':{conversion}')
gdb.Breakpoint({source:?} + ':{resolve}')
gdb.Breakpoint('std::fixed::from')
conversions, resolutions, fixed = set(), set(), set()
gdb.execute('run')
while gdb.selected_thread() is not None:
    frame = gdb.selected_frame()
    name = frame.name()
    if name == 'std::fixed::from':
        argument = frame.read_var('value')
        if argument.type.name.replace(' ', '') != 'ufixed[8]':
            gdb.execute('continue')
            continue
        value = int(argument)
        fixed.add(value)
        expected = value >> 2
    elif name == 'debug_hardware::from':
        value = int(frame.read_var('value'))
        conversions.add(value)
        expected = value + 1
    else:
        assert name == 'debug_hardware::resolve', name
        values = tuple(int(frame.read_var(parameter)) for parameter in ['self', 'rhs'])
        resolutions.add(values)
        expected = int(any(values))
    gdb.execute('finish')
    returned = gdb.history(0)
    if name == 'debug_hardware::from':
        assert returned.type.name == 'Count', str(returned.type)
        actual = int(returned['value'])
    else:
        actual = int(returned)
    assert actual == expected, (name, actual, expected)
    gdb.execute('continue')
assert int(gdb.parse_and_eval('$_exitcode')) == 0
assert {{-3, 3}} <= conversions <= {{-3, 0, 3}}, conversions
assert {{(0, 0), (0, 1), (1, 0), (1, 1)}} <= resolutions, resolutions
assert {{24, 40}} <= fixed <= {{0, 24, 40}}, fixed
"#,
            source = source.to_string_lossy(),
            conversion = line("conversion formal stop"),
            resolve = line("resolve formal stop"),
        );
        success(
            Command::new("gdb")
                .args(["-nx", "-q", "--batch"])
                .arg(&debug)
                .args([
                    "-ex",
                    "set debuginfod enabled off",
                    "-ex",
                    "set disable-randomization off",
                    "-ex",
                    "set breakpoint pending off",
                    "-ex",
                    &format!("python {finish}"),
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
