//! A clocked block nested under a condition ignored the condition: the
//! asynchronous reset `if rst == '1' { .. } else if clk.rising() { .. }`
//! (spec 3.15) kept counting edges while the reset was held, and a clock
//! enable `if en == '1' { if clk.rising() { .. } }` counted with `en` low.

#[cfg(feature = "llvm")]
fn run(name: &str, source: &str) -> String {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_edge_guards_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join(name);
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join(format!("{name}.siox")), source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let output = std::process::Command::new(&binary).output().unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[cfg(feature = "llvm")]
#[test]
fn a_held_reset_and_a_low_enable_stop_the_count() {
    let report = run(
        "guards",
        r#"module g;
use std::bits::unsigned;
use std::logic::Bit;
entity Cnt { clk: Bit in, rst: Bit in, en: Bit in, r: unsigned[8] out, e: unsigned[8] out }
impl Cnt {
    let rc: unsigned[8] = 0; let ec: unsigned[8] = 0;
    reset: process { if rst == '1' { rc = 0; } else if clk.rising() { rc = rc + 1; } }
    enable: process { if en == '1' { if clk.rising() { ec = ec + 1; } } }
    r = rc; e = ec;
}
#[test] entity T {}
impl T {
    let clk: Bit = '0'; let rst: Bit = '0'; let en: Bit = '1';
    let r: unsigned[8]; let e: unsigned[8];
    let c: Cnt = { .clk = clk, .rst = rst, .en = en, .r = r, .e = e };
    clk = not clk after 5ns;
    check: process {
        await 32ns;
        print!("|A {} {}\n", r, e);
        rst = '1'; en = '0';
        await 40ns;
        print!("|B {} {}\n", r, e);
        rst = '0'; en = '1';
        await 20ns;
        print!("|C {} {}\n", r, e);
    }
}
"#,
    );
    // Edges at 5, 15, 25 ns; 35..65 held; then 75 and 85 ns.
    assert!(report.contains("|A 3 3\n"), "{report}");
    assert!(report.contains("|B 0 3\n"), "{report}");
    assert!(report.contains("|C 2 5\n"), "{report}");
}
