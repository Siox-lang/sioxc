//! Regression for sioxc #637: a tuple type written as a trait argument
//! (`IndexAssign<integer, (integer, integer)>`) is read as that type, so an
//! index contract over tuples is checked. It read as nothing, and a write of
//! any value or a read into any binding passed and failed later.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

fn diagnostics(name: &str, source: &str) -> String {
    let path = std::env::temp_dir().join(format!("siox_i637_{name}.siox"));
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, source),
            Emit::Metadata,
        ))
        .render_diagnostics()
}

#[test]
fn a_tuple_index_contract_is_checked() {
    let text = diagnostics("write", WRITE);
    assert!(text.contains("indexed assignment on"), "{text}");
    let text = diagnostics("read", READ);
    assert!(
        text.contains("cannot initialize Bool with (integer, integer)"),
        "{text}"
    );
    assert!(
        text.contains("has no method `non_existent_method`"),
        "{text}"
    );
}

const WRITE: &str = r#"module test_tuple_idxassign;

struct PairContainer {
    pub a: integer,
    pub b: integer,
}

impl IndexAssign<integer, (integer, integer)> for PairContainer {
    fn index_assign(idx: integer, val: (integer, integer)) {
        self.a = val.0;
        self.b = val.1;
    }
}

#[test]
entity Tb {}

impl Tb {
    process {
        let c: PairContainer = PairContainer { .a = 1, .b = 2 };
        c[0] = true; // Bool assigned where (integer, integer) expected!
    }
}
"#;

const READ: &str = r#"module test_tuple_idx;

struct PairContainer {
    pub a: integer,
    pub b: integer,
}

impl Index<integer, (integer, integer)> for PairContainer {
    fn index(idx: integer) -> (integer, integer) {
        return (self.a, self.b);
    }
}

fn test() {
    let c: PairContainer = PairContainer { .a = 1, .b = 2 };
    let b: Bool = c[0]; // passes Stage 4 with 0 errors!
    c[0].non_existent_method(); // passes Stage 4 with 0 errors!
}
"#;
