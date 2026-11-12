#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn nominal_derived_constants_drive_native_layouts_and_simulation() {
    for reversed in [false, true] {
        let mut reference = None;
        for debug in [false, true] {
            let dir = std::env::temp_dir().join(format!(
                "siox-derived-constants-{}-{reversed}-{debug}",
                std::process::id()
            ));
            std::fs::create_dir_all(dir.join("providers")).unwrap();
            std::fs::write(
                dir.join("providers/left.siox"),
                "module providers::left;
                 pub struct Word(integer);
                 pub type Alias = Word;
                 pub struct Count(natural);
                 pub struct Rate(real);
                 pub enum Mode { A = 3, B = 9 }
                 pub enum DerivedMode(Mode);
                 impl From<Mode> for DerivedMode {
                     static fn from(value: Mode) -> Self {
                         if integer(value) == 3 { return Self::B; }
                         return Self::A;
                     }
                 }
                 impl Eq<Mode> for Mode {
                     fn eq(self, rhs: Mode) -> Bool { return true; }
                 }
                 const TWO: natural = 2;
                 pub const WIDTH: Alias = Alias(3);
                 pub const COUNT: Count = Count(TWO);
                 pub const GAIN: Rate = Rate(2.5);
                 pub const MODE: DerivedMode = DerivedMode(Mode::A);
                 pub const DESCENDING: integer[2..0] = [7, 9, 11];
                 pub const LABELLED: integer[6..4] = [13, 15, 17];
                 pub struct Config { pub width: Alias, pub count: Count }
                 pub struct Envelope(Config);
                 pub const CFG: Envelope = Envelope(Config { .width = WIDTH, .count = COUNT });
                 pub fn width() -> integer {
                     if Mode(MODE) == Mode::B {
                         return if DerivedMode::A != DerivedMode::B { integer(CFG.width) } else { 99 };
                     }
                     return 99;
                 }
                 pub fn count() -> integer { return integer(natural(CFG.count)); }
                 pub fn gain() -> real { return real(GAIN); }",
            )
            .unwrap();
            std::fs::write(
                dir.join("providers/right.siox"),
                "module providers::right;
                 pub struct Word(integer);
                 pub const WIDTH: Word = Word(91);",
            )
            .unwrap();
            let imports = if reversed {
                "use R = providers::right; use L = providers::left;"
            } else {
                "use L = providers::left; use R = providers::right;"
            };
            let source = format!(
                "module derived_contexts_api;
                 use std::bits::unsigned;
                 {imports}
                 pub attr score: integer for entity = L::width();
                 entity Dut {{
                     source: unsigned[L::width()] in,
                     result: unsigned[L::width()] out,
                     lanes: integer[L::count()] out,
                     index: integer in,
                     descending: integer out,
                     labelled: integer out,
                     gain: real out
                 }}
                 impl Dut {{
                     attr score = L::width();
                     result = source;
                     gain = L::gain();
                     descending = L::DESCENDING[index];
                     labelled = L::LABELLED[index + 4];
                     for index in 0..(L::count() - 1) {{ lanes[index] = index + 10; }}
                 }}
                 #[test] entity Probe {{}}
                 impl Probe {{
                     let input: unsigned[3] = 5;
                     let index: integer = 2;
                     let dut: Dut = {{ .source = input, .index = index }};
                     process {{
                         let wrapped: L::Alias = L::Alias(7);
                         assert!(integer(wrapped) == 7 and integer(R::WIDTH) == 91);
                         assert!(L::Mode::A == L::Mode::B);
                         assert!(L::DerivedMode::A != L::DerivedMode::B);
                         assert!(integer(L::DerivedMode(L::Mode::B)) == 3);
                         assert!(integer(L::Mode(L::MODE)) == 9);
                         assert!(dut.source'length == 3 and dut.lanes'length == 2);
                         await 1ns;
                         assert!(integer(dut.result) == 5 and dut.gain == 2.5);
                         assert!(dut.lanes[0] == 10 and dut.lanes[1] == 11);
                         assert!(dut.descending == 7 and dut.labelled == 13);
                         assert!(dut'score == 3);
                         input = 3;
                         index = 0;
                         await 1ns;
                         assert!(integer(dut.result) == 3);
                         assert!(dut.descending == 11 and dut.labelled == 17);
                     }}
                 }}"
            );
            let binary = dir.join("test");
            let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
                CompileRequest::new(
                    SourceInput::memory(dir.join("input.siox"), source),
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
                dir.display()
            );
            let waveform = dir.join("wave.vcd");
            let output = std::process::Command::new(&binary)
                .arg("-o")
                .arg(&waveform)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}\nretained artifact: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
                dir.display()
            );
            let wave = std::fs::read(&waveform).unwrap();
            if let Some(reference) = &reference {
                assert_eq!(
                    &wave, reference,
                    "debug output must preserve simulation behavior"
                );
            } else {
                reference = Some(wave);
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
