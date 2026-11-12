//! Regression for sioxc #660: calling an entity method on an element of an
//! instance array (`cells[0].step()`) is reported where it is written. The
//! call skipped the caller rewrite and failed later: hardware lowering
//! could not build it, and a testbench crashed at run time.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

fn diagnostics(name: &str, source: &str) -> String {
    let path = std::env::temp_dir().join(format!("siox_i660_{name}.siox"));
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, source),
            Emit::Metadata,
        ))
        .render_diagnostics()
}

#[test]
fn a_method_call_on_an_instance_array_element_is_reported() {
    for (name, source) in [("hardware", HARDWARE), ("testbench", TESTBENCH)] {
        let text = diagnostics(name, source);
        assert!(
            text.contains("on an element of the instance array `cells` is not supported yet"),
            "{name}: {text}"
        );
    }
}

#[test]
fn a_method_call_on_a_single_instance_still_compiles() {
    let text = diagnostics("single", SINGLE);
    assert!(!text.contains("error"), "{text}");
}

const HARDWARE: &str = r#"module test;

entity Cell {}

impl Cell {
    pub fn step() -> integer {
        return 1;
    }
}

entity Top {
    out_val: integer out,
}

impl Top {
    let cells: Cell[2];
    out_val = cells[0].step();
}
"#;

const TESTBENCH: &str = r#"module test;

entity Cell {}

impl Cell {
    pub fn step() -> integer {
        return 1;
    }
}

#[test]
entity TestTop {}

impl TestTop {
    let cells: Cell[2];

    process {
        let x: integer = cells[0].step();
        assert!(x == 1);
    }
}
"#;

const SINGLE: &str = r#"module single;

entity Cell {}

impl Cell {
    pub fn step() -> integer {
        return 1;
    }
}

entity Top {
    out_val: integer out,
}

impl Top {
    let cell: Cell;
    out_val = cell.step();
}
"#;
