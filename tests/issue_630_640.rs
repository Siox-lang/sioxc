//! Regression for sioxc #630 and #640: an `Index`/`IndexAssign` contract
//! keeps its value type's width and element array. Its signature kept a
//! family name only, so `unsigned[8]` read or took any width (#630), and an
//! element array (`integer[4]`) none at all: a read was `Ty::Error`, which
//! no later check saw through, and a write took any value (#640).

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

fn diagnostics(name: &str, source: &str) -> String {
    let path = std::env::temp_dir().join(format!("siox_i630_{name}.siox"));
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, source),
            Emit::Metadata,
        ))
        .render_diagnostics()
}

#[test]
fn a_contract_reads_and_writes_its_width() {
    let text = diagnostics("read", READ_WIDTH);
    assert!(
        text.contains("cannot initialize unsigned[16] with unsigned[8]"),
        "{text}"
    );
    let text = diagnostics("write", WRITE_WIDTH);
    assert!(text.contains("indexed assignment on"), "{text}");
}

#[test]
fn an_element_array_contract_is_typed() {
    let text = diagnostics("array", ELEMENT_ARRAY);
    assert!(
        text.contains("`integer[4]` has no method `non_existent_method`"),
        "{text}"
    );
    assert!(
        text.contains("cannot initialize Bool with integer[4]"),
        "{text}"
    );
}

const READ_WIDTH: &str = r#"module test_index_read_width;

use std::bits::unsigned;

struct Mem {
    pub data: unsigned[8],
}

impl Index<integer, unsigned[8]> for Mem {
    fn index(index: integer) -> unsigned[8] {
        return self.data;
    }
}

#[test] entity T {}
impl T {
    process {
        let mem: Mem = Mem { .data = 42 };
        // mem[0] returns unsigned[8], but assigned to unsigned[16]; passes Stage 4!
        let x: unsigned[16] = mem[0];
        print!("x={}\n", x);
    }
}
"#;

const WRITE_WIDTH: &str = r#"module test_index_assign_width;

use std::bits::unsigned;

struct Mem {
    pub data: unsigned[8],
}

impl IndexAssign<integer, unsigned[8]> for Mem {
    fn index_assign(index: integer, val: unsigned[8]) {
        self.data = val;
    }
}

#[test] entity T {}
impl T {
    process {
        let mem: Mem = Mem { .data = 0 };
        // IndexAssign expects unsigned[8], but wide is unsigned[16]; passes Stage 4!
        let wide: unsigned[16] = 0x1234;
        mem[0] = wide;
        print!("mem.data={}\n", mem.data);
    }
}
"#;

const ELEMENT_ARRAY: &str = r#"module test_idx_arr;

struct Container {
    pub data0: integer[4],
}

impl Index<integer, integer[4]> for Container {
    fn index(index: integer) -> integer[4] {
        return self.data0;
    }
}

impl IndexAssign<integer, integer[4]> for Container {
    fn index_assign(index: integer, value: integer[4]) {
        self.data0 = value;
    }
}

#[test]
entity TB {}
impl TB {
    process {
        let c: Container = Container { .data0 = [1, 2, 3, 4] };

        // 1. Index read evaluates to Ty::Error, allowing non-existent method call and invalid type assignment:
        let x: Bool = c[0].non_existent_method();
        let b: Bool = c[0];

        // 2. IndexAssign matches any type:
        c[0] = true;
    }
}
"#;
