#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn same_named_conversions_keep_the_selected_body_in_hardware_and_processes() {
    for reversed in [false, true] {
        let mut reference = None;
        for debug in [false, true] {
            let dir = std::env::temp_dir().join(format!(
                "siox-into-identity-{}-{reversed}-{debug}",
                std::process::id()
            ));
            std::fs::create_dir_all(dir.join("providers")).unwrap();
            for (name, amount) in [("left", 11), ("right", 29)] {
                std::fs::write(dir.join("providers").join(format!("{name}.siox")), format!(
                    "module providers::{name};
                     pub struct Token {{ pub value: integer }}
                     pub struct Target {{ pub value: integer }}
                     pub type Input = Token; pub type Output = Target;
                     impl Into<Target> for Token {{
                         fn into() -> Target {{ return Target {{ .value = self.value + {amount} }}; }}
                     }}"
                )).unwrap();
            }
            let imports = if reversed {
                "use R = providers::right; use L = providers::left;"
            } else {
                "use L = providers::left; use R = providers::right;"
            };
            let source = format!(
                r#"module into_identity_api;
                {imports}
                struct Pair {{ pub left: L::Output, pub right: R::Output }}
                fn consume_left(value: L::Output) -> integer {{ return value.value; }}
                fn consume_right(value: R::Output) -> integer {{ return value.value; }}
                entity Dut {{
                    l: L::Input in, r: R::Input in,
                    yl: L::Output out, yr: R::Output out,
                    vl: integer out, vr: integer out, sum: integer out
                }}
                impl Dut {{
                    yl = l; yr = r;
                    vl = consume_left(l); vr = consume_right(r);
                    let pair: Pair;
                    pair = {{ .left = l, .right = r }};
                    sum = pair.left.value + pair.right.value;
                }}
                fn convert_left(value: L::Input) -> L::Output {{ return value; }}
                fn convert_right(value: R::Input) -> R::Output {{ return value; }}
                #[test] entity Probe {{}}
                impl Probe {{
                    let l: L::Input = {{ .value = 3 }};
                    let r: R::Input = {{ .value = 5 }};
                    let dut: Dut = {{ .l = l, .r = r }};
                    process {{
                        let a: L::Output = l;
                        let b: R::Output = r;
                        assert!(a.value == 14 and b.value == 34);
                        let c: L::Output = convert_left(l);
                        let d: R::Output = convert_right(r);
                        assert!(c.value == 14 and d.value == 34);
                        assert!(consume_left(l) == 14 and consume_right(r) == 34);
                        let pair: Pair = {{ .left = l, .right = r }};
                        assert!(pair.left.value == 14 and pair.right.value == 34);
                        await 1ns;
                        assert!(dut.yl.value == 14 and dut.yr.value == 34);
                        assert!(dut.vl == 14 and dut.vr == 34 and dut.sum == 48);
                        l.value = 7; r.value = 9;
                        await 1ns;
                        assert!(dut.yl.value == 18 and dut.yr.value == 38);
                        assert!(dut.vl == 18 and dut.vr == 38 and dut.sum == 56);
                        a = l; b = r;
                        assert!(a.value == 18 and b.value == 38);
                    }}
                }}
            "#
            );
            let binary = dir.join("probe");
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
                "{}\n{:?}",
                compilation.render_diagnostics(),
                compilation.failure
            );
            let wave = dir.join("output.vcd");
            let run = std::process::Command::new(&binary)
                .arg("-o")
                .arg(&wave)
                .output()
                .unwrap();
            assert!(
                run.status.success(),
                "reversed={reversed}, debug={debug}: {}\n{}\nartifact: {}",
                String::from_utf8_lossy(&run.stdout),
                String::from_utf8_lossy(&run.stderr),
                binary.display()
            );
            let trace = std::fs::read(&wave).unwrap();
            assert!(!trace.is_empty());
            if let Some(expected) = &reference {
                assert_eq!(&trace, expected, "debugging must not change the waveform");
            } else {
                reference = Some(trace);
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
