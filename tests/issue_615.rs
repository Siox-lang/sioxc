//! Regression for sioxc #615: an impl's method takes the parameter types and
//! returns the type its trait declares. Only arity was compared, so a call
//! through a trait bound type-checked against the trait and lowered the
//! impl's mismatched body, failing in lowering or at run time. core's hook
//! traits (`From`, `Suffix`) are read by the compiler and stay unchecked.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn a_trait_impl_method_keeps_its_signature() {
    for (name, source) in [("hardware", HARDWARE), ("testbench", TESTBENCH)] {
        let path = std::env::temp_dir().join(format!("siox_i615_{name}.siox"));
        let text = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
            .compile(CompileRequest::new(
                SourceInput::memory(path, source),
                Emit::Metadata,
            ))
            .render_diagnostics();
        assert!(text.contains("`hello` takes"), "{name}: {text}");
        assert!(
            text.contains("`hello` returns `integer` in `Greet`, not `real`"),
            "{name}: {text}"
        );
    }
}

const HARDWARE: &str = r#"module test_trait_signature_hw;

trait Greet {
    fn hello(name: string) -> integer;
}

struct Person {
    pub id: integer,
}

impl Greet for Person {
    fn hello(name: Bool) -> real {
        return 3.14;
    }
}

fn call_greet<T: Greet>(item: T, x: string) -> integer {
    return T::hello(x);
}

entity Top {
    out_val: integer out,
}

impl Top {
    let p: Person = Person { .id = 1 };
    out_val = call_greet(p, "Alice");
}
"#;

const TESTBENCH: &str = r#"module test_trait_signature_tb;

trait Greet {
    fn hello(name: string) -> integer;
}

struct Person {
    pub id: integer,
}

impl Greet for Person {
    fn hello(name: Bool) -> real {
        return 3.14;
    }
}

fn call_greet<T: Greet>(item: T, x: string) -> integer {
    return T::hello(x);
}

#[test]
entity TestTop {}

impl TestTop {
    p: process {
        let p: Person = Person { .id = 1 };
        let ans: integer = call_greet(p, "Alice");
        print!("ans = {}\n", ans);
    }
}
"#;
