#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn borrowed_view_aliases_wire_native_inputs_and_outputs() {
    for reversed in [false, true] {
        let mut reference = None;
        for debug in [false, true] {
            let dir = std::env::temp_dir().join(format!(
                "siox-borrowed-views-{}-{reversed}-{debug}",
                std::process::id()
            ));
            std::fs::create_dir_all(dir.join("providers")).unwrap();
            for (name, data, ready) in [("left", "out", "in"), ("right", "in", "out")] {
                std::fs::write(
                    dir.join("providers").join(format!("{name}.siox")),
                    format!(
                        "module providers::{name};
                     pub struct Bus {{ pub data: integer, pub ready: integer }}
                     pub struct Queue {{ pub data: integer, pub ready: integer }}
                     pub type Alias = Bus;
                     pub type QueueAlias = Queue;
                     pub view Source for Bus {{ data {data}, ready {ready} }}
                     pub view Source for Queue {{ data {ready}, ready {data} }}
                     pub type Port = Alias Source;"
                    ),
                )
                .unwrap();
            }
            let imports = if reversed {
                "use R = providers::right; use L = providers::left;"
            } else {
                "use L = providers::left; use R = providers::right;"
            };
            let source = format!(
                "module borrowed_views_api;
                 {imports}
                 entity Dut {{
                     left: L::Port, queue: L::QueueAlias L::Source,
                     right: R::Alias R::Source, lanes: L::Alias[2] L::Source
                 }}
                 impl Dut {{
                     left.data = left.ready + 10;
                     queue.ready = queue.data + 20;
                     right.ready = right.data + 30;
                     for index in 0..1 {{ lanes[index].data = lanes[index].ready + 40; }}
                 }}
                 #[test] entity Probe {{}}
                 impl Probe {{
                     let left: L::Alias = {{ .data = 0, .ready = 3 }};
                     let queue: L::QueueAlias = {{ .data = 5, .ready = 0 }};
                     let right: R::Alias = {{ .data = 7, .ready = 0 }};
                     let lanes: L::Alias[2] = [
                         L::Alias {{ .data = 0, .ready = 11 }},
                         L::Alias {{ .data = 0, .ready = 13 }}
                     ];
                     let dut: Dut = {{ .left = left, .queue = queue, .right = right, .lanes = lanes }};
                     process {{
                         await 1ns;
                         assert!(left.data == 13 and queue.ready == 25 and right.ready == 37);
                         assert!(lanes[0].data == 51 and lanes[1].data == 53);
                         assert!(dut.left.ready == 3 and dut.queue.data == 5 and dut.right.data == 7);
                         left.ready = 17; queue.data = 19; right.data = 23;
                         lanes[0].ready = 29; lanes[1].ready = 31;
                         await 1ns;
                         assert!(left.data == 27 and queue.ready == 39 and right.ready == 53);
                         assert!(lanes[0].data == 69 and lanes[1].data == 71);
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
                    "debug waveform must preserve alias wiring"
                );
            } else {
                reference = Some(wave);
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
