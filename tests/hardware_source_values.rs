//! Source hardware function locals retain canonical identities. Std-defined
//! float arithmetic must compile as hardware without tree growth, even when
//! caller ports shadow the library's integer local names.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[cfg(feature = "llvm")]
#[test]
fn procedure_arguments_share_foreign_values_without_caller_name_capture() {
    let source = "module procedure_sharing;\n\
        extern \"C\" { fn labs(value: integer) -> integer; }\n\
        struct Pair { pub first: integer, pub second: integer }\n\
        fn store(pair: Pair, value: integer) { pair.first = value; pair.second = value; }\n\
        fn forward(pair: Pair, value: integer) { store(pair, value); }\n\
        entity E { a: integer in, first: integer out, second: integer out }\n\
        impl E { let pair: Pair; forward(pair, labs(a)); first = pair.first; second = pair.second; }\n";
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/procedure_sharing.siox", source),
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
    let mut calls = 0;
    let mut maximum = 0;
    let mut function = "";
    for line in llvm.lines() {
        if line.starts_with("define ") {
            calls = 0;
            function = line;
        }
        if line.contains("call i64 @labs(") {
            calls += 1;
        }
        if line == "}" {
            assert!(
                calls <= 1,
                "{function}: emitted {calls} foreign argument calls"
            );
            maximum = maximum.max(calls);
        }
    }
    assert_eq!(maximum, 1, "foreign argument was not exercised");
}

#[test]
fn complete_aggregate_stores_cover_all_leaves_without_hiding_partial_latches() {
    for (alternative, expected_latch) in [("else { y = [3, 4]; }", false), ("", true)] {
        let source = format!("module aggregate_coverage;\nuse std::bits::unsigned;\nentity E {{ c: Bool in, y: unsigned[8][2] out }}\nimpl E {{ if c {{ y = [1, 2]; }} {alternative} }}\n");
        let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
            CompileRequest::new(
                SourceInput::memory("/virtual/aggregate_coverage.siox", source),
                Emit::Metadata,
            ),
        );
        assert!(
            compilation.succeeded(),
            "{}",
            compilation.render_diagnostics()
        );
        assert_eq!(
            compilation
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code == Some(siox::diag::codes::POSSIBLE_LATCH)),
            expected_latch,
            "{}",
            compilation.render_diagnostics()
        );
    }
}

#[test]
fn hardware_float_inline_retains_shared_values_and_lexical_types() {
    let dir = std::env::temp_dir().join(format!("siox_inline_budget_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.siox");
    std::fs::write(
        &entry,
        "module main;\nuse std::float::float;\n\
         entity E { a: float<32, 23> in, b: float<32, 23> in, s: float<32, 23> out }\n\
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
fn repeated_block_local_updates_keep_linear_graphs_and_declared_widths() {
    let mut source = "module updates;\nuse std::bits::unsigned;\nentity E { a: unsigned[8] in, s: unsigned[8] out }\nimpl E { if a != 0 { let x: unsigned[8] = a;\n".to_owned();
    for _ in 0..1_000 {
        source.push_str("x = x + x;\n");
    }
    source.push_str("s = x; } else { s = 0; } }\n");
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/updates.siox", source),
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
        design.process_ir.values.len() < 15_000,
        "{} values",
        design.process_ir.values.len()
    );
    let local_boundaries = design
        .process_ir
        .values
        .iter()
        .enumerate()
        .filter(|(_, value)| {
            matches!(value.kind, siox::ir::ProcessValueKind::RawResize { .. })
                && value.bit_width == Some(8)
        })
        .collect::<Vec<_>>();
    assert!(
        local_boundaries.len() >= 1_001,
        "each update must retain the local format"
    );
    for (index, _) in local_boundaries {
        assert!(
            matches!(design.process_ir.value_layouts[index].as_ref().map(|layout| &layout.kind),
            Some(siox::ir::LayoutKind::Packed { width: 8, family, .. }) if family.ends_with("unsigned"))
        );
    }
}

#[test]
fn nested_calls_share_repeated_arguments_without_intermediate_lets() {
    let mut expression = "a".to_owned();
    for _ in 0..12 {
        expression = format!("double({expression})");
    }
    let source = format!("module nested;\nfn double(x: integer) -> integer {{ return x + x; }}\nentity E {{ a: integer in, s: integer out }}\nimpl E {{ s = {expression}; }}\n");
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/nested.siox", source),
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
        design.process_ir.values.len() < 100,
        "{} values",
        design.process_ir.values.len()
    );
}

#[cfg(feature = "llvm")]
#[test]
fn repeated_static_array_parameter_reads_do_not_duplicate_foreign_argument_calls() {
    let source = "module shared_array;\nextern \"C\" { fn labs(x: integer) -> integer; }\nfn twice(v: integer[2]) -> integer { return v[0] + v[0]; }\nentity E { a: integer in, s: integer out }\nimpl E { s = twice([labs(a), 0]); }\n";
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/shared_array.siox", source),
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
    let mut calls = 0;
    let mut maximum = 0;
    for line in llvm.lines() {
        if line.starts_with("define ") {
            calls = 0;
        }
        if line.contains("call i64 @labs(") {
            calls += 1;
        }
        if line == "}" {
            assert!(
                calls <= 1,
                "one entry duplicated the argument call {calls} times"
            );
            maximum = maximum.max(calls);
        }
    }
    assert_eq!(maximum, 1, "foreign array argument was not exercised");
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
