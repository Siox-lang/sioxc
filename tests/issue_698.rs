//! Regression for sioxc #698: a generate `for` inside a scoped block binds
//! its own loop variable. Its `i` hid the block member `i` without a word,
//! and every use of it read the member (99 for 0); it is now the shadowing
//! error a process's loop variable is (#402), and a distinct name works.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

fn diagnostics(name: &str, source: &str) -> String {
    let path = std::env::temp_dir().join(format!("siox_i698_{name}.siox"));
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, source),
            Emit::Metadata,
        ))
        .render_diagnostics()
}

#[test]
fn a_generate_loop_variable_hiding_a_block_member_is_reported() {
    let text = diagnostics("hidden", HIDDEN);
    assert!(
        text.contains("`i` is already visible from an enclosing scope"),
        "{text}"
    );
}

#[test]
fn a_generate_loop_with_its_own_name_is_accepted() {
    let text = diagnostics("distinct", DISTINCT);
    assert!(!text.contains("error"), "{text}");
}

const HIDDEN: &str = r#"module test_block_for_hijack;

entity Top {
    out_a: integer out,
    out_b: integer out,
}
impl Top {
    blk: {
        let i: integer = 99;
        out_a = i;
        for i in 0..0 {
            out_b = i;
        }
    }
}

#[test]
entity Tb {}
impl Tb {
    let a: integer;
    let b: integer;
    let d: Top = { .out_a = a, .out_b = b };
    process {
        await 1ns;
        print!("a={} b={}\n", a, b);
        assert!(b == 0, "expected b == 0 from loop variable i, got {}", b);
    }
}
"#;

const DISTINCT: &str = r#"module p698;

entity Top {
    out_a: integer out,
    out_b: integer out,
}
impl Top {
    blk: {
        let i: integer = 99;
        out_a = i;
        for k in 0..0 {
            out_b = k;
        }
    }
}

#[test]
entity Tb {}
impl Tb {
    let a: integer;
    let b: integer;
    let d: Top = { .out_a = a, .out_b = b };
    process {
        await 1ns;
        print!("a={} b={}\n", a, b);
        assert!(b == 0, "expected b == 0 from loop variable i, got {}", b);
    }
}
"#;
