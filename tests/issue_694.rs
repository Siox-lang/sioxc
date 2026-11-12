//! Regression for sioxc #694: a hardware `static let` written outside a
//! clock edge is reported however it is written: through a free procedure
//! that assigns its parameter (`inc_free(c);`), a qualified method call, or
//! a tuple or concatenation target. #633 saw only `c.inc();` and plain
//! targets. A procedure that only reads its parameter stays allowed.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

fn diagnostics(name: &str, source: &str) -> String {
    let path = std::env::temp_dir().join(format!("siox_i694_{name}.siox"));
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, source),
            Emit::Metadata,
        ))
        .render_diagnostics()
}

#[test]
fn unclocked_static_writes_through_calls_and_tuples_are_reported() {
    let text = diagnostics("unclocked", UNCLOCKED);
    assert!(
        text.contains("`inc_free(..)` may write `c` here without one"),
        "{text}"
    );
    assert!(text.contains("`a` is written here without one"), "{text}");
}

#[test]
fn clocked_writes_and_read_only_calls_are_accepted() {
    let text = diagnostics("clocked", CLOCKED);
    assert!(!text.contains("written only under a clock edge"), "{text}");
}

const UNCLOCKED: &str = r#"module test_static_unclocked;

use std::bits::unsigned;

pub struct Counter {
    pub v: unsigned[8],
}

fn inc_free(c: Counter) {
    c.v = c.v + 1;
}

fn make_pair() -> (unsigned[8], unsigned[8]) {
    return (1, 2);
}

entity TopFreeProc {
    clk: Bit in,
    out_v: unsigned[8] out,
}
impl TopFreeProc {
    process {
        static let c: Counter = { .v = 0 };
        inc_free(c);
        out_v = c.v;
    }
}

entity TopTuple {
    clk: Bit in,
    out_v: unsigned[8] out,
}
impl TopTuple {
    process {
        static let a: unsigned[8] = 0;
        let b: unsigned[8] = 0;
        (a, b) = make_pair();
        out_v = a;
    }
}
"#;

const CLOCKED: &str = r#"module p694;

use std::bits::unsigned;

pub struct Counter {
    pub v: unsigned[8],
}

fn inc_free(c: Counter) {
    c.v = c.v + 1;
}

fn read_only(c: Counter) -> unsigned[8] {
    return c.v;
}

fn peek(c: Counter) {
    let seen: unsigned[8] = c.v;
}

entity Top {
    clk: Bit in,
    out_v: unsigned[8] out,
}
impl Top {
    process {
        static let c: Counter = { .v = 0 };
        static let a: unsigned[8] = 0;
        let b: unsigned[8] = 0;
        if clk.rising() {
            inc_free(c);
            (a, b) = (c.v, c.v);
        }
        peek(c);
        out_v = read_only(c) + a;
    }
}
"#;
