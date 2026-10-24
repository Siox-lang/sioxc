use super::*;

#[test]
#[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
fn string_return_boundary_preserves_array_types_and_payload_widths() {
    use siox::ir::{ProcessValueInline, ScalarDomain};

    for (literal, fixed, width) in [
        (true, false, 32),
        (true, true, 32),
        (false, false, 64),
        (false, false, 193),
    ] {
        let context = Context::create();
        let module = context.create_module("string_return");
        let builder = context.create_builder();
        let mut sources = siox::diag::SourceMap::new();
        let text = if fixed {
            "fn echo(value: Char[1]) -> Char[1] { return value; }"
        } else {
            "fn echo(value: string) -> string { return value; }"
        };
        let file = sources.add("string_return.siox", text);
        let span = Span::new(file, 0..text.len() as u32);
        let layout = SourceLayout {
            span,
            kind: LayoutKind::Array {
                range: fixed.then_some(LayoutRange { left: 0, right: 0 }),
                element: Box::new(SourceLayout {
                    span,
                    kind: LayoutKind::Scalar {
                        width: 32,
                        domain: ScalarDomain::Character,
                        nominal: Some("Char".into()),
                        value_range: None,
                    },
                }),
            },
        };
        let source = ProcessValueInline {
            name: "echo".into(),
            function: span,
            call: span,
            result_span: span,
            result_layout: (fixed || !literal).then(|| layout.clone()),
            aggregate: None,
            branches: Vec::new(),
            parameters: Vec::new(),
            locals: Vec::new(),
        };
        let design = Design {
            process_ir: ProcessIr {
                values: vec![ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(if literal { 32 } else { 64 }),
                    kind: if literal {
                        ProcessValueKind::String("μ".into())
                    } else {
                        ProcessValueKind::Parameter {
                            function: siox::ir::ProcessFunctionId(0),
                            index: 0,
                        }
                    },
                }],
                value_layouts: vec![Some(layout)],
                ..ProcessIr::default()
            },
            ..Design::default()
        };
        let debug = DebugInfo::new(&context, &module, &sources, &design).unwrap();
        let result = debug::source_return_type(
            &debug,
            &module,
            &design,
            &source,
            ProcessValueId(0),
            width,
            Some(7),
        )
        .unwrap();
        let bits = context
            .custom_width_int_type(std::num::NonZeroU32::new(width).unwrap())
            .unwrap();
        if fixed {
            let debug::SourceReturn::Direct(ty) = &result else {
                panic!("fixed Char[1] must not acquire a dynamic string view");
            };
            assert_eq!(*ty, bits.into());
        } else {
            let debug::SourceReturn::Memory { storage, .. } = &result else {
                panic!("unbounded string return needs its borrowed source view");
            };
            assert_eq!(
                storage
                    .get_field_type_at_index(0)
                    .unwrap()
                    .into_array_type()
                    .len(),
                32
            );
            assert_eq!(storage.get_field_type_at_index(1).unwrap(), bits.into());
        }
        let native = module.add_function("echo", result.fn_type(&context, &[bits.into()]), None);
        result.declare(&context, native, Some(&debug));
        builder.position_at_end(context.append_basic_block(native, "entry"));
        let value = native.get_last_param().unwrap().into_int_value();
        result.emit_return(Some(&debug), &builder, value).unwrap();
        let caller = module.add_function("caller", bits.fn_type(&[bits.into()], false), None);
        builder.position_at_end(context.append_basic_block(caller, "entry"));
        let value = debug::call_return(
            &context,
            &builder,
            native,
            &[caller.get_first_param().unwrap().into()],
            "returned",
            Some(&debug),
        )
        .unwrap();
        assert_eq!(value.get_type(), bits);
        builder.build_return(Some(&value)).unwrap();
        assert_eq!(
            module.get_function("sx_runtime_debug_string").is_some(),
            !literal
        );
        if width == 193 {
            assert!(module.print_to_string().to_string().contains("trunc i193"));
        }
        debug.finish(&module, &design);
        module.verify().unwrap();
    }
}

#[test]
#[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
fn shared_return_boundary_preserves_canonical_bits() {
    use inkwell::attributes::{Attribute, AttributeLoc};
    use siox::ir::{
        LayoutField, ProcessCall, ProcessFunction, ProcessFunctionId, ProcessValueInline,
        ScalarDomain,
    };

    for (fields, signature) in [
        (
            vec![(ScalarDomain::Integer, 3), (ScalarDomain::Integer, 3)],
            Some("i16"),
        ),
        (
            vec![
                (ScalarDomain::Real, 64),
                (ScalarDomain::Integer, 3),
                (ScalarDomain::Integer, 3),
            ],
            Some("{ double, i16 }"),
        ),
        (
            vec![(ScalarDomain::Real, 64), (ScalarDomain::Integer, 64)],
            Some("{ double, i64 }"),
        ),
        (
            vec![(ScalarDomain::Integer, 64), (ScalarDomain::Real, 64)],
            Some("{ i64, double }"),
        ),
        (
            vec![(ScalarDomain::Real, 64), (ScalarDomain::Real, 64)],
            Some("{ double, double }"),
        ),
        (
            vec![(ScalarDomain::Character, 32), (ScalarDomain::Real, 64)],
            None,
        ),
        (
            vec![(ScalarDomain::Real, 64), (ScalarDomain::Character, 32)],
            Some("{ double, i32 }"),
        ),
        (
            vec![(ScalarDomain::Real, 64), (ScalarDomain::Integer, 8)],
            Some("{ double, i8 }"),
        ),
        (
            vec![(ScalarDomain::Integer, 8), (ScalarDomain::Real, 64)],
            None,
        ),
        (
            vec![(ScalarDomain::Real, 64), (ScalarDomain::Integer, 16)],
            Some("{ double, i16 }"),
        ),
        (
            vec![(ScalarDomain::Integer, 16), (ScalarDomain::Real, 64)],
            None,
        ),
        (
            vec![(ScalarDomain::Integer, 3), (ScalarDomain::Real, 64)],
            None,
        ),
        (
            vec![
                (ScalarDomain::Real, 64),
                (ScalarDomain::Integer, 32),
                (ScalarDomain::Integer, 16),
            ],
            Some("{ double, i48 }"),
        ),
        (
            vec![
                (ScalarDomain::Real, 64),
                (ScalarDomain::Integer, 8),
                (ScalarDomain::Integer, 8),
            ],
            Some("{ double, i16 }"),
        ),
        (
            vec![
                (ScalarDomain::Integer, 64),
                (ScalarDomain::Real, 64),
                (ScalarDomain::Integer, 64),
            ],
            None,
        ),
        (
            vec![(ScalarDomain::Integer, 81), (ScalarDomain::Real, 64)],
            None,
        ),
    ] {
        let context = Context::create();
        let module = context.create_module("shared_return");
        let builder = context.create_builder();
        let mut sources = siox::diag::SourceMap::new();
        let text = "fn identity(value: Pair) -> Pair { return value; }";
        let file = sources.add("shared_return.siox", text);
        let span = Span::new(file, 0..text.len() as u32);
        let width = fields.iter().map(|(_, width)| width).sum::<u32>();
        let bytes = fields
            .iter()
            .map(|(_, width)| width.div_ceil(8))
            .sum::<u32>();
        let layout = SourceLayout {
            span,
            kind: LayoutKind::Struct {
                name: "Pair".into(),
                view: None,
                fields: fields
                    .into_iter()
                    .enumerate()
                    .map(|(index, (domain, width))| LayoutField {
                        name: format!("field{index}"),
                        direction: None,
                        layout: SourceLayout {
                            span,
                            kind: LayoutKind::Scalar {
                                width,
                                domain,
                                nominal: None,
                                value_range: None,
                            },
                        },
                    })
                    .collect(),
            },
        };
        let source = ProcessValueInline {
            name: "identity".into(),
            function: span,
            call: span,
            result_span: span,
            result_layout: Some(layout),
            aggregate: None,
            branches: Vec::new(),
            parameters: Vec::new(),
            locals: Vec::new(),
        };
        let node = |kind| ProcessValue {
            span,
            ty: None,
            bit_width: Some(width),
            kind,
        };
        let call = ProcessCall {
            value: ProcessValueId(1),
            function: ProcessFunctionId(0),
            arguments: vec![ProcessValueId(0)],
        };
        let design = Design {
            process_ir: ProcessIr {
                values: vec![
                    node(ProcessValueKind::Parameter {
                        function: ProcessFunctionId(0),
                        index: 0,
                    }),
                    node(ProcessValueKind::InlineCall {
                        operand: ProcessValueId(0),
                        source: Box::new(source),
                    }),
                ],
                functions: vec![ProcessFunction {
                    id: call.function,
                    name: "identity".into(),
                    span,
                    parameters: vec![ProcessValueId(0)],
                    result: ProcessValueId(1),
                }],
                ..ProcessIr::default()
            },
            ..Design::default()
        };
        let debug = DebugInfo::new(&context, &module, &sources, &design).unwrap();
        let bits = context
            .custom_width_int_type(std::num::NonZeroU32::new(width).unwrap())
            .unwrap();
        let probe = module.add_function("probe", bits.fn_type(&[bits.into()], false), None);
        builder.position_at_end(context.append_basic_block(probe, "entry"));
        let mut cache = ProcessValueCache::new(&[false; 2], &[true; 2]);
        cache.debug = Some(&debug);
        cache.parameters.insert(
            ProcessValueId(0),
            probe.get_first_param().unwrap().into_int_value(),
        );
        let returned = shared_call(
            &context,
            &module,
            &builder,
            &design,
            &call,
            None,
            &HashMap::new(),
            &mut cache,
        )
        .unwrap();
        assert_eq!(returned.get_type(), bits);
        builder.build_return(Some(&returned)).unwrap();
        debug.finish(&module, &design);
        module.verify().unwrap();
        let native = module.get_function("sx.fn.0.identity").unwrap();
        if let Some(signature) = signature {
            assert_eq!(
                native
                    .get_type()
                    .get_return_type()
                    .unwrap()
                    .print_to_string()
                    .to_string(),
                signature
            );
            assert_eq!(native.count_params(), 1);
        } else {
            assert!(native.get_type().get_return_type().is_none());
            assert_eq!(native.count_params(), 2);
            let attribute = native
                .get_enum_attribute(
                    AttributeLoc::Param(0),
                    Attribute::get_named_enum_kind_id("sret"),
                )
                .unwrap();
            let storage = attribute.get_type_value().into_struct_type();
            assert_eq!(
                storage
                    .get_field_type_at_index(0)
                    .unwrap()
                    .into_array_type()
                    .len(),
                bytes
            );
            assert_eq!(
                storage
                    .get_field_type_at_index(1)
                    .unwrap()
                    .into_int_type()
                    .get_bit_width(),
                width
            );
            let entry = probe.get_first_basic_block().unwrap();
            assert_eq!(
                entry.get_first_instruction().unwrap().get_opcode(),
                inkwell::values::InstructionOpcode::Alloca
            );
            let text = module.print_to_string().to_string();
            assert!(
                text.contains("call void @sx.fn.0.identity(ptr sret("),
                "{text}"
            );
        }
        assert_eq!(
            module
                .get_functions()
                .filter(|function| function.get_name().to_bytes().starts_with(b"sx.debug.call"))
                .count(),
            0,
            "shared bodies must not acquire a second execution wrapper"
        );
    }
}

