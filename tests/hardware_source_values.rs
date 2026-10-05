//! Source hardware function locals retain canonical identities. Std-defined
//! float arithmetic must compile as hardware without tree growth, even when
//! caller ports shadow the library's integer local names.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn concatenation_parts_keep_their_source_anchors() {
    let source = "module concat_anchors; use std::bits::unsigned;\n\
        entity Dut { a: unsigned[4] in, b: unsigned[4] in, y: unsigned[8] out }\n\
        impl Dut { y = {a, b}; }\n";
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/concat_anchors.siox", source),
            Emit::Metadata,
        ));
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let design = compilation.design.unwrap();
    let mut parts = std::collections::HashSet::new();
    for node in &design.process_ir.values {
        let siox::ir::ProcessValueKind::Signal { signals, .. } = &node.kind else {
            continue;
        };
        if signals.len() != 1 {
            continue;
        }
        let signal = &design.signals[signals[0].0 as usize];
        let Some(part) = ["a", "b"]
            .into_iter()
            .find(|part| signal.path.ends_with(&format!(".{part}")))
        else {
            continue;
        };
        let text = &source[node.span.start as usize..node.span.end as usize];
        if text == part {
            parts.insert(part);
        }
    }
    assert_eq!(
        parts.len(),
        2,
        "both concat operands retain their own spans"
    );
    assert!(
        design.validate().is_empty(),
        "canonical design must validate"
    );
}

#[test]
fn if_expression_operands_keep_their_own_source_spans() {
    let source = "module if_anchors; use std::bits::unsigned;\n\
        entity Dut { flag: Bit in, y: unsigned[4] out }\n\
        impl Dut { y = if flag { 3 } else { 7 }; }\n";
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/if_anchors.siox", source),
            Emit::Metadata,
        ));
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let design = compilation.design.unwrap();
    let mut checked = false;
    for node in &design.process_ir.values {
        let siox::ir::ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        } = node.kind
        else {
            continue;
        };
        let then_node = &design.process_ir.values[then_value.0 as usize];
        let else_node = &design.process_ir.values[else_value.0 as usize];
        if !matches!(&then_node.kind, siox::ir::ProcessValueKind::Number(siox::ir::ProcessNumber::Integer(words)) if words == &[3])
            || !matches!(&else_node.kind, siox::ir::ProcessValueKind::Number(siox::ir::ProcessNumber::Integer(words)) if words == &[7])
        {
            continue;
        }
        for (id, expected) in [(condition, "flag"), (then_value, "3"), (else_value, "7")] {
            let span = design.process_ir.values[id.0 as usize].span;
            assert_eq!(&source[span.start as usize..span.end as usize], expected);
        }
        checked = true;
    }
    assert!(checked, "the source conditional must be exercised");
}

#[test]
fn negative_packed_read_offsets_keep_integer_evaluation_boundaries() {
    let source = "module negative_read_boundaries; use std::bits::unsigned;\n\
        entity Dut { a: unsigned[63..-64] in, index: integer in,\n\
          ascending: Logic out, descending: Logic out }\n\
        impl Dut { let reverse: unsigned[-64..63]; reverse = a;\n\
          ascending = reverse[index]; descending = a[index]; }\n";
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/negative_read_boundaries.siox", source),
            Emit::Metadata,
        ));
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let design = compilation.design.unwrap();
    let mut shifts = 0;
    for node in &design.process_ir.values {
        if let siox::ir::ProcessValueKind::Binary {
            operation: siox::ir::ProcessBinaryOp::Shr,
            right,
            ..
        } = node.kind
        {
            if node.bit_width != Some(128) {
                continue;
            }
            let offset = &design.process_ir.values[right.0 as usize];
            assert!(
                matches!(offset.kind, siox::ir::ProcessValueKind::RawResize { .. }),
                "wide shift must not widen index arithmetic: {offset:?}"
            );
            assert_eq!(offset.ty, Some(siox::types::Ty::Integer));
            assert_eq!(offset.bit_width, Some(64));
            shifts += 1;
        }
    }
    assert_eq!(shifts, 2, "both directed reads must exercise the boundary");
}

#[test]
fn source_selection_nodes_retain_access_and_index_spans() {
    let source = "module selection_anchors; use std::bits::unsigned;\n\
        entity Dut { a: unsigned[16] in, index: integer in,\n\
          y: unsigned[4] out, element: Logic out }\n\
        impl Dut { y = a[11..8]; element = a[index]; }\n";
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/selection_anchors.siox", source),
            Emit::Metadata,
        ));
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let design = compilation.design.unwrap();
    let mut static_slice = false;
    let mut checked_index = false;
    for node in &design.process_ir.values {
        match node.kind {
            siox::ir::ProcessValueKind::BitSlice {
                high: 11, low: 8, ..
            } => {
                let text = &source[node.span.start as usize..node.span.end as usize];
                assert!(
                    matches!(text, "a[11..8]" | "11..8"),
                    "coarse selection anchor: {text:?}"
                );
                static_slice = true;
            }
            siox::ir::ProcessValueKind::CheckedIndex {
                span,
                left: 0,
                right: 15,
                ..
            } => {
                assert_eq!(node.span, span);
                assert_eq!(&source[span.start as usize..span.end as usize], "index");
                checked_index = true;
            }
            _ => {}
        }
    }
    assert!(
        static_slice && checked_index,
        "both selection paths must be exercised"
    );
}

