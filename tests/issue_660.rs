//! Regression for sioxc #660: calling an entity method on an element of an
//! instance array (`cells[0].step()`) skipped the caller rewrite and failed
//! later. A hardware caller now drives the element's method ports (see
//! `tests/entity_method_arrays.rs`); what is not supported is reported where
//! it is written: a testbench caller, a nested index, and a second call
//! site on an element.

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
fn a_hardware_call_on_an_instance_array_element_compiles() {
    let text = diagnostics("hardware", HARDWARE);
    assert!(!text.contains("error"), "{text}");
}

#[test]
fn unsupported_array_element_calls_are_reported() {
    for (name, source, message) in [
        (
            "testbench",
            TESTBENCH,
            "calling entity method `cells[..].step` from a testbench is not supported yet",
        ),
        (
            "nested",
            NESTED,
            "calling entity method `step` through a nested index of `grid` is not supported yet",
        ),
        (
            "clash",
            CLASH,
            "`cells[..].step` is already called on this element",
        ),
    ] {
        let text = diagnostics(name, source);
        assert!(text.contains(message), "{name}: {text}");
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

const NESTED: &str = r#"module nested;

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
    let grid: Cell[2][2];
    out_val = grid[0][1].step();
}
"#;

const CLASH: &str = r#"module clash;

entity Cell {}

impl Cell {
    pub fn step() -> integer {
        return 1;
    }
}

entity Top {
    sel: integer<0..1> in,
    a: integer out,
    b: integer out,
}

impl Top {
    let cells: Cell[2];
    a = cells[1].step();
    b = cells[sel].step();
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
