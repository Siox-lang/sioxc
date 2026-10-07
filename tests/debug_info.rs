//! Source metadata must describe the direct LLVM pipeline and real storage.
#![cfg(feature = "cli")]

use std::process::{Command, Output};

fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
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
assert gdb.selected_frame().name() == 'debug_calls::check_string'
view = gdb.parse_and_eval('value')
assert int(view['length']) == 3 and int(view['byte_length']) == 7
assert [int(view['data'][i]) for i in range(3)] == [ord(c) for c in 'β\x00🙂']
assert bytes(int(view['utf8'][i]) for i in range(7)) == 'β\x00🙂'.encode()
string.delete()
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
