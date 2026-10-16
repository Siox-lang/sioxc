//! Several DUT outputs on one testbench name: the testbench read whichever
//! output came last, so a disabled tristate driver's `'Z'` hid an enabled
//! one's `'1'`, and two enabled drivers never resolved. They now share a net
//! that folds through the type's `Resolve`; an unresolved type (`Bit`) is the
//! E-P014 conflict it is elsewhere.

use std::process::Command;

fn build(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_testbench_nets_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    let binary = dir.join(name);
    std::fs::write(&file, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
        ])
        .arg(&file)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text, binary)
}

#[test]
fn tristate_drivers_on_a_testbench_name_resolve() {
    let (ok, text, binary) = build(
        "tristate",
        r#"module g;
use std::logic::{Bit, Logic};
entity Drv { oe: Bit in, d: Logic[4] in, bus: Logic[4] out }
impl Drv { bus = if oe == '1' { d } else { "ZZZZ" }; }
entity Drv1 { oe: Bit in, d: Logic in, line: Logic out }
impl Drv1 { line = if oe == '1' { d } else { 'Z' }; }
#[test] entity T {}
impl T {
    let oe1: Bit = '1'; let oe2: Bit = '0';
    let d1: Logic[4] = "1010"; let d2: Logic[4] = "0110";
    let bus: Logic[4];
    let a: Drv = { .oe = oe1, .d = d1, .bus = bus };
    let b: Drv = { .oe = oe2, .d = d2, .bus = bus };
    let l1: Logic = '1'; let l2: Logic = 'H'; let line: Logic;
    let x: Drv1 = { .oe = oe1, .d = l1, .line = line };
    let y: Drv1 = { .oe = oe2, .d = l2, .line = line };
    check: process {
        await 1ns; print!("|A {} {}\n", bus, line);
        oe1 = '0'; oe2 = '1'; await 1ns; print!("|B {} {}\n", bus, line);
        oe1 = '1'; await 1ns; print!("|C {} {}\n", bus, line);
        oe1 = '0'; oe2 = '0'; await 1ns; print!("|D {} {}\n", bus, line);
    }
}
"#,
    );
    assert!(ok, "{text}");
    let report =
        String::from_utf8_lossy(&Command::new(&binary).output().unwrap().stdout).into_owned();
    for line in ["|A 1010 '1'", "|B 0110 'H'", "|C XX10 '1'", "|D ZZZZ 'Z'"] {
        assert!(report.contains(line), "{line}: {report}");
    }
}

#[test]
fn unresolved_outputs_on_a_testbench_name_conflict() {
    let (ok, text, _) = build(
        "bits",
        "module g;\nuse std::logic::Bit;\nentity D1 { a: Bit in, y: Bit out }\nimpl D1 { y = a; }\n\
         #[test] entity T {}\nimpl T { let a: Bit = '1'; let b: Bit = '0'; let y: Bit; \
         let x: D1 = { .a = a, .y = y }; let z: D1 = { .a = b, .y = y }; }\n",
    );
    assert!(
        !ok && text.contains("is driven by 2 conflicting sources"),
        "{text}"
    );
}
