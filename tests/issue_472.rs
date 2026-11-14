//! Regression for sioxc #472: a method call on a string (`Char[]`) is
//! checked as one on any array is. Runtime strings skipped the check, so
//! any name passed and did not lower.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn unknown_string_methods_are_reported() {
    let path = std::env::temp_dir().join("siox_i472.siox");
    let text = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, SOURCE),
            Emit::Metadata,
        ))
        .render_diagnostics();
    assert!(
        text.contains("`string` has no method `any_arbitrary_method_call`"),
        "{text}"
    );
    assert!(
        text.contains("`string` has no method `completely_fake_method`"),
        "{text}"
    );
}

const SOURCE: &str = r#"module test_string_methods;

fn foo() {
    let s: string;
    let c: Char[];
    s.any_arbitrary_method_call("foo", 123);
    c.completely_fake_method(true);
}
"#;
