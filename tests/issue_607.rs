//! Regression for sioxc #607: a method call on a view-typed value
//! (`m: Bus Master`, `impl Bus Master { .. }`) is checked like any other: an
//! unknown method, a wrong argument count, and a testbench-only operation
//! reached from hardware are reported. The methods are keyed by
//! `View@Target`, the receiver by the view alone, so none was found and
//! every check of the call was skipped.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

fn diagnostics(name: &str, source: &str) -> String {
    let path = std::env::temp_dir().join(format!("siox_i607_{name}.siox"));
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, source),
            Emit::Metadata,
        ))
        .render_diagnostics()
}

#[test]
fn view_method_calls_are_checked() {
    let text = diagnostics("hardware", HARDWARE);
    assert!(
        text.contains("has no method `non_existent_method`"),
        "{text}"
    );
    assert!(
        text.contains("takes 1 argument(s) but 3 were given"),
        "{text}"
    );
    assert!(text.contains("`m.send` reaches `await`"), "{text}");
    let text = diagnostics("testbench", TESTBENCH);
    assert!(
        text.contains("has no method `non_existent_method`"),
        "{text}"
    );
}

const HARDWARE: &str = r#"module repro_view_bypass;

struct Bus {
    pub valid: Bit,
    pub data: unsigned[8]
}

view Master for Bus {
    valid out,
    data out
}

impl Bus Master {
    pub fn send(val: unsigned[8]) {
        self.data = val;
        self.valid = '1';
        await 10ns;
        self.valid = '0';
    }
}

entity Top {
    m: Bus Master
}

impl Top {
    // 1. Non-existent method accepted without error
    m.non_existent_method(1, 2, 3);

    // 2. Argument type/count mismatch accepted without error;
    // await 10ns is dropped and conflicting drivers (1 and 0) are emitted
    m.send("wrong", true, 999);
}
"#;

const TESTBENCH: &str = r#"module repro_tb_view;

struct Bus {
    pub valid: Bit
}

view Master for Bus {
    valid out
}

impl Bus Master {
    pub fn send() {}
}

#[test]
entity Tb {}

impl Tb {
    let m: Bus Master;

    run: process {
        m.non_existent_method();
    }
}
"#;