#[test]
fn shared_debug_calls_emit_one_body_per_consumer_format() {
    let context = Context::create();
    let module = context.create_module("shared_debug_calls");
    let builder = context.create_builder();
    let mut sources = siox::diag::SourceMap::new();
    let text = "fn bump(value: integer, unused: integer) -> integer {\n let local = value + 1;\n return local;\n}\n";
    let file = sources.add("shared.siox", text);
    let span = Span::new(file, 0..text.len() as u32);
    let source = siox::ir::ProcessValueInline {
        name: "bump".into(),
        function: span,
        call: span,
        result_span: span,
        result_layout: None,
        aggregate: None,
        branches: Vec::new(),
        parameters: vec![
            siox::ir::ProcessValueBinding {
                name: "value".into(),
                span,
                scope: None,
                layout: None,
                value: Some(ProcessValueId(0)),
                fields: Vec::new(),
            },
            siox::ir::ProcessValueBinding {
                name: "unused".into(),
                span,
                scope: None,
                layout: None,
                value: Some(ProcessValueId(1)),
                fields: Vec::new(),
            },
            siox::ir::ProcessValueBinding {
                name: "aggregate".into(),
                span,
                scope: None,
                layout: Some(siox::ir::SourceLayout {
                    span,
                    kind: siox::ir::LayoutKind::Struct {
                        name: "Bundle".into(),
                        view: None,
                        fields: ["used", "unused"]
                            .into_iter()
                            .map(|name| siox::ir::LayoutField {
                                name: name.into(),
                                direction: None,
                                layout: siox::ir::SourceLayout {
                                    span,
                                    kind: siox::ir::LayoutKind::Scalar {
                                        width: 64,
                                        domain: siox::ir::ScalarDomain::Integer,
                                        nominal: None,
                                        value_range: None,
                                    },
                                },
                            })
                            .collect(),
                    },
                }),
                value: None,
                fields: vec![
                    ("used".into(), Some(ProcessValueId(0))),
                    ("unused".into(), Some(ProcessValueId(1))),
                ],
            },
        ],
        locals: vec![siox::ir::ProcessValueBinding {
            name: "local".into(),
            span,
            scope: None,
            layout: None,
            value: Some(ProcessValueId(3)),
            fields: Vec::new(),
        }],
    };
    let node = |kind| ProcessValue {
        span,
        ty: Some(siox::types::Ty::Integer),
        bit_width: Some(64),
        kind,
    };
    let design = Design {
        process_ir: ProcessIr {
            values: vec![
                node(ProcessValueKind::Unary {
                    operation: ProcessUnaryOp::Neg,
                    operand: ProcessValueId(8),
                }),
                node(ProcessValueKind::ForeignCall {
                    name: "must_not_run".into(),
                    arguments: Vec::new(),
                    float_arguments: Vec::new(),
                    integer_arguments: Vec::new(),
                    float_result: false,
                    integer_result: true,
                    void_result: false,
                }),
                node(ProcessValueKind::Number(ProcessNumber::Integer(vec![1]))),
                node(ProcessValueKind::Binary {
                    operation: ProcessBinaryOp::Add,
                    left: ProcessValueId(0),
                    right: ProcessValueId(7),
                }),
                node(ProcessValueKind::InlineCall {
                    operand: ProcessValueId(3),
                    source: Box::new(source),
                }),
                node(ProcessValueKind::Number(ProcessNumber::Integer(vec![2]))),
                node(ProcessValueKind::Binary {
                    operation: ProcessBinaryOp::Eq,
                    left: ProcessValueId(0),
                    right: ProcessValueId(2),
                }),
                node(ProcessValueKind::Select {
                    condition: ProcessValueId(6),
                    then_value: ProcessValueId(2),
                    else_value: ProcessValueId(5),
                }),
                node(ProcessValueKind::Signal {
                    signals: vec![SignalId(0)],
                    state: ProcessSignalState::Current,
                }),
            ],
            ..ProcessIr::default()
        },
        signals: vec![Signal {
            path: "value".into(),
            declaration_span: span,
            width: 64,
            real: false,
            integer: true,
            char: false,
            range: None,
            init: vec![0],
            enum_type: None,
        }],
        ..Design::default()
    };
    let debug = DebugInfo::new(&context, &module, &sources, &design).unwrap();
    let function = module.add_function(
        "probe",
        context
            .void_type()
            .fn_type(&[context.i64_type().into()], false),
        None,
    );
    builder.position_at_end(context.append_basic_block(function, "entry"));
    let input = function.get_first_param().unwrap().into_int_value();
    let mut cache = ProcessValueCache::new(&[false; 5], &[true; 5]);
    cache.debug = Some(&debug);
    cache.emitted.insert((ProcessValueId(8), None, None), input);
    cache.emitted.insert(
        (ProcessValueId(0), None, None),
        builder.build_int_neg(input, "argument").unwrap(),
    );
    // Different selected consumers must reuse the function definition, never
    // recursively duplicate its body. Width formats remain distinct.
    for width in [64, 128] {
        for threshold in 0..8 {
            let active = builder
                .build_int_compare(
                    IntPredicate::UGT,
                    input,
                    context.i64_type().const_int(threshold, false),
                    "active",
                )
                .unwrap();
            let value = process_value_at(
                &context,
                &module,
                &builder,
                &design,
                ProcessValueId(4),
                width,
                false,
                Some(active),
                &HashMap::new(),
                &mut cache,
            )
            .unwrap();
            assert_eq!(value.get_type().get_bit_width(), width);
        }
    }
    builder.build_return(None).unwrap();
    debug.finish(&module, &design);
    module.verify().unwrap();
    let llvm = module.print_to_string().to_string();
    let signatures = llvm
        .lines()
        .filter(|line| line.starts_with("define internal") && line.contains("@sx.debug.call.4"))
        .collect::<Vec<_>>();
    assert!(
        signatures
            .iter()
            .any(|line| line.contains("i64 %") && line.contains("i65 %")),
        "one body must retain distinct arithmetic/comparison argument formats: {llvm}"
    );
    assert!(
        signatures
            .iter()
            .any(|line| line.contains("i128 %") && line.contains("i65 %")),
        "widened arithmetic must not change the comparison argument format: {llvm}"
    );
    assert!(
        !llvm.contains("@must_not_run"),
        "metadata must not execute unused arguments: {llvm}"
    );
    assert!(llvm.contains("DW_OP_LLVM_fragment, 0, 64"), "{llvm}");
    assert!(
        !llvm.contains("DW_OP_LLVM_fragment, 64, 64"),
        "unused aggregate fields must remain unavailable: {llvm}"
    );
    assert_eq!(
        llvm.lines()
            .filter(|line| {
                line.starts_with("define internal") && line.contains("@sx.debug.call.4")
            })
            .count(),
        2,
        "{llvm}"
    );
    assert_eq!(
        llvm.lines()
            .filter(|line| line.contains(" = add i64 "))
            .count(),
        1,
        "{llvm}"
    );
    assert_eq!(
        llvm.lines()
            .filter(|line| line.contains(" = add i128 "))
            .count(),
        1,
        "{llvm}"
    );
}

