//! Regression for sioxc #491: an `Into` impl converts only the type
//! arguments it declares. Keyed by bare names, `impl<T> Into<Target<T>> for
//! Source<T>` took `Source<real>` into `Target<integer>`, and `impl
//! Into<Target<integer>> for Source<integer>` took `Source<real>` into
//! `Target<string>`.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

fn diagnostics(name: &str, source: &str) -> String {
    let path = std::env::temp_dir().join(format!("siox_i491_{name}.siox"));
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, source),
            Emit::Metadata,
        ))
        .render_diagnostics()
}

#[test]
fn into_holds_its_type_arguments() {
    let text = diagnostics("generic", GENERIC_MISMATCH);
    assert!(text.contains("cannot assign Source to Target"), "{text}");
    let text = diagnostics("monomorphic", MONOMORPHIC_MISMATCH);
    assert!(
        text.contains("cannot initialize Target with Source"),
        "{text}"
    );
    let text = diagnostics("matching", MATCHING);
    assert!(!text.contains("error"), "{text}");
}

const GENERIC_MISMATCH: &str = r#"module test_generic_into_hw;

struct Source<T> { pub val: T }
struct Target<T> { pub val: T }

// Only meant for matching T:
impl<T> Into<Target<T>> for Source<T> {
    fn into() -> Target<T> {
        return Target<T> { .val = self.val };
    }
}

entity Top {
    s: Source<real> in,
    t: Target<integer> out,
}

impl Top {
    // Stage 4 Typecheck passes!
    // Silently bitcasts real bits to integer bits via RawResize!
    t = s;
}
"#;

const MONOMORPHIC_MISMATCH: &str = r#"module test_generic_into_sim;

struct Source<T> { pub val: T }
struct Target<T> { pub val: T }

// Only implemented for integer -> integer!
impl Into<Target<integer>> for Source<integer> {
    fn into() -> Target<integer> {
        return Target<integer> { .val = self.val };
    }
}

#[test]
entity Top {}
impl Top {
    check: process {
        let s: Source<real> = Source<real> { .val = 3.14 };
        // Source<real> implicitly converts to Target<string>!
        let t: Target<string> = s;
        // String iteration crashes at runtime because t.val contains a float:
        for ch in t.val {
            print!("ch = {}\n", ch);
        }
    }
}
"#;

const MATCHING: &str = r#"module p491;

struct Source<T> { pub val: T }
struct Target<T> { pub val: T }

impl<T> Into<Target<T>> for Source<T> {
    fn into() -> Target<T> {
        return Target<T> { .val = self.val };
    }
}

entity Top {
    s: Source<integer> in,
    t: Target<integer> out,
}

impl Top {
    t = s;
}
"#;
