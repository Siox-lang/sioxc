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
fn direct_dwarf_supports_source_steps_and_persistent_values() {
    let directory = std::env::temp_dir().join(format!("siox_dwarf_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("debug.siox");
    let mut text = r#"module debug;
use std::bits::{unsigned, signed};
enum Mode { Idle = 3, Active = 7 }
struct Packet { pub tag: unsigned[3], pub data: unsigned[128], pub gain: real }
struct LogicPacket { pub tag: unsigned[3], pub gain: real, pub data: unsigned[4] }
entity LogicProbe { states: unsigned[9] in, ascending: Logic[-2..0] in, echo: unsigned[9] out }
impl LogicProbe { echo = states; }
entity Probe { a: unsigned[3] in, b: unsigned[5] in, y: unsigned[8] out, negative: signed[81] in }
impl Probe { y = unsigned[8](a) + unsigned[8](b); }
#[test] entity T {}
impl T {
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
        for i in 0..1 {
            await 1ns;
            assert!(i >= 0, "loop index");
        }
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
    std::fs::write(&source, &text).unwrap();
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
    let debug = directory.join("debug.bin");
    compile(&["--test", "--debug"], &debug);
    let normal = directory.join("normal.bin");
    compile(&["--test"], &normal);
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
entry = gdb.Breakpoint('T::stimulus')
gdb.execute('run')
assert gdb.selected_frame().name() == 'T::stimulus'
entry.delete()
gdb.execute('continue')
frame = gdb.selected_frame()
assert frame.find_sal().line == {line}
assert frame.name() == 'T::stimulus', frame.name()
def value(expr): return gdb.parse_and_eval(expr)
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
loop = gdb.Breakpoint({source:?} + ':{loop_line}')
for expected in [0, 1]:
    gdb.execute('continue')
    assert gdb.selected_frame().find_sal().line == {loop_line}
    assert int(value('i')) == expected
loop.delete()
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