#[test]
fn derived_hardware_keeps_shared_values_when_widening_arithmetic() {
    use siox::ir::{Driver, Expr, ProcessIr, ProcessValue};
    let span = siox::diag::Span::new(siox::diag::FileId(0), 0..1);
    let signal = |name: &str, width| siox::ir::Signal {
        path: name.into(),
        declaration_span: span,
        width,
        real: false,
        integer: false,
        char: false,
        range: None,
        init: vec![0],
        enum_type: None,
    };
    let mut ir = ProcessIr::default();
    ir.values.push(ProcessValue {
        span,
        ty: None,
        bit_width: Some(8),
        kind: ProcessValueKind::Signal {
            signals: vec![SignalId(0)],
            state: ProcessSignalState::Current,
        },
    });
    let mut previous = ProcessValueId(0);
    for _ in 0..30 {
        let id = ProcessValueId(ir.values.len() as u32);
        ir.values.push(ProcessValue {
            span,
            ty: None,
            bit_width: Some(8),
            kind: ProcessValueKind::Binary {
                operation: ProcessBinaryOp::Add,
                left: previous,
                right: previous,
            },
        });
        previous = id;
    }
    let design = Design {
        signals: vec![signal("input", 8), signal("output", 64)],
        drivers: vec![Driver {
            target: SignalId(1),
            cond: None,
            meta: None,
            ctx: 0,
            span: Some(span),
            expr: Expr::Canonical {
                value: previous,
                reads: std::sync::Arc::from([SignalId(0)]),
            },
        }],
        process_ir: ir,
        ..Design::default()
    };
    let llvm = super::super::emit::emit_module_ir(&design).expect("canonical hardware emits");
    assert_eq!(
        llvm.lines()
            .filter(|line| line.contains(" = add i64 ") && line.contains("pv.add"))
            .count(),
        30,
        "each widened shared node should emit once, not 2^30 times"
    );
}
use siox::diag::{FileId, Span};
use siox::elab::InstanceId;
use siox::ir::{
    ProcessBinaryOp, ProcessBlock, ProcessBlockId, ProcessCfg, ProcessId, ProcessIr, ProcessRegion,
    ProcessSignalState, ProcessTerminator, ProcessTest, ProcessValue, ProcessValueId,
    ProcessValueKind, Signal, SignalId,
};
use siox::resolve::DefId;

/// A source-less span for hand-built backend fixtures.
fn span() -> Span {
    Span::new(FileId(0), 0..0)
}

fn recursive_metadata_fixture(width: u32) -> (Design, SourceLayout) {
    // This fixture tests layout mechanics, not a compiler-owned truth table.
    let design = Design {
        logic_encodings: HashMap::from([(
            "FixtureElement".into(),
            siox::ir::LogicEncoding::default(),
        )]),
        ..Design::default()
    };
    let layout = SourceLayout {
        span: span(),
        kind: LayoutKind::Struct {
            name: "FixtureRecord".into(),
            view: None,
            fields: vec![
                siox::ir::LayoutField {
                    name: "count".into(),
                    direction: None,
                    layout: SourceLayout {
                        span: span(),
                        kind: LayoutKind::Scalar {
                            width: 64,
                            domain: siox::ir::ScalarDomain::Integer,
                            nominal: None,
                            value_range: None,
                        },
                    },
                },
                siox::ir::LayoutField {
                    name: "payload".into(),
                    direction: None,
                    layout: SourceLayout {
                        span: span(),
                        kind: LayoutKind::Array {
                            range: Some(LayoutRange { left: -1, right: 0 }),
                            element: Box::new(SourceLayout {
                                span: span(),
                                kind: LayoutKind::Packed {
                                    width,
                                    family: "FixtureWord".into(),
                                    range: Some(LayoutRange {
                                        left: i64::from(width) - 1,
                                        right: 0,
                                    }),
                                    element_enum: Some("FixtureElement".into()),
                                },
                            }),
                        },
                    },
                },
            ],
        },
    };
    (design, layout)
}

#[test]
fn recursive_metadata_regions_use_value_offsets_and_exact_widths() {
    let (design, layout) = recursive_metadata_fixture(128);
    assert_eq!(layout_width(&layout), Some(320));
    assert_eq!(layout_meta_width(&design, &layout), Some(1280));
    let selected = projection_slice(&layout, ".payload[0]").unwrap();
    assert_eq!((selected.offset, selected.width), (192, 128));
    assert_eq!(layout_meta_width(&design, selected.layout), Some(512));
    let scalar = field_slice(&layout, "count").unwrap();
    assert_eq!(layout_meta_width(&design, scalar.layout), None);
}

