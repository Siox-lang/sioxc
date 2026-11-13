//! Regression for sioxc #710: a bare call of a method name with no
//! arguments (`rising()`, `default()`) is reported, as one with arguments
//! is (#634); it passed the checker and failed in lowering. A bare call of
//! a `static fn` points at `Owner::name(..)`, not at a receiver.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn bare_method_and_static_calls_are_reported() {
    let path = std::env::temp_dir().join("siox_i710.siox");
    let text = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, SOURCE),
            Emit::Metadata,
        ))
        .render_diagnostics();
    assert!(
        text.contains("`rising` is a method, not a function"),
        "{text}"
    );
    assert!(
        text.contains("call it on its receiver: `value.rising()`"),
        "{text}"
    );
    assert!(
        text.contains("`default` is an associated function"),
        "{text}"
    );
    assert!(
        text.contains("call it through its type: `Type::default()`"),
        "{text}"
    );
    assert!(
        text.contains("call it through its type: `Foo::make(5)`"),
        "{text}"
    );
}

const SOURCE: &str = r#"module repro;

struct Foo {
    pub x: integer,
}

impl Foo {
    pub static fn make(x: integer) -> Foo {
        return Foo { .x = x };
    }
}

entity Top {
    clk: Bit in,
    y: unsigned[8] out,
}

impl Top {
    let r: unsigned[8] = 0;
    // 1. Passes typecheck with 0 errors, then fails in hardware lowering with E-P017:
    if rising() {
        r = 1;
    }
    y = r;
}

#[test]
entity Tb {}

impl Tb {
    let clk: Bit;
    let y: unsigned[8];
    let dut: Top = { .clk = clk, .y = y };
    process {
        // 2. Passes typecheck with 0 errors, then aborts at testbench runtime:
        let x: unsigned[8] = default();
        // 3. Suggests `5.make()` instead of `Foo::make(5)`:
        let f: Foo = make(5);
        await 1ns;
    }
}
"#;
