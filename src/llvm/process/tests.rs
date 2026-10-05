use super::*;

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
    assert!(llvm.contains("@sx_process_abi_version = constant i32 13"));
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
    let llvm = crate::llvm::emit_module_ir(&design).expect("fixed string Process values lower");
    assert!(llvm.contains("define internal i8 @sx.process.0(i32"));
    assert!(llvm.contains("br i1"), "{llvm}");
}

/// Resume dispatch preserves CFG block identity and emits the stable
/// control-only status values before executable instruction coverage.
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
    assert!(llvm.contains("i32 3, label %bb3"), "{llvm}");
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