#[test]
fn state_epoch_caches_preserve_signal_versions_widths_and_metadata_layouts() {
    let (mut design, _) = recursive_metadata_fixture(128);
    let signal = |name: &str, width| Signal {
        path: name.into(),
        declaration_span: span(),
        width,
        real: false,
        integer: false,
        char: false,
        range: None,
        init: vec![0; width.div_ceil(64) as usize],
        enum_type: None,
    };
    design.signals = vec![signal("value", 128), signal("metadata", 512)];
    design.meta_of.insert(0, 1);
    design.process_ir.values.push(ProcessValue {
        span: span(),
        ty: None,
        bit_width: Some(128),
        kind: ProcessValueKind::Signal {
            signals: vec![SignalId(0)],
            state: ProcessSignalState::Current,
        },
    });
    let context = Context::create();
    let module = context.create_module("cache");
    let builder = context.create_builder();
    for name in [
        "sx_read_word",
        "sx.process.read.old",
        "sx.process.read.event",
    ] {
        module.add_function(
            name,
            context.i64_type().fn_type(
                &[context.i32_type().into(), context.i32_type().into()],
                false,
            ),
            None,
        );
    }
    let function = module.add_function("probe", context.void_type().fn_type(&[], false), None);
    builder.position_at_end(context.append_basic_block(function, "entry"));
    let mut cache = ProcessValueCache::new(&[true], &[false]);
    let read = |state, width, cache: &mut _| {
        cached_signal_value(
            &context,
            &module,
            &builder,
            &design,
            &[SignalId(0)],
            state,
            width,
            cache,
        )
        .unwrap()
    };
    let current = read(ProcessSignalState::Current, 128, &mut cache);
    assert_eq!(current, read(ProcessSignalState::Current, 128, &mut cache));
    assert_ne!(current, read(ProcessSignalState::Old, 128, &mut cache));
    assert_ne!(current, read(ProcessSignalState::Current, 64, &mut cache));
    read(ProcessSignalState::Event, 1, &mut cache);
    assert_eq!(cache.signals.len(), 4);
    let mut layout = SourceLayout {
        span: span(),
        kind: LayoutKind::Packed {
            width: 128,
            family: "FixtureWord".into(),
            range: Some(LayoutRange {
                left: 127,
                right: 0,
            }),
            element_enum: Some("FixtureElement".into()),
        },
    };
    let sites = HashMap::new();
    let active = Some(context.bool_type().const_int(1, false));
    let first = process_packed_meta_in_layout(
        &context,
        &module,
        &builder,
        &design,
        ProcessValueId(0),
        &layout,
        active,
        &sites,
        &mut cache,
    )
    .unwrap();
    assert_eq!(
        first,
        process_packed_meta_in_layout(
            &context,
            &module,
            &builder,
            &design,
            ProcessValueId(0),
            &layout,
            active,
            &sites,
            &mut cache
        )
        .unwrap()
    );
    assert_eq!(cache.metadata.len(), 1);
    if let LayoutKind::Packed { range, .. } = &mut layout.kind {
        *range = Some(LayoutRange {
            left: 0,
            right: 127,
        });
    }
    process_packed_meta_in_layout(
        &context,
        &module,
        &builder,
        &design,
        ProcessValueId(0),
        &layout,
        active,
        &sites,
        &mut cache,
    )
    .unwrap();
    process_packed_meta_in_layout(
        &context,
        &module,
        &builder,
        &design,
        ProcessValueId(0),
        &layout,
        Some(context.bool_type().const_zero()),
        &sites,
        &mut cache,
    )
    .unwrap();
    assert_eq!(
        cache.metadata.len(),
        3,
        "layout and checked activity distinguish planes"
    );
    cache.clear();
    assert!(cache.metadata.is_empty() && cache.signals.is_empty());
    assert_ne!(current, read(ProcessSignalState::Current, 128, &mut cache));
    builder.build_return(None).unwrap();
    module.verify().unwrap();
    assert_eq!(
        module
            .print_to_string()
            .to_string()
            .lines()
            .filter(|line| line.contains("call i64 @sx_read_word"))
            .count(),
        13,
        "two current words + one narrow word + eight companion words + two after invalidation"
    );
}

#[test]
fn inactive_inline_debug_call_does_not_emit_its_body_or_frame() {
    let context = Context::create();
    let module = context.create_module("inactive_inline");
    let builder = context.create_builder();
    let function = module.add_function("probe", context.void_type().fn_type(&[], false), None);
    builder.position_at_end(context.append_basic_block(function, "entry"));
    let mut sources = siox::diag::SourceMap::new();
    sources.add("inactive.siox", "fn inactive() {}\n");
    let design = Design::default();
    let debug = DebugInfo::new(&context, &module, &sources, &design).unwrap();
    let mut cache = ProcessValueCache::new(&[], &[]);
    cache.debug = Some(&debug);
    let source = siox::ir::ProcessValueInline {
        name: "inactive".into(),
        function: span(),
        call: span(),
        result_span: span(),
        result_layout: None,
        aggregate: None,
        branches: Vec::new(),
        parameters: Vec::new(),
        locals: Vec::new(),
    };
    let result = debug::inline_value(
        &module,
        &builder,
        &design,
        ProcessValueId(0),
        ProcessValueId(0),
        &source,
        129,
        false,
        None,
        Some(context.bool_type().const_zero()),
        &HashMap::new(),
        &mut cache,
        |_, _, _, _| panic!("a compile-time-inactive call must not emit its body"),
    )
    .unwrap();
    assert_eq!(result.get_type().get_bit_width(), 129);
    assert!(result.is_const());
    assert!(cache.value_frames.is_empty());
    assert_eq!(function.count_basic_blocks(), 1);
    let storage = builder
        .build_alloca(context.bool_type(), "predicate")
        .unwrap();
    builder
        .build_store(storage, context.bool_type().const_int(1, false))
        .unwrap();
    let predicate = builder
        .build_load(context.bool_type(), storage, "predicate")
        .unwrap()
        .into_int_value();
    let key = ProcessValueId(7);
    cache.emitted.insert((key, None, None), result);
    cache.contextual.insert((key, None, 129, false), result);
    assert_eq!(
        cache.emitted_value((key, Some(predicate), None)),
        Some(result)
    );
    assert_eq!(
        cache.contextual_value((key, Some(predicate), 129, false)),
        Some(result)
    );
    assert!(cache
        .contextual_value((key, Some(predicate), 128, false))
        .is_none());
    assert!(cache
        .contextual_value((key, Some(predicate), 129, true))
        .is_none());
    cache.clear();
    assert!(cache.emitted_value((key, Some(predicate), None)).is_none());
    assert!(cache
        .contextual_value((key, Some(predicate), 129, false))
        .is_none());
    for active in [context.bool_type().const_int(1, false), predicate] {
        let result = debug::inline_value(
            &module,
            &builder,
            &design,
            ProcessValueId(0),
            ProcessValueId(0),
            &source,
            129,
            false,
            None,
            Some(active),
            &HashMap::new(),
            &mut cache,
            |_, _, active, _| {
                assert!(
                    active.is_none(),
                    "the selected arm already guarantees activity"
                );
                Some(
                    context
                        .custom_width_int_type(std::num::NonZeroU32::new(129).unwrap())
                        .unwrap()
                        .const_zero(),
                )
            },
        )
        .unwrap();
        assert_eq!(result.get_type().get_bit_width(), 129);
    }
    builder.build_return(None).unwrap();
    debug.finish(&module, &design);
    module.verify().unwrap();
}

#[test]
fn branch_cache_retains_values_not_replaced_keys_or_invalidated_reads() {
    fn bind<'ctx>(
        cache: &mut ProcessValueCache<'ctx, '_>,
        id: u32,
        value: IntValue<'ctx>,
        layout: &SourceLayout,
    ) {
        let node = ProcessValueId(id);
        cache.emitted.insert((node, None, None), value);
        cache.contextual.insert((node, None, 64, false), value);
        cache.metadata.insert((node, None, layout.clone()), value);
        cache
            .signals
            .insert((SignalId(id), ProcessSignalState::Current, 64), value);
        cache.evaluated_calls.insert((node, None), value);
        for width in [64, 128] {
            cache.source_results.insert(
                (node, width, false, None),
                (
                    value,
                    value
                        .get_type()
                        .get_context()
                        .bool_type()
                        .const_int(1, false),
                ),
            );
        }
    }
    let context = Context::create();
    let module = context.create_module("branch_cache");
    let builder = context.create_builder();
    let function = module.add_function("probe", context.void_type().fn_type(&[], false), None);
    let entry = context.append_basic_block(function, "entry");
    let arm = context.append_basic_block(function, "arm");
    let join = context.append_basic_block(function, "join");
    builder.position_at_end(entry);
    let integer = context.i64_type();
    let storage = builder.build_alloca(integer, "value").unwrap();
    builder
        .build_store(storage, integer.const_int(1, false))
        .unwrap();
    let load = || {
        builder
            .build_load(integer, storage, "read")
            .unwrap()
            .into_int_value()
    };
    let first = load();
    let second = load();
    let layout = SourceLayout {
        span: span(),
        kind: LayoutKind::Scalar {
            width: 64,
            domain: siox::ir::ScalarDomain::Integer,
            nominal: None,
            value_range: None,
        },
    };
    let mut cache = ProcessValueCache::new(&[], &[]);
    bind(&mut cache, 0, first, &layout);
    bind(&mut cache, 1, second, &layout);
    let dominating = cache.dominating_values();
    builder.build_unconditional_branch(arm).unwrap();
    builder.position_at_end(arm);
    let replacement = load();
    bind(&mut cache, 0, replacement, &layout);
    bind(&mut cache, 2, replacement, &layout);
    let branch_ready = builder
        .build_int_compare(IntPredicate::EQ, replacement, first, "branch.ready")
        .unwrap();
    cache
        .source_results
        .insert((ProcessValueId(3), 64, false, None), (second, branch_ready));
    builder.build_unconditional_branch(join).unwrap();
    builder.position_at_end(join);
    cache.retain_dominating(&dominating);
    assert_eq!(cache.emitted.len(), 1);
    assert_eq!(cache.contextual.len(), 1);
    assert_eq!(cache.metadata.len(), 1);
    assert_eq!(cache.signals.len(), 1);
    assert_eq!(cache.evaluated_calls.len(), 1);
    assert_eq!(cache.source_results.len(), 2);
    for width in [64, 128] {
        assert_eq!(
            cache.source_results[&(ProcessValueId(1), width, false, None)].0,
            second
        );
    }
    assert_eq!(cache.emitted[&(ProcessValueId(1), None, None)], second);

    // A foreign call can invalidate old state and reload an existing key.
    // Pruning must not restore either the old read or that new definition.
    cache.clear();
    assert!(cache.source_results.is_empty());
    bind(&mut cache, 1, load(), &layout);
    cache.retain_dominating(&dominating);
    assert!(cache.emitted.is_empty());
    assert!(cache.contextual.is_empty());
    assert!(cache.metadata.is_empty());
    assert!(cache.signals.is_empty());
    assert!(cache.evaluated_calls.is_empty());
    assert!(cache.source_results.is_empty());
    builder.build_return(None).unwrap();
    module.verify().unwrap();
}

