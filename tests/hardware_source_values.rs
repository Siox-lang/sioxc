//! Source hardware function locals retain canonical identities. Std-defined
//! float arithmetic must compile as hardware without tree growth, even when
//! caller ports shadow the library's integer local names.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn hardware_float_inline_retains_shared_values_and_lexical_types() {
    let dir = std::env::temp_dir().join(format!("siox_inline_budget_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.siox");
    std::fs::write(
        &entry,
        "module main;\nuse std::float::float;\n\
         entity E { a: float[8..-23] in, b: float[8..-23] in, s: float[8..-23] out }\n\
         impl E { s = a * b; }\n",
    )
    .unwrap();
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(SourceInput::path(&entry), Emit::Metadata),
    );
    let _ = std::fs::remove_dir_all(dir);
    let rendered = compilation.render_diagnostics();
    assert!(compilation.succeeded(), "{rendered}");
    let design = compilation.design.unwrap();
    assert!(design.validate().is_empty(), "{:?}", design.validate());
    assert!(
        design.process_ir.values.len() < 2_000,
        "hardware multiply expanded to {} values",
        design.process_ir.values.len()
    );
    assert!(design
        .drivers
        .iter()
        .all(|write| matches!(write.expr, siox::ir::Expr::Canonical { .. })));
}

fn alias_source(count: usize) -> String {
    let mut source = "module aliases;\nfn advance(a: integer) -> integer {\n".to_owned();
    source.push_str("let x0: integer = a;\n");
    for index in 1..=count {
        source.push_str(&format!("let x{index}: integer = x{} + 1;\n", index - 1));
    }
    source.push_str(&format!("return x{count}; }}\n"));
    source.push_str("entity E { a: integer in, s: integer out }\nimpl E { s = advance(a); }\n");
    source
}

#[test]
fn long_source_alias_chains_are_linear_and_do_not_recurse_per_statement() {
    let source = alias_source(2_000);
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/aliases.siox", source),
            Emit::Metadata,
        ));
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let design = compilation.design.unwrap();
    assert!(design.validate().is_empty(), "{:?}", design.validate());
    assert!(
        design.process_ir.values.len() < 10_000,
        "{} values",
        design.process_ir.values.len()
    );
}

#[cfg(feature = "llvm")]
#[test]
fn typed_aliases_do_not_recompute_arithmetic_at_each_consumers_width() {
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/aliases.siox", alias_source(30)),
            Emit::LlvmIr,
        ));
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let Some(siox::compiler::Artifact::Text(llvm)) = compilation.artifact else {
        panic!("LLVM IR expected");
    };
    // Both the derived hardware helper and canonical CFG entry may appear in
    // the object. Each must evaluate an alias once, not recursively at every
    // ancestor's wider signed-operation format.
    let mut additions = 0;
    let mut maximum = 0;
    for line in llvm.lines() {
        if line.starts_with("define ") {
            additions = 0;
        }
        if line.contains(" = add i") && line.contains("pv.add") {
            additions += 1;
        }
        if line == "}" {
            assert!(additions <= 30, "one entry emitted {additions} additions");
            maximum = maximum.max(additions);
        }
    }
    assert_eq!(maximum, 30, "shared source arithmetic was not exercised");
}
