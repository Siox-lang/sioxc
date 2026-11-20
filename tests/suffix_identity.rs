#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn aliased_suffix_contracts_execute_in_hardware_and_processes() {
    for reversed in [false, true] {
        let mut reference = None;
        for debug in [false, true] {
            let dir = std::env::temp_dir().join(format!(
                "siox-suffix-identity-{}-{reversed}-{debug}",
                std::process::id()
            ));
            std::fs::create_dir_all(dir.join("providers")).unwrap();
            for (name, scale, real_scale) in [("left", 37, 1.5), ("right", 11, 2.5)] {
                std::fs::write(
                    dir.join("providers").join(format!("{name}.siox")),
                    format!(
                        "module providers::{name};
                         pub struct Measure {{ pub value: integer }}
                         pub type Alias = Measure;
                         type Amount = integer;
                         impl Suffix<\"{name}unit\", integer> for Alias {{
                             static fn suffix(value: Amount) -> Alias {{
                                 return Alias {{ .value = value * {scale} }};
                             }}
                             static fn helper(value: Amount) -> Alias {{
                                 return Alias {{ .value = value + 1000 }};
                             }}
                         }}
                         pub struct Rate {{ pub value: real }}
                         pub type RateAlias = Rate;
                         type Scalar = real;
                         impl Suffix<\"{name}rate\", real> for RateAlias {{
                             static fn suffix(value: Scalar) -> RateAlias {{
                                 return RateAlias {{ .value = value * {real_scale} }};
                             }}
                             static fn helper(value: Scalar) -> RateAlias {{
                                 return RateAlias {{ .value = value + 1000.0 }};
                             }}
                         }}
                         pub struct Physical(real);
                         pub type PhysicalAlias = Physical;
                         fn adjust(value: Scalar) -> Scalar {{ return value + 1.0; }}
                         impl Suffix<\"{name}physical\", real> for PhysicalAlias {{
                             static fn suffix(value: Scalar) -> Self {{
                                 let local: Scalar = value * {real_scale};
                                 return Self(adjust(local));
                             }}
                         }}"
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
                "module suffix_identity_api;
                 {imports}
                 entity Dut {{
                     left: L::Alias out, right: R::Alias out,
                     lf: L::RateAlias out, rf: R::RateAlias out,
                     lp: L::PhysicalAlias out, rp: R::PhysicalAlias out
                 }}
                 impl Dut {{
                     left = 2leftunit; right = 3rightunit;
                     lf = 2.5leftrate; rf = 1.25rightrate;
                     lp = 2.5leftphysical; rp = 1.25rightphysical;
                 }}
                 #[test] entity Probe {{}}
                 impl Probe {{
                     let dut: Dut = {{}};
                     process {{
                         let left: L::Alias = 2leftunit;
                         let right: R::Alias = 3rightunit;
                         let lf: L::RateAlias = 2.5leftrate;
                         let rf: R::RateAlias = 1.25rightrate;
                         let lp: L::PhysicalAlias = 2.5leftphysical;
                         let rp: R::PhysicalAlias = 1.25rightphysical;
                         assert!(left.value == 74 and right.value == 33);
                         assert!(lf.value == 3.75 and rf.value == 3.125);
                         assert!(real(lp) == 4.75 and real(rp) == 4.125);
                         await 1ns;
                         assert!(dut.left.value == 74 and dut.right.value == 33);
                         assert!(dut.lf.value == 3.75 and dut.rf.value == 3.125);
                         assert!(real(dut.lp) == 4.75 and real(dut.rp) == 4.125);
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
                assert_eq!(&wave, reference, "debug waveform must preserve behavior");
            } else {
                reference = Some(wave);
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