#[test]
fn recursive_metadata_preflight_rejects_an_unsupported_leaf() {
    let (mut design, layout) = recursive_metadata_fixture(128);
    let array = field_slice(&layout, "payload").unwrap().layout.clone();
    design.process_ir.values = vec![
        ProcessValue {
            span: span(),
            ty: None,
            bit_width: Some(128),
            kind: ProcessValueKind::Invalid,
        },
        ProcessValue {
            span: span(),
            ty: None,
            bit_width: Some(256),
            kind: ProcessValueKind::Array(vec![ProcessValueId(0), ProcessValueId(0)]),
        },
    ];
    design.process_ir.value_layouts = vec![None, Some(array.clone())];
    let supported = supported_process_values(&design);
    assert!(!process_value_meta_supported(
        &design,
        ProcessValueId(1),
        &array,
        &supported
    ));
    assert!(!process_value_supported_in_layout(
        &design,
        ProcessValueId(1),
        &array,
        &supported
    ));
}

#[test]
fn oversized_recursive_metadata_frames_fail_before_llvm_type_construction() {
    let (mut design, layout) = recursive_metadata_fixture(crate::llvm::emit::LLVM_MAX_INT_BITS / 8);
    // The value fits LLVM's integer limit; its recursive companion does not.
    let width = layout_width(&layout).unwrap();
    assert!(width < crate::llvm::emit::LLVM_MAX_INT_BITS);
    design.process_ir.values = vec![ProcessValue {
        span: span(),
        ty: None,
        bit_width: Some(width),
        kind: ProcessValueKind::Default,
    }];
    design.process_ir.value_layouts = vec![Some(layout)];
    let error = validate_metadata_widths(&design).unwrap_err();
    assert!(
        error.contains("process value 0") && error.contains("metadata frame"),
        "{error}"
    );
}

#[test]
fn metavalue_facts_handle_a_deeply_shared_dag_without_recursion() {
    let mut values = vec![ProcessValue {
        span: span(),
        ty: Some(siox::types::Ty::Integer),
        bit_width: Some(64),
        kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![1])),
    }];
    // Each node references its predecessor twice: a recursive walk revisits
    // 2^depth paths, while the arena contains only depth + 1 values.
    for index in 0..50_000 {
        let previous = ProcessValueId(index);
        values.push(ProcessValue {
            span: span(),
            ty: Some(siox::types::Ty::Integer),
            bit_width: Some(64),
            kind: ProcessValueKind::Binary {
                operation: ProcessBinaryOp::And,
                left: previous,
                right: previous,
            },
        });
    }
    let design = Design {
        process_ir: ProcessIr {
            values,
            ..ProcessIr::default()
        },
        ..Design::default()
    };
    let facts = supported_process_values(&design);
    assert_eq!(facts.meta_free.len(), 50_001);
    assert!(facts.meta_free.iter().all(|free| *free));
    assert!(facts.values.iter().all(|supported| *supported));
}

#[test]
fn metavalue_facts_preserve_unknown_inputs_and_explicit_integer_conversion() {
    let node = |kind, ty| ProcessValue {
        span: span(),
        ty,
        bit_width: Some(8),
        kind,
    };
    let design = Design {
        meta_of: HashMap::from([(0, 1)]),
        process_ir: ProcessIr {
            values: vec![
                node(
                    ProcessValueKind::Signal {
                        signals: vec![SignalId(0)],
                        state: ProcessSignalState::Current,
                    },
                    None,
                ),
                node(
                    ProcessValueKind::Unary {
                        operation: ProcessUnaryOp::Not,
                        operand: ProcessValueId(0),
                    },
                    None,
                ),
                node(
                    ProcessValueKind::RawResize {
                        operand: ProcessValueId(1),
                    },
                    Some(siox::types::Ty::Integer),
                ),
                node(
                    ProcessValueKind::Binary {
                        operation: ProcessBinaryOp::And,
                        left: ProcessValueId(1),
                        right: ProcessValueId(2),
                    },
                    None,
                ),
                // A forward/cyclic dependency is never treated as known.
                node(
                    ProcessValueKind::RawResize {
                        operand: ProcessValueId(4),
                    },
                    None,
                ),
            ],
            ..ProcessIr::default()
        },
        ..Design::default()
    };
    assert_eq!(
        meta_free_process_values(&design),
        [false, false, true, false, false]
    );
}

fn scheduled_process(id: u32, schedules: &[(Option<u32>, ProcessValueId)]) -> ProcessCfg {
    let span = span();
    ProcessCfg {
        id: ProcessId(id),
        root: InstanceId(0),
        owner: InstanceId(0),
        label: None,
        span,
        activation: ProcessActivation::TimeZero,
        region: ProcessRegion::Procedural,
        entry: ProcessBlockId(0),
        inline_calls: Vec::new(),
        locals: Vec::new(),
        blocks: vec![ProcessBlock {
            id: ProcessBlockId(0),
            instructions: schedules
                .iter()
                .map(|(driver_context, target)| ProcessInstruction::Schedule {
                    driver_context: *driver_context,
                    target: *target,
                    value: ProcessValueId(2),
                    delay: ProcessValueId(3),
                    span,
                })
                .collect(),
            terminator: ProcessTerminator::Return {
                value: None,
                span: Some(span),
            },
        }],
    }
}

/// A waveform follows a driver/place pair, not a source instruction. This is
/// what lets later statements edit earlier pending transactions while
/// preventing another driver on the same resolved signal from cancelling it.
#[test]
fn schedule_waveforms_share_only_driver_and_exact_place() {
    let span = span();
    let signal = |path: &str| Signal {
        path: path.into(),
        declaration_span: span,
        width: 1,
        real: false,
        integer: false,
        char: false,
        range: None,
        init: vec![0],
        enum_type: None,
    };
    let signal_value = |signal| ProcessValue {
        span,
        ty: None,
        bit_width: Some(1),
        kind: ProcessValueKind::Signal {
            signals: vec![SignalId(signal)],
            state: ProcessSignalState::Current,
        },
    };
    let design = Design {
        signals: vec![signal("T.a"), signal("T.b")],
        process_ir: ProcessIr {
            processes: vec![
                scheduled_process(
                    0,
                    &[
                        (None, ProcessValueId(0)),
                        (None, ProcessValueId(0)),
                        (None, ProcessValueId(1)),
                    ],
                ),
                scheduled_process(1, &[(None, ProcessValueId(0))]),
                scheduled_process(2, &[(Some(7), ProcessValueId(0))]),
                scheduled_process(3, &[(Some(7), ProcessValueId(0))]),
            ],
            values: vec![
                signal_value(0),
                signal_value(1),
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(1),
                    kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![0])),
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(1),
                    kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![1])),
                },
            ],
            ..ProcessIr::default()
        },
        ..Design::default()
    };

    let sites = schedule_sites(&design);
    assert_eq!(sites.len(), 6);
    assert_eq!(sites[0].lanes[0].waveform, sites[1].lanes[0].waveform);
    assert_ne!(sites[0].lanes[0].waveform, sites[2].lanes[0].waveform);
    assert_ne!(sites[0].lanes[0].waveform, sites[3].lanes[0].waveform);
    assert_eq!(sites[4].lanes[0].waveform, sites[5].lanes[0].waveform);
    assert_ne!(sites[0].lanes[0].waveform, sites[4].lanes[0].waveform);
}

