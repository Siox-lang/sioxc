//! Regression for sioxc #581: an entity's signal declared with a bare vector
//! family (`let s: unsigned = unsigned[8](5);`) needs its width written; it
//! failed native lowering in a testbench and was "not a constant" in
//! hardware. A process local still takes its initializer's width.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

fn diagnostics(name: &str, source: &str) -> String {
    let path = std::env::temp_dir().join(format!("siox_i581_{name}.siox"));
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, source),
            Emit::Metadata,
        ))
        .render_diagnostics()
}

#[test]
fn a_signal_writes_its_width() {
    let text = diagnostics("signal", SIGNAL);
    assert!(
        text.contains("its width must be set in a signal's declaration"),
        "{text}"
    );
}

#[test]
fn a_process_local_takes_its_initializer_width() {
    let text = diagnostics("local", LOCAL);
    assert!(!text.contains("error"), "{text}");
}

const SIGNAL: &str = r#"module gap;
use std::bits::unsigned;
#[test] entity TB {}
impl TB {
    let s: unsigned = unsigned[8](5);
    process {
        assert!(s == 5, "s {}", s);
        print!("done\n");
    }
}
"#;

const LOCAL: &str = r#"module local;
use std::bits::unsigned;
#[test] entity TB {}
impl TB {
    process {
        let s: unsigned = unsigned[8](5);
        assert!(s == 5, "s {}", s);
        print!("done\n");
    }
}
"#;