#[test]
fn packed_runtime_writes_have_one_update_per_storage_plane() {
    let source = "module compact_writes; use std::bits::unsigned;\n\
        entity Dut { clk: Bit in, index: integer in, data: Logic in, q: Logic out }\n\
        impl Dut { let word: unsigned[128] = 0;\n\
          if clk.rising() { word[index] = data; } q = word[index]; }\n";
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/compact_writes.siox", source),
            Emit::Metadata,
        ));
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let design = compilation.design.unwrap();
    let updates: Vec<_> = design
        .event_blocks
        .iter()
        .flat_map(|block| &block.updates)
        .collect();
    assert_eq!(
        updates.len(),
        2,
        "one value update and one companion update"
    );
    let mut widths: Vec<_> = updates
        .iter()
        .map(|update| design.signals[update.target.0 as usize].width)
        .collect();
    widths.sort_unstable();
    assert_eq!(widths, [128, 512]);
}

#[cfg(feature = "llvm")]
#[test]
fn array_operators_share_returned_foreign_values_instead_of_expanding_caller_syntax() {
    let source = "module operator_sharing;\n\
        use std::bits::unsigned;\n\
        extern \"C\" { fn labs(value: integer) -> integer; }\n\
        fn supply(a: integer) -> unsigned[64][-1..0] {\n\
          let value: unsigned[64] = unsigned[64](labs(a)); return [value, value]; }\n\
        entity E { a: integer in, b: unsigned[64][7..6] in, y: unsigned[64][2..3] out }\n\
        impl E { y = supply(a) and b; }\n";
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/operator_sharing.siox", source),
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
    let mut maximum = 0;
    for definition in llvm.split("define ").skip(1) {
        let body = definition.split("\n}").next().unwrap();
        let calls = body.matches("call i64 @labs(").count();
        assert!(
            calls <= 1,
            "one entry duplicated the returned operand {calls} times:\n{body}"
        );
        maximum = maximum.max(calls);
    }
    assert_eq!(maximum, 1, "returned foreign operand was not exercised");
}

#[test]
fn clean_connected_test_arrays_reserve_runtime_planes_before_hardware_lowering() {
    let source = "module connected_planes;\nuse std::bits::unsigned;\n\
        entity Dut { a: unsigned[4][2] in, b: Bit in, y: unsigned[4][2] out }\n\
        impl Dut { y = not a; }\n\
        impl Dut { y = [\"ZZZZ\", \"ZZZZ\"]; }\n\
        #[test] entity Test {}\n\
        impl Test { let a: unsigned[4][2] = [0, 0]; let b: Bit = '0';\n\
          let dut: Dut = { .a = a, .b = b }; }\n";
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/connected_planes.siox", source),
            Emit::Metadata,
        ));
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let design = compilation.design.unwrap();
    for suffix in ["dut.a[0]", "dut.a[1]", "dut.y[0]", "dut.y[1]"] {
        let id = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(suffix))
            .unwrap();
        let companion = *design
            .meta_of
            .get(&(id as u32))
            .unwrap_or_else(|| panic!("missing runtime plane for {suffix}"));
        assert_eq!(design.signal_width(siox::ir::SignalId(companion)), Some(16));
        if suffix.contains(".y[") {
            let input = design
                .signals
                .iter()
                .position(|signal| signal.path.ends_with(&suffix.replace(".y[", ".a[")))
                .unwrap();
            let input_plane = design.meta_of[&(input as u32)];
            let mut pending = vec![siox::ir::SignalId(companion)];
            let mut seen = std::collections::HashSet::new();
            while let Some(signal) = pending.pop() {
                if !seen.insert(signal) {
                    continue;
                }
                for driver in design
                    .drivers
                    .iter()
                    .filter(|driver| driver.target == signal)
                {
                    let siox::ir::Expr::Canonical { reads, .. } = &driver.expr else {
                        panic!("derived driver must retain canonical reads");
                    };
                    pending.extend(reads.iter().copied());
                }
            }
            assert!(
                seen.contains(&siox::ir::SignalId(input_plane)),
                "resolved {suffix} froze the binary reset value before reserving its input plane"
            );
        }
    }
    let bit = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with("dut.b"))
        .unwrap();
    assert!(
        !design.meta_of.contains_key(&(bit as u32)),
        "scalar enum discriminants do not need companion planes"
    );
    assert!(design.validate().is_empty(), "{:?}", design.validate());
}

#[test]
fn packed_constant_array_and_struct_leaves_retain_literal_planes() {
    let source = "module literal_planes;\nuse std::bits::unsigned;\n\
        struct Record { pub bits: unsigned[4] }\n\
        entity E { y: unsigned[4] out }\n\
        impl E { let a: unsigned[4][-1..0] = [\"1XZ0\", \"0ZX1\"];\n\
          let r: Record = Record { .bits = \"1XZ0\" }; y = a[-1]; }\n";
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/literal_planes.siox", source),
            Emit::Metadata,
        ));
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let design = compilation.design.unwrap();
    let mut companions = Vec::new();
    for suffix in ["a[-1]", "a[0]", "r.bits"] {
        let id = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(suffix))
            .unwrap();
        let companion = *design
            .meta_of
            .get(&(id as u32))
            .unwrap_or_else(|| panic!("missing literal plane for {suffix}"));
        companions.push(design.signals[companion as usize].init.clone());
    }
    assert_eq!(
        companions[0], companions[2],
        "scalar and aggregate literals disagree"
    );
    assert_ne!(
        companions[0], companions[1],
        "directed literal positions were lost"
    );
    assert!(design.validate().is_empty(), "{:?}", design.validate());
}

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