/// The design object exports stable flattened descriptors directly.
#[test]
fn emits_runtime_discovery_metadata() {
    let process_ir = ProcessIr {
        functions: Vec::new(),
        calls: Vec::new(),
        processes: vec![
            ProcessCfg {
                id: ProcessId(0),
                root: InstanceId(7),
                owner: InstanceId(7),
                label: Some("stimulus".into()),
                span: span(),
                activation: ProcessActivation::TimeZero,
                region: ProcessRegion::Procedural,
                entry: ProcessBlockId(0),
                inline_calls: Vec::new(),
                locals: vec![],
                blocks: vec![ProcessBlock {
                    id: ProcessBlockId(0),
                    instructions: vec![],
                    terminator: ProcessTerminator::Return {
                        value: None,
                        span: None,
                    },
                }],
            },
            ProcessCfg {
                id: ProcessId(1),
                root: InstanceId(7),
                owner: InstanceId(8),
                label: Some("dut.y".into()),
                span: span(),
                activation: ProcessActivation::Reactive {
                    sensitivity: vec![ProcessSensitivity::Signal(siox::ir::SignalId(0))],
                },
                region: ProcessRegion::Procedural,
                entry: ProcessBlockId(0),
                inline_calls: Vec::new(),
                locals: vec![],
                blocks: vec![ProcessBlock {
                    id: ProcessBlockId(0),
                    instructions: vec![],
                    terminator: ProcessTerminator::Return {
                        value: None,
                        span: None,
                    },
                }],
            },
        ],
        tests: vec![ProcessTest {
            entity: DefId(11),
            root: InstanceId(7),
            qualified_name: "examples::Smoke".into(),
            span: span(),
            processes: vec![ProcessId(0), ProcessId(1)],
        }],
        storages: vec![],
        values: vec![],
        value_layouts: vec![],
    };
    let design = Design {
        signals: vec![siox::ir::Signal {
            path: "Smoke.dut.input".into(),
            declaration_span: span(),
            width: 1,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: vec![0],
            enum_type: None,
        }],
        process_ir,
        ..Design::default()
    };
    let llvm = crate::llvm::emit_module_ir(&design).unwrap();
    assert!(llvm.contains("@sx_process_abi_version = constant i32 16"));
    assert!(llvm.contains("define i8 @sx_process_commit()"));
    assert!(llvm.contains("define i8 @sx_process_changed(i32"));
    assert!(llvm.contains("define i8 @sx_process_storage_changed(i32"));
    assert!(llvm.contains("define internal void @sx.process.stage.0(i1"));
    assert!(llvm.contains("@sx_test_count = constant i32 1"));
    assert!(llvm.contains("@sx_process_count = constant i32 2"));
    assert!(llvm.contains("@sx.test.name.0 = private constant [16 x i8] c\"examples::Smoke\\00\""));
    assert!(llvm.contains("@sx_test_process_offsets = constant [2 x i32] [i32 0, i32 2]"));
    assert!(llvm.contains("@sx_test_process_ids = constant [2 x i32] [i32 0, i32 1]"));
    assert!(llvm.contains(
        "@sx_process_entries = constant [2 x ptr] [ptr @sx.process.0, ptr @sx.process.1]"
    ));
    assert!(llvm.contains("@sx_process_initial_blocks = constant [2 x i32] zeroinitializer"));
    assert!(llvm.contains("@sx_process_activations = constant [2 x i8] c\"\\00\\01\""));
    assert!(
        llvm.contains("@sx_process_sensitivity_offsets = constant [3 x i32] [i32 0, i32 0, i32 1]")
    );
    assert!(llvm.contains("@sx_process_sensitivity_ids = constant [1 x i32] zeroinitializer"));
    assert!(llvm.contains("@sx_wave_signal_count = constant i32 1"));
    assert!(llvm.contains("@sx_wave_signal_ids = constant [1 x i32] zeroinitializer"));
    assert!(llvm.contains("@sx_wave_signal_widths = constant [1 x i32] [i32 1]"));
    assert!(llvm.contains("@sx_wave_signal_kinds = constant [1 x i8] zeroinitializer"));
    assert!(llvm.contains("@sx_wave_signal_companions = constant [1 x i32] [i32 -1]"));
    assert!(llvm.contains("@sx_wave_scope_count = constant i32 2"));
    assert!(llvm.contains("@sx_wave_scope_parents = constant [2 x i32] [i32 -1, i32 0]"));
    assert!(llvm.contains("@sx_wave_scope_names = constant [2 x ptr]"));
    assert!(llvm.contains("@sx_wave_signal_scopes = constant [1 x i32] [i32 1]"));
    assert!(llvm.contains("@sx_wave_signal_names = constant [1 x ptr]"));
    assert!(llvm.contains("@sx_wave_vcd_header = constant"));
    assert!(llvm.contains("@sx_source_location_count = constant i32 0"));
    assert!(llvm.contains("@sx_source_location_texts = constant [1 x ptr] zeroinitializer"));
    assert!(llvm.contains("$scope module Smoke $end"));
    assert!(llvm.contains("define internal i8 @sx.process.0(i32"));
    assert!(llvm.contains("define internal i8 @sx.process.1(i32"));
}

/// Scalar discriminants and packed companion planes have distinct ABIs.
#[test]
fn scalar_wave_metadata_ignores_incidental_companions() {
    let signal = |name: &str, width, enum_type| Signal {
        path: format!("Wave.{name}"),
        declaration_span: span(),
        width,
        enum_type,
        real: false,
        integer: false,
        char: false,
        range: None,
        init: vec![0],
    };
    // A synthetic encoding checks that this classifier uses source metadata,
    // not std type names or assumptions about particular discriminants.
    let design = Design {
        signals: vec![
            signal("scalar", 4, Some("Element".into())),
            signal("scalar_meta", 16, None),
            signal("packed", 4, None),
            signal("packed_meta", 16, None),
        ],
        meta_of: HashMap::from([(0, 1), (2, 3)]),
        array_element_enums: HashMap::from([(2, "Element".into())]),
        enum_syms: HashMap::from([(
            "Element".into(),
            HashMap::from([(7, "Unknown".into()), (11, "High".into())]),
        )]),
        logic_encodings: HashMap::from([(
            "Element".into(),
            siox::ir::LogicEncoding {
                unknown: std::collections::HashSet::from([7]),
                value_bits: HashMap::from([(11, true)]),
                ..siox::ir::LogicEncoding::default()
            },
        )]),
        ..Design::default()
    };
    let context = Context::create();
    let module = context.create_module("wave_metadata");
    super::metadata::emit_wave_metadata(&context, &module, &design);
    let llvm = module.print_to_string().to_string();
    assert!(
        llvm.contains("@sx_wave_signal_kinds = constant [2 x i8] c\"\\03\\04\""),
        "{llvm}"
    );
    assert!(
        llvm.contains("@sx_wave_signal_companions = constant [2 x i32] [i32 -1, i32 3]"),
        "{llvm}"
    );
    assert!(llvm.contains("$var wire 1 v0 scalar $end"));
    assert!(llvm.contains("$var wire 4 v2 packed $end"));
    assert!(!llvm.contains("$var wire 16"));
    module.verify().unwrap();
}

/// Empty logical descriptor tables still have legal storage for the C ABI,
/// while their exported counts remain zero.
#[test]
fn empty_design_uses_unobservable_sentinels() {
    let llvm = crate::llvm::emit_module_ir(&Design::default()).unwrap();
    assert!(llvm.contains("@sx_test_count = constant i32 0"));
    assert!(llvm.contains("@sx_process_count = constant i32 0"));
    assert!(llvm.contains("@sx_test_names = constant [1 x ptr] zeroinitializer"));
    assert!(llvm.contains("@sx_process_entries = constant [1 x ptr] zeroinitializer"));
    assert!(llvm.contains("@sx_process_activations = constant [1 x i8] zeroinitializer"));
    assert!(llvm.contains("@sx_wave_signal_count = constant i32 0"));
    assert!(llvm.contains("@sx_wave_signal_ids = constant [1 x i32] zeroinitializer"));
    assert!(llvm.contains("@sx_wave_scope_count = constant i32 0"));
    assert!(llvm.contains("@sx_wave_scope_names = constant [1 x ptr] zeroinitializer"));
    assert!(llvm.contains("@sx_wave_signal_names = constant [1 x ptr] zeroinitializer"));
}

