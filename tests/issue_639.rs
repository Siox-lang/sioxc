//! Regression for part of sioxc #639: an operator impl returning a tuple
//! (`impl Add<integer> for Point { fn add(..) -> (integer, integer) }`)
//! types its result as that tuple. Its output had no type key, so the
//! written type was dropped and the result typed as the left operand:
//! `let q: Point = p + 5` passed the checker and crashed at run time.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn a_tuple_operator_output_is_its_type() {
    let path = std::env::temp_dir().join("siox_i639.siox");
    let text = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, SOURCE),
            Emit::Metadata,
        ))
        .render_diagnostics();
    assert!(
        text.contains("cannot initialize Point with (integer, integer)"),
        "{text}"
    );
}

const SOURCE: &str = r#"module test_ret_tuple;

struct Point {
    pub x: integer,
    pub y: integer,
}

impl Add<integer> for Point {
    fn add(rhs: integer) -> (integer, integer) {
        return (self.x + rhs, self.y + rhs);
    }
}

#[test]
entity TB {}
impl TB {
    process {
        let p: Point = Point { .x = 1, .y = 2 };
        // Fails Stage 4: error[E-P003]: cannot assign Point to (integer, integer) without an explicit conversion
        // let (a, b): (integer, integer) = p + 5;

        // Passes Stage 4 with 0 diagnostics, but crashes simulation at runtime:
        let q: Point = p + 5;
    }
}
"#;
