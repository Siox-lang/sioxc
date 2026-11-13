//! Regression for sioxc #717: an instance's output port is not written
//! through an index (`u.q[0] = ..`), a sub-field (`u.pt.x = ..`) or a tuple
//! target (`(u.q, x) = ..`). #610 caught only a write of the whole port.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn instance_output_writes_are_reported_in_every_form() {
    let path = std::env::temp_dir().join("siox_i717.siox");
    let text = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, SOURCE),
            Emit::Metadata,
        ))
        .render_diagnostics();
    for target in ["u.q[0]", "u.pt.x", "u.q`"] {
        assert!(
            text.contains(&format!("cannot assign to `{target}")),
            "{target}: {text}"
        );
    }
}

const SOURCE: &str = r#"module repro;

use std::bits::unsigned;

struct Pt {
    pub x: integer,
    pub y: integer,
}

entity Dut {
    q: unsigned[8] out,
    pt: Pt out,
}

impl Dut {
    q = 42;
    pt = Pt { .x = 1, .y = 2 };
}

#[test]
entity Tb {}

impl Tb {
    let u: Dut = {};

    process {
        await 1ns;
        let x: unsigned[8];
        // 1. Indexed write to `out` port `u.q`:
        u.q[0] = '1';
        // 2. Subfield write to struct `out` port `u.pt`:
        u.pt.x = 99;
        // 3. Tuple assignment to `out` port `u.q`:
        (u.q, x) = (100, 5);
        print!("u.q={} u.pt.x={}\n", u.q, u.pt.x);
    }
}
"#;