/// Fixed strings are ordinary packed Process values, while equality over
/// two zero-element strings remains executable without inventing a
/// one-bit storage object for either empty array.
#[test]
fn fixed_and_empty_strings_are_executable_process_values() {
    let string_type = |len| siox::types::Ty::Array {
        elem: Box::new(siox::types::Ty::Char),
        len,
        family: None,
        low: 0,
    };
    let values = vec![
        ProcessValue {
            span: span(),
            ty: Some(string_type(2)),
            bit_width: Some(64),
            kind: ProcessValueKind::String("hé".into()),
        },
        ProcessValue {
            span: span(),
            ty: Some(string_type(2)),
            bit_width: Some(64),
            kind: ProcessValueKind::String("hé".into()),
        },
        ProcessValue {
            span: span(),
            ty: None,
            bit_width: Some(1),
            kind: ProcessValueKind::Binary {
                operation: ProcessBinaryOp::Eq,
                left: ProcessValueId(0),
                right: ProcessValueId(1),
            },
        },
        ProcessValue {
            span: span(),
            ty: Some(string_type(0)),
            bit_width: None,
            kind: ProcessValueKind::String(String::new()),
        },
        ProcessValue {
            span: span(),
            ty: Some(string_type(0)),
            bit_width: None,
            kind: ProcessValueKind::String(String::new()),
        },
        ProcessValue {
            span: span(),
            ty: None,
            bit_width: Some(1),
            kind: ProcessValueKind::Binary {
                operation: ProcessBinaryOp::Eq,
                left: ProcessValueId(3),
                right: ProcessValueId(4),
            },
        },
        ProcessValue {
            span: span(),
            ty: None,
            bit_width: Some(1),
            kind: ProcessValueKind::Binary {
                operation: ProcessBinaryOp::And,
                left: ProcessValueId(2),
                right: ProcessValueId(5),
            },
        },
    ];
    let process_ir = ProcessIr {
        processes: vec![ProcessCfg {
            id: ProcessId(0),
            root: InstanceId(0),
            owner: InstanceId(0),
            label: Some("string-values".into()),
            span: span(),
            activation: ProcessActivation::TimeZero,
            region: ProcessRegion::Procedural,
            entry: ProcessBlockId(0),
            inline_calls: Vec::new(),
            locals: vec![],
            blocks: vec![
                ProcessBlock {
                    id: ProcessBlockId(0),
                    instructions: vec![],
                    terminator: ProcessTerminator::Branch {
                        condition: ProcessValueId(6),
                        then_block: ProcessBlockId(1),
                        else_block: ProcessBlockId(2),
                    },
                },
                ProcessBlock {
                    id: ProcessBlockId(1),
                    instructions: vec![],
                    terminator: ProcessTerminator::Stop { span: span() },
                },
                ProcessBlock {
                    id: ProcessBlockId(2),
                    instructions: vec![],
                    terminator: ProcessTerminator::Stop { span: span() },
                },
            ],
        }],
        values,
        ..ProcessIr::default()
    };
    let design = Design {
        process_ir,
        ..Design::default()
    };

    let supported = supported_process_values(&design);
    assert_eq!(
        supported.values,
        [true, true, true, false, false, true, true]
    );
    assert_eq!(
        process_layout_attribute(&design, ProcessValueId(0), "length"),
        Some(2)
    );
    assert_eq!(
        process_layout_attribute(&design, ProcessValueId(3), "length"),
        Some(0)
    );
    let llvm = crate::llvm::emit_module_ir(&design).expect("fixed string Process values lower");
    assert!(llvm.contains("define internal i8 @sx.process.0(i32"));
    assert!(llvm.contains("br i1"), "{llvm}");
}

/// Resume dispatch preserves CFG block identity and emits the stable
/// control-only status values before executable instruction coverage. It
/// enters only at the initial block or a suspension's resume block, so a
/// block reached by branching (or not at all) is no entry of its own.
#[test]
fn process_entries_dispatch_control_only_cfgs() {
    let process_ir = ProcessIr {
        processes: vec![ProcessCfg {
            id: ProcessId(0),
            root: InstanceId(0),
            owner: InstanceId(0),
            label: Some("control".into()),
            span: span(),
            activation: ProcessActivation::TimeZero,
            region: ProcessRegion::Procedural,
            entry: ProcessBlockId(0),
            inline_calls: Vec::new(),
            locals: vec![],
            blocks: vec![
                ProcessBlock {
                    id: ProcessBlockId(0),
                    instructions: vec![],
                    terminator: ProcessTerminator::Goto(ProcessBlockId(1)),
                },
                ProcessBlock {
                    id: ProcessBlockId(1),
                    instructions: vec![],
                    terminator: ProcessTerminator::Stop { span: span() },
                },
                ProcessBlock {
                    id: ProcessBlockId(2),
                    instructions: vec![],
                    terminator: ProcessTerminator::Finish { span: span() },
                },
                ProcessBlock {
                    id: ProcessBlockId(3),
                    instructions: vec![],
                    terminator: ProcessTerminator::Return {
                        value: Some(siox::ir::ProcessValueId(0)),
                        span: Some(span()),
                    },
                },
            ],
        }],
        values: vec![siox::ir::ProcessValue {
            span: span(),
            ty: Some(siox::types::Ty::Integer),
            bit_width: Some(64),
            kind: siox::ir::ProcessValueKind::Number(siox::ir::ProcessNumber::Integer(vec![1])),
        }],
        ..ProcessIr::default()
    };
    let llvm = crate::llvm::emit_module_ir(&Design {
        process_ir,
        ..Design::default()
    })
    .unwrap();
    assert!(llvm.contains("i32 0, label %bb0"), "{llvm}");
    assert!(!llvm.contains("i32 3, label %bb3"), "{llvm}");
    let body = |label: &str| {
        llvm.split_once(label)
            .map(|(_, rest)| rest.split_once("\n\n").map_or(rest, |(block, _)| block))
            .expect("emitted process block")
    };
    assert!(body("bb0:").contains("br label %bb1"), "{llvm}");
    assert!(body("bb1:").contains("ret i8 2"), "{llvm}");
    assert!(body("bb2:").contains("ret i8 3"), "{llvm}");
    assert!(body("bb3:").contains("ret i8 -1"), "{llvm}");
}

