#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

fn run(name: &str, body: &str) {
    let dir = std::env::temp_dir().join(format!("siox-enum-owner-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("test");
    let source = format!(
        r#"module enum_owner_api;
        enum Mode {{ Low = 9, High = 17 }}
        type Alias = Mode;
        const HIGH: Alias = Alias::High;
        impl Mode {{
            pub fn new(value: integer) -> Self {{
                if value == 0 {{ return Mode::Low; }}
                return Mode::High;
            }}
            pub fn forwarded(value: integer) -> Self {{ return Self::new(value); }}
        }}
        enum Zero {{ Low = 23, High = 31 }}
        impl Zero {{ pub fn new() -> Self {{ return Zero::High; }} }}
        entity Dut {{ value: integer out }}
        impl Dut {{ value = integer(Zero::new()); }}
        #[test] entity Probe {{}}
        impl Probe {{ let dut: Dut = {{}}; process {{ {body} }} }}
    "#
    );
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("input.siox"), source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compilation.succeeded(),
        "{}\n{:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    let output = std::process::Command::new(&binary).output().unwrap();
    assert!(
        output.status.success(),
        "{name}: {}\n{}\nretained artifact: {}\nProcess IR: {:#?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        binary.display(),
        compilation.design.as_ref().unwrap().process_ir
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn direct_alias_constructor_runs() {
    run(
        "direct",
        "let value: Alias = Alias::new(0); assert!(integer(value) == 9);",
    );
}

#[test]
fn forwarded_alias_constructor_runs() {
    run(
        "forwarded",
        "let value: Alias = Alias::forwarded(1); assert!(integer(value) == 17);",
    );
}

#[test]
fn explicit_zero_argument_constructor_overrides_default() {
    run(
        "zero",
        "let value: Zero = Zero::new(); assert!(integer(value) == 31);",
    );
}

#[test]
fn alias_qualified_constant_keeps_its_enum_value() {
    run("constant", "assert!(integer(HIGH) == 17);");
}

#[test]
fn hardware_zero_argument_constructor_overrides_default() {
    run("hardware", "await 1ns; assert!(dut.value == 31);");
}
