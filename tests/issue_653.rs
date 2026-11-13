//! Regression for sioxc #653: a `Display::fmt` body holding anything but
//! `let`s and `write!` (an `if`, a loop, an assignment) is reported when it
//! is checked. It is expanded into the caller's format at compile time, so
//! such a body built a test that aborted when it printed.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn an_unsupported_display_body_is_reported() {
    let path = std::env::temp_dir().join("siox_i653.siox");
    let text = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, SOURCE),
            Emit::Metadata,
        ))
        .render_diagnostics();
    assert!(
        text.contains("a `Display::fmt` body is formatted at compile time"),
        "{text}"
    );
}

const SOURCE: &str = r#"module test_fmt_if;

struct Pair {
    pub a: integer,
    pub b: integer,
}

impl Display for Pair {
    fn fmt(f: Formatter) {
        if self.a > 0 {
            write!(f, "pos");
        } else {
            write!(f, "neg");
        }
    }
}

#[test]
entity Tb {}

impl Tb {
    check: process {
        let p: Pair = { .a = 5, .b = 10 };
        print!("{}", p);
    }
}
"#;