/// Direct branch lowering reconstructs current/old state from as many ABI
/// words as required, combines it with event state, and branches on a
/// selected high bit.
#[test]
fn process_branch_reads_exact_width_state_and_operations() {
    let process_ir = ProcessIr {
        processes: vec![ProcessCfg {
            id: ProcessId(0),
            root: InstanceId(0),
            owner: InstanceId(0),
            label: Some("wide-branch".into()),
            span: span(),
            activation: ProcessActivation::TimeZero,
            region: ProcessRegion::Procedural,
            entry: ProcessBlockId(0),
            inline_calls: Vec::new(),
            locals: vec![],
            blocks: vec![
                ProcessBlock {
                    id: ProcessBlockId(0),
                    instructions: vec![],
                    terminator: ProcessTerminator::Branch {
                        condition: ProcessValueId(6),
                        then_block: ProcessBlockId(1),
                        else_block: ProcessBlockId(2),
                    },
                },
                ProcessBlock {
                    id: ProcessBlockId(1),
                    instructions: vec![],
                    terminator: ProcessTerminator::Stop { span: span() },
                },
                ProcessBlock {
                    id: ProcessBlockId(2),
                    instructions: vec![],
                    terminator: ProcessTerminator::Finish { span: span() },
                },
            ],
        }],
        values: vec![
            ProcessValue {
                span: span(),
                ty: None,
                bit_width: Some(65),
                kind: ProcessValueKind::Signal {
                    signals: vec![SignalId(0)],
                    state: ProcessSignalState::Current,
                },
            },
            ProcessValue {
                span: span(),
                ty: None,
                bit_width: Some(65),
                kind: ProcessValueKind::Signal {
                    signals: vec![SignalId(0)],
                    state: ProcessSignalState::Old,
                },
            },
            ProcessValue {
                span: span(),
                ty: None,
                bit_width: Some(1),
                kind: ProcessValueKind::Signal {
                    signals: vec![SignalId(0)],
                    state: ProcessSignalState::Event,
                },
            },
            ProcessValue {
                span: span(),
                ty: None,
                bit_width: Some(1),
                kind: ProcessValueKind::Binary {
                    operation: ProcessBinaryOp::Ne,
                    left: ProcessValueId(0),
                    right: ProcessValueId(1),
                },
            },
            ProcessValue {
                span: span(),
                ty: None,
                bit_width: Some(1),
                kind: ProcessValueKind::BitSlice {
                    base: ProcessValueId(0),
                    high: 64,
                    low: 64,
                },
            },
            ProcessValue {
                span: span(),
                ty: None,
                bit_width: Some(1),
                kind: ProcessValueKind::Binary {
                    operation: ProcessBinaryOp::And,
                    left: ProcessValueId(2),
                    right: ProcessValueId(3),
                },
            },
            ProcessValue {
                span: span(),
                ty: None,
                bit_width: Some(1),
                kind: ProcessValueKind::Binary {
                    operation: ProcessBinaryOp::And,
                    left: ProcessValueId(4),
                    right: ProcessValueId(5),
                },
            },
        ],
        ..ProcessIr::default()
    };
    let design = Design {
        signals: vec![Signal {
            path: "Wide.flag".into(),
            declaration_span: span(),
            width: 65,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: vec![0, 0],
            enum_type: None,
        }],
        process_ir,
        ..Design::default()
    };
    let llvm = crate::llvm::emit_module_ir(&design).unwrap();
    assert!(
        llvm.contains("call i64 @sx_read_word(i32 0, i32 0)"),
        "{llvm}"
    );
    assert!(
        llvm.contains("call i64 @sx_read_word(i32 0, i32 1)"),
        "{llvm}"
    );
    assert!(
        llvm.contains("call i64 @sx.process.read.old(i32 0, i32 1)"),
        "{llvm}"
    );
    assert!(
        llvm.contains("call i64 @sx.process.read.event(i32 0, i32 0)"),
        "{llvm}"
    );
    assert!(llvm.contains("lshr i65"), "{llvm}");
    assert!(llvm.contains("icmp ne i65"), "{llvm}");
    assert!(llvm.contains("and i1"), "{llvm}");
    assert!(llvm.contains("br i1"), "{llvm}");
}

/// Oversized process-only values fail before asking LLVM to construct an
/// unsupported integer type, even when no hardware signal has that width.
#[test]
fn unsupported_process_value_width_is_an_error_not_a_panic() {
    let process_ir = ProcessIr {
        values: vec![ProcessValue {
            span: span(),
            ty: None,
            bit_width: Some(crate::llvm::emit::LLVM_MAX_INT_BITS + 1),
            kind: ProcessValueKind::Number(siox::ir::ProcessNumber::Integer(vec![0])),
        }],
        ..ProcessIr::default()
    };
    let error = crate::llvm::emit_module_ir(&Design {
        process_ir,
        ..Design::default()
    })
    .unwrap_err();
    assert!(error.contains("process value 0"), "{error}");
    assert!(
        error.contains(&crate::llvm::emit::LLVM_MAX_INT_BITS.to_string()),
        "{error}"
    );
}

#[test]
fn binary_logic_mapping_preserves_std_codes_and_wrapping() {
    let context = Context::create();
    let module = context.create_module("binary_codes");
    let function = module.add_function("check", context.void_type().fn_type(&[], false), None);
    let builder = context.create_builder();
    builder.position_at_end(context.append_basic_block(function, "entry"));
    for (width, low, high) in [(4, 13, 2), (8, 0, 1), (8, 2, 13), (8, 255, 0)] {
        let encoding = siox::ir::LogicEncoding {
            value_bits: [(low, false), (high, true)].into(),
            binary: [low, high].into(),
            ..siox::ir::LogicEncoding::default()
        };
        for (bit, expected) in [(false, low), (true, high)] {
            let value = context.bool_type().const_int(u64::from(bit), false);
            let mapped =
                packed_discriminant(&context, &builder, &encoding, value, None, width).unwrap();
            assert_eq!(mapped.get_zero_extended_constant(), Some(expected));
            let unknown = mapped.get_type().const_int(9, false);
            let retained =
                packed_discriminant(&context, &builder, &encoding, value, Some(unknown), width)
                    .unwrap();
            assert_eq!(retained.get_zero_extended_constant(), Some(9));
        }
    }
    builder.build_return(None).unwrap();
    module.verify().unwrap();
}

/// A chain of guarded writes: each branch block dominates everything after
/// it, and a pure value read by two guarded writes is shared, so it is
/// emitted once in the entry rather than in each write's own block. A value
/// read by one write, or one that reads a local, stays in its block.
#[test]
fn guarded_writes_share_pure_values_through_dominators() {
    let span = span();
    let node = |kind| ProcessValue {
        span,
        ty: None,
        bit_width: Some(8),
        kind,
    };
    let binary = |operation, left: u32, right: u32| ProcessValueKind::Binary {
        operation,
        left: ProcessValueId(left),
        right: ProcessValueId(right),
    };
    let signal = |id: u32| ProcessValueKind::Signal {
        signals: vec![SignalId(id)],
        state: ProcessSignalState::Current,
    };
    let values = vec![
        node(signal(0)),                                                 // 0: a
        node(ProcessValueKind::Number(ProcessNumber::Integer(vec![1]))), // 1
        node(binary(ProcessBinaryOp::Add, 0, 1)),                        // 2: shared a + 1
        node(binary(ProcessBinaryOp::Add, 2, 1)),                        // 3: first write only
        node(binary(ProcessBinaryOp::Mul, 2, 2)),                        // 4: second write only
        node(signal(1)),                                                 // 5: first target
        node(signal(2)),                                                 // 6: second target
        node(signal(3)),                                                 // 7: guard
        node(ProcessValueKind::Local {
            process: ProcessId(0),
            local: ProcessLocalId(0),
        }), // 8: a local
        node(binary(ProcessBinaryOp::Add, 8, 2)),                        // 9: reads the local
    ];
    let assign = |target: u32, value: u32| ProcessInstruction::Assign {
        semantics: ProcessAssignment::StagedSignal,
        driver_context: None,
        target: ProcessValueId(target),
        value: ProcessValueId(value),
        span,
    };
    let block = |id: u32, instructions, terminator| ProcessBlock {
        id: ProcessBlockId(id),
        instructions,
        terminator,
    };
    let branch = |then_block: u32, else_block: u32| ProcessTerminator::Branch {
        condition: ProcessValueId(7),
        then_block: ProcessBlockId(then_block),
        else_block: ProcessBlockId(else_block),
    };
    let mut process = scheduled_process(0, &[]);
    process.region = ProcessRegion::Combinational;
    process.blocks = vec![
        block(0, Vec::new(), branch(1, 2)),
        block(
            1,
            vec![assign(5, 3)],
            ProcessTerminator::Goto(ProcessBlockId(2)),
        ),
        block(2, Vec::new(), branch(3, 4)),
        block(
            3,
            vec![assign(6, 4), assign(6, 9)],
            ProcessTerminator::Goto(ProcessBlockId(4)),
        ),
        block(
            4,
            Vec::new(),
            ProcessTerminator::Return {
                value: None,
                span: Some(span),
            },
        ),
    ];
    let mut design = Design::default();
    design.process_ir.values = values;

    assert_eq!(
        immediate_dominators(&process),
        vec![None, Some(0), Some(0), Some(2), Some(2)]
    );
    let free = place_free_process_values(&design);
    assert!(free[2] && !free[8] && !free[9]);
    let activity = vec![false; design.process_ir.values.len()];
    let shared = shared_pure_values(&design, &process, &free, &activity);
    assert!(shared.contains(&ProcessValueId(2)), "{shared:?}");
    assert!(
        shared.contains(&ProcessValueId(7)),
        "both branches read the guard"
    );
    for single in [3, 4, 9] {
        assert!(!shared.contains(&ProcessValueId(single)), "{shared:?}");
    }
    assert!(
        shared.windows(2).all(|pair| pair[0].0 > pair[1].0),
        "operands come after their users: {shared:?}"
    );
}
