//! Signal, process, indexing, validation, and representation tests.

use super::*;

#[test]
fn source_process_keeps_multiple_targets_in_one_canonical_cfg() {
    let design = lower_src(
        "module source; entity E { input: integer in, a: integer out, b: integer out, c: integer out }\n\
         impl E { pair: process { a = input; b = input; } other: process { c = input; } }",
    );
    assert!(design.validate().is_empty(), "{:?}", design.validate());
    assert_eq!(design.process_ir.processes.len(), 2);
    let pair = &design.process_ir.processes[0];
    assert_eq!(pair.label.as_deref(), Some("E::pair"));
    assert_eq!(pair.region, ProcessRegion::Combinational);
    assert_eq!(pair.blocks.len(), 1);
    assert_eq!(pair.blocks[0].instructions.len(), 2);
    let writes = pair.blocks[0]
        .instructions
        .iter()
        .map(|instruction| {
            let ProcessInstruction::Assign {
                target,
                driver_context,
                ..
            } = instruction
            else {
                panic!("expected a canonical source signal write");
            };
            let ProcessValueKind::Signal { signals, .. } =
                &design.process_ir.values[target.0 as usize].kind
            else {
                panic!("expected a flattened source signal");
            };
            (
                design.signals[signals[0].0 as usize].path.as_str(),
                *driver_context,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        writes.iter().map(|write| write.0).collect::<Vec<_>>(),
        ["E.a", "E.b"]
    );
    assert_eq!(writes[0].1, writes[1].1);
    assert_eq!(
        design.process_ir.processes[1].label.as_deref(),
        Some("E::other")
    );
    assert!(
        matches!(pair.activation, ProcessActivation::Reactive { ref sensitivity } if sensitivity.len() == 1)
    );
    // Backend target decomposition remains a derived optimization. It must
    // not split the source process or become the input to CFG construction.
    assert_eq!(design.processes().len(), 3);
}

#[test]
fn independent_resolved_targets_keep_separate_root_contexts() {
    let design = lower_src(
        "module roots; impl Resolve for Logic { fn resolve(self, rhs: Logic) -> Logic { return self; } }\n\
         entity A { q: Logic out } impl A { one: process { q = '1'; } two: process { q = '0'; } }\n\
         entity B { q: Logic out } impl B { one: process { q = '0'; } two: process { q = '1'; } }",
    );
    assert!(design.validate().is_empty(), "{:?}", design.validate());
    assert_eq!(design.process_ir.processes.len(), 2);
    let processes = &design.process_ir.processes;
    assert_ne!(processes[0].root, processes[1].root);
    assert_eq!(processes[0].root, processes[0].owner);
    assert_eq!(processes[1].root, processes[1].owner);
    let context = |process: &ProcessCfg| {
        let ProcessInstruction::Assign {
            driver_context: Some(ctx),
            ..
        } = process.blocks[0].instructions[0]
        else {
            panic!("expected a resolved implicit source context");
        };
        ctx
    };
    assert_ne!(context(&processes[0]), context(&processes[1]));
    assert!(processes[0].label.as_ref().unwrap().starts_with("A::"));
    assert!(processes[1].label.as_ref().unwrap().starts_with("B::"));
}

#[test]
fn source_process_owner_is_its_container_not_its_first_child_target() {
    let design = lower_src(
        "module ownership; entity Child { input: integer in, output: integer out } impl Child { output = input; }\n\
         entity Parent { input: integer in, output: integer out } impl Parent {\n\
           let child: Child = {};\n\
           link: process { child.input = input; output = input; }\n\
         }",
    );
    assert!(design.validate().is_empty(), "{:?}", design.validate());
    let process = design
        .process_ir
        .processes
        .iter()
        .find(|process| process.label.as_deref() == Some("Parent::link"))
        .expect("parent source context");
    assert_eq!(
        process.owner, process.root,
        "parent owns a process that drives a child endpoint"
    );
    assert_eq!(process.blocks[0].instructions.len(), 2);
}

#[test]
/// The basic lowering produces the expected signals, driver and event block.
fn lowers_signals_driver_and_event_block() {
    let d = lower_src(COUNTER);
    // Counter signals: clk, rst, en, count, value. The instance's `W = 8`
    // makes the parametric `unsigned[W]` widths concrete.
    let count = d.signals.iter().find(|s| s.path == "H.dut.count").unwrap();
    assert_eq!(count.width, 8);
    assert!(d.signals.iter().any(|s| s.path == "H.dut.value"));
    // One combinational driver: count = value.
    assert_eq!(d.drivers.len(), 1);
    // One event block (clk.rising()) with two next-state updates.
    assert_eq!(d.event_blocks.len(), 1);
    assert_eq!(d.event_blocks[0].updates.len(), 2);
}

#[test]
/// Nested instances lower with their port connections resolved.
fn lowers_nested_instances_with_connections() {
    // Add2 instantiates two Add1s wired through `mid`. Each instance must
    // get its own signals, and every port connection must become a driver.
    let src = "module m;\n\
            entity Add1 { a: unsigned[8] in, y: unsigned[8] out }\n\
            impl Add1 { y = a + 1; }\n\
            entity Add2 { a: unsigned[8] in, y: unsigned[8] out }\n\
            impl Add2 {\n\
              let mid: unsigned[8];\n\
              let s1: Add1 = { .a = a, .y = mid };\n\
              let s2: Add1 = { .a = mid, .y = y };\n\
            }\n\
            #[test] entity T {}\n\
            impl T {\n\
              let a: unsigned[8] = 10;\n\
              let y: unsigned[8];\n\
              let dut: Add2 = { .a = a, .y = y };\n\
            }\n";
    let d = lower_src(src);
    let id = |path: &str| {
        d.signals
            .iter()
            .position(|s| s.path == path)
            .map(|i| SignalId(i as u32))
    };
    // Two distinct Add1 instances, each with its own signals.
    assert!(id("T.dut.s1.a").is_some() && id("T.dut.s1.y").is_some());
    assert!(id("T.dut.s2.a").is_some() && id("T.dut.s2.y").is_some());
    // Every connection is a driver: `in` ports read the parent, `out`
    // ports drive it.
    let wired = |target: &str, source: &str| {
        let (t, s) = (id(target).unwrap(), id(source).unwrap());
        d.drivers
            .iter()
            .any(|dr| dr.target == t && matches!(&dr.expr, Expr::Current(x) if *x == s))
    };
    assert!(wired("T.dut.s1.a", "T.dut.a"), "s1.a <- a");
    assert!(wired("T.dut.mid", "T.dut.s1.y"), "mid <- s1.y");
    assert!(wired("T.dut.s2.a", "T.dut.mid"), "s2.a <- mid");
    assert!(wired("T.dut.y", "T.dut.s2.y"), "y <- s2.y");
}

#[test]
/// A testbench literal connected directly to a DUT input is hardware input
/// semantics, not private setup performed by one native-test backend.
fn testbench_value_connections_become_canonical_drivers() {
    let source = "module m;
         entity Echo { input: unsigned[12] in, output: unsigned[12] out }
         impl Echo { output = input; }
         #[test] entity T {}
         impl T {
           let source: unsigned[12] = 7;
           let observed: unsigned[12];
           let dut: Echo = { .input = 0xA5C, .output = observed };
           let dynamic: Echo = { .input = source + 1 };
         }";
    let diagnostics = lower_diags(source);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.contains("no value named `source`")),
        "testbench-dependent connections must not enter hardware lowering: {diagnostics:#?}"
    );
    let design = lower_src(source);
    let input = design
        .signals
        .iter()
        .position(|signal| signal.path == "T.dut.input")
        .map(|index| SignalId(index as u32))
        .expect("DUT input signal");
    let driver = design
        .drivers
        .iter()
        .find(|driver| driver.target == input)
        .expect("literal input connection must become a driver");

    assert!(matches!(driver.expr, Expr::Const(0xA5C)));
    assert!(
        !design
            .drivers
            .iter()
            .any(|driver| { design.signals[driver.target.0 as usize].path == "T.dynamic.input" }),
        "a connection reading testbench storage stays with Process IR"
    );
    assert!(design.validate().is_empty(), "{:#?}", design.validate());
}

#[test]
/// An `if` expression lowers to a select rather than a branch.
fn if_expression_lowers_to_select() {
    let d = lower_src(
        "module m;\n\
             entity Mux { sel: Bit in, a: unsigned[8] in, b: unsigned[8] in, y: unsigned[8] out }\n\
             impl Mux { y = if sel { a } else { b }; }\n\
             #[test] entity T {}\n\
             impl T { let sel: Bit; let a: unsigned[8]; let b: unsigned[8]; let y: unsigned[8];\n\
               let dut: Mux = { .sel = sel, .a = a, .b = b, .y = y }; }\n",
    );
    let y = d
        .signals
        .iter()
        .position(|s| s.path == "T.dut.y")
        .map(|i| SignalId(i as u32))
        .unwrap();
    let dr = d.drivers.iter().find(|dr| dr.target == y).unwrap();
    assert!(
        matches!(&dr.expr, Expr::Select { .. }),
        "if-expression must lower to a select"
    );
}

/// An expression that is not a place must be distinguished from supported
/// field/index targets.
#[test]
fn a_bad_assignment_target_says_which_kind_it_is() {
    let not_a_place = lower_diags(
        "module m;
             fn f(x: unsigned[8]) -> unsigned[8] { return x; }
             entity E { a: unsigned[8] in, y: unsigned[8] out }
             impl E { f(a) = a; y = a; }",
    );
    assert!(
        not_a_place
            .iter()
            .any(|d| d.contains("E-P018") && d.contains("`f(a)` cannot be assigned to")),
        "{not_a_place:#?}"
    );
}

/// Nested array indices are independent runtime mux dimensions on reads
/// and a conjunction of match gates on writes.
#[test]
fn chained_runtime_indices_lower_to_muxes_and_gated_writes() {
    let source = "module m;
             entity E {
               a: unsigned[8] in, row: integer in, col: integer in,
               y: unsigned[8] out
             }
             impl E {
               let mm: unsigned[8][2][2];
               mm[row][col] = a;
               y = mm[row][col];
             }";
    let diags = lower_diags(source);
    assert!(
        diags
            .iter()
            .all(|diagnostic| !diagnostic.contains("E-P017")),
        "nested runtime access should no longer be rejected: {diags:#?}"
    );
    let design = lower_src(source);
    assert!(design.validate().is_empty(), "{:#?}", design.validate());
    let matrix_writes: Vec<_> = design
        .drivers
        .iter()
        .filter(|driver| {
            design.signals[driver.target.0 as usize]
                .path
                .contains(".mm[")
        })
        .collect();
    assert_eq!(matrix_writes.len(), 4, "one gated write per scalar leaf");
    assert!(
        matrix_writes.iter().all(|driver| driver.cond.is_some()),
        "every leaf write must test both runtime indices"
    );
    let output = design
        .signals
        .iter()
        .position(|signal| signal.path == "E.y")
        .map(|index| SignalId(index as u32))
        .unwrap();
    let read = design
        .drivers
        .iter()
        .find(|driver| driver.target == output)
        .unwrap();
    assert!(
        matches!(read.expr, Expr::Select { .. }),
        "read is a mux tree"
    );
}

#[test]
/// A runtime index followed by a struct field reaches the right scalar leaf.
fn runtime_index_then_struct_field_reaches_the_scalar_leaf() {
    let source = "module m;
             struct Packet { data: unsigned[8], tag: unsigned[4] }
             entity E {
               a: unsigned[8] in, slot: integer in, y: unsigned[8] out
             }
             impl E {
               let packets: Packet[2];
               packets[slot].data = a;
               y = packets[slot].data;
             }";
    let diagnostics = lower_diags(source);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.contains("E-P017")),
        "{diagnostics:#?}"
    );
    assert!(lower_src(source).validate().is_empty());
}

#[test]
/// Packed vector indices use the declared labels and the corresponding
/// storage offsets, which differ for a descending range.
fn packed_vector_indices_use_declared_labels_and_storage_offsets() {
    let design = lower_src(
        "module m; use std::bits::unsigned; use std::logic::Logic;
             entity E {
               value: unsigned[15..8] in,
               high: Logic out, low: Logic out, whole: unsigned[8] out
             }
             impl E { high = value[15]; low = value[8]; whole = value[15..8]; }",
    );
    let driver = |path: &str| {
        let signal = design
            .signals
            .iter()
            .position(|signal| signal.path == path)
            .map(|index| SignalId(index as u32))
            .unwrap();
        &design
            .drivers
            .iter()
            .find(|driver| driver.target == signal)
            .unwrap()
            .expr
    };
    assert!(matches!(driver("E.high"), Expr::Slice { hi: 7, lo: 7, .. }));
    assert!(matches!(driver("E.low"), Expr::Slice { hi: 0, lo: 0, .. }));
    assert!(matches!(
        driver("E.whole"),
        Expr::Slice { hi: 7, lo: 0, .. }
    ));
    assert!(design.validate().is_empty(), "{:#?}", design.validate());
}

#[test]
/// A runtime packed bit read and write updates both the value plane and the
/// metavalue companion.
fn runtime_packed_bit_read_write_updates_value_and_metavalue_planes() {
    let source = "module m; use std::bits::unsigned; use std::logic::{Bit, Logic};
             entity E {
               clk: Bit in, index: unsigned[5] in, data: Logic in, q: Logic out
             }
             impl E {
               let word: unsigned[15..8] = \"00000000\";
               if clk.rising() { word[index] = data; }
               q = word[index];
             }";
    let diagnostics = lower_diags(source);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.contains("E-P017")),
        "{diagnostics:#?}"
    );
    let design = lower_src(source);
    let word = design
        .signals
        .iter()
        .position(|signal| signal.path == "E.word")
        .unwrap() as u32;
    let meta = *design
        .meta_of
        .get(&word)
        .expect("runtime writes need a companion");
    let event = design.event_blocks.first().expect("clocked bit write");
    assert_eq!(
        event
            .updates
            .iter()
            .filter(|update| update.target.0 == word)
            .count(),
        1,
        "one checked masked value update for the packed storage leaf"
    );
    assert_eq!(
        event
            .updates
            .iter()
            .filter(|update| update.target.0 == meta)
            .count(),
        1,
        "one checked masked companion update for the selected nibble"
    );
    let q = design
        .signals
        .iter()
        .position(|signal| signal.path == "E.q")
        .unwrap() as u32;
    let mut reads = Vec::new();
    read_set(
        &design
            .drivers
            .iter()
            .find(|driver| driver.target.0 == q)
            .expect("q driver")
            .expr,
        &mut reads,
    );
    assert!(reads.iter().any(|signal| signal.0 == word));
    assert!(reads.iter().any(|signal| signal.0 == meta));
    assert!(design.validate().is_empty(), "{:#?}", design.validate());
}

/// What reaches the site table, and in what order.
///
/// Every consumer uses this numbering, so reordering or duplicating entries
/// would still line up internally. What is asserted here is the *contents*: a
/// span only earns an index if
/// some assignment can be blamed through it, which is what leaves index 0
/// free to mean "fall back to the declaration". The order is pinned as the
/// documented shape rather than as a defence against drift.
#[test]
fn range_sites_indexes_only_blamable_assignments() {
    let span = |start: u32| crate::diag::Span::new(crate::diag::FileId(0), start..start + 1);
    let signal = |range: Option<(i64, i64)>| Signal {
        path: "s".into(),
        declaration_span: span(0),
        width: 8,
        real: false,
        integer: true,
        char: false,
        range,
        init: vec![0],
        enum_type: None,
    };
    let driver = |target: u32, at: Option<crate::diag::Span>| Driver {
        target: SignalId(target),
        cond: None,
        expr: Expr::Const(0),
        meta: None,
        ctx: 0,
        span: at,
    };
    let design = Design {
        // 0 is ranged, 1 is not.
        signals: vec![signal(Some((0, 10))), signal(None)],
        drivers: vec![
            driver(0, Some(span(100))),
            // A driver the lowering synthesized -- a port connection has no
            // line of its own, and must not take an index.
            driver(0, None),
            // An unranged target can never fail this way.
            driver(1, Some(span(200))),
            // The same statement lowered twice (one body, two instances)
            // is one site, rather than two indistinguishable diagnostics.
            driver(0, Some(span(100))),
            driver(0, Some(span(300))),
        ],
        event_blocks: vec![EventBlock {
            condition: Expr::Const(1),
            updates: vec![
                NextUpdate {
                    target: SignalId(0),
                    cond: None,
                    expr: Expr::Const(0),
                    meta: None,
                    span: Some(span(400)),
                },
                NextUpdate {
                    target: SignalId(1),
                    cond: None,
                    expr: Expr::Const(0),
                    meta: None,
                    span: Some(span(500)),
                },
            ],
            ctx: 0,
        }],
        ..Design::default()
    };
    let sites = design.range_sites();
    assert_eq!(
        sites,
        vec![span(100), span(300), span(400)],
        "drivers first in lowering order, then event updates"
    );
    // Drivers with no span, and spans on unranged targets, are absent --
    // which is what leaves site 0 free to mean "fall back to the
    // declaration".
    assert!(!sites.contains(&span(200)));
    assert!(!sites.contains(&span(500)));
}

#[test]
/// The storage arena carries the same identity discipline as the process
/// and value arenas: dense self-consistent ids, one declaration per name
/// per root, in-range initializers and bindings, and no repeated binding
/// edge. A `ProcessValueKind::Storage` must name one that exists. Several
/// signals may intentionally share a projection when a testbench value
/// fans out to several DUT ports.
///
/// Worth checking because storage is the one arena reachable without going
/// through a process, so a stale id here would otherwise surface only in a
/// backend.
fn process_storage_identity_is_validated() {
    let span = crate::diag::Span::new(FileId(0), 0..1);
    let storage = |id: u32, name: &str| ProcessStorage {
        id: ProcessStorageId(id),
        owner: crate::elab::InstanceId(0),
        name: name.to_string(),
        source: None,
        span,
        ty: None,
        layout: None,
        initializer: None,
        bindings: Vec::new(),
    };

    let mut ir = ProcessIr {
        storages: vec![storage(0, "a")],
        ..ProcessIr::default()
    };
    assert!(ir.validate(1).is_empty(), "a well-formed arena is accepted");

    // An id that disagrees with its slot.
    ir.storages = vec![storage(3, "a")];
    assert!(ir
        .validate(1)
        .iter()
        .any(|issue| issue.contains("is stored at index 0")));

    // One name declared twice in the same root.
    ir.storages = vec![storage(0, "a"), storage(1, "a")];
    assert!(ir
        .validate(1)
        .iter()
        .any(|issue| issue.contains("declared more than once")));

    // An initializer naming a value that does not exist.
    let mut bad = storage(0, "a");
    bad.initializer = Some(ProcessValueId(7));
    ir.storages = vec![bad];
    assert!(ir
        .validate(1)
        .iter()
        .any(|issue| issue.contains("initializer references invalid value")));

    // A binding to a signal outside the design, and a repeated edge.
    let mut bound = storage(0, "a");
    bound.bindings = vec![
        ProcessStorageBinding {
            projection: String::new(),
            signal: SignalId(9),
            direction: LayoutDirection::In,
        },
        ProcessStorageBinding {
            projection: String::new(),
            signal: SignalId(0),
            direction: LayoutDirection::In,
        },
        ProcessStorageBinding {
            projection: String::new(),
            signal: SignalId(0),
            direction: LayoutDirection::In,
        },
    ];
    ir.storages = vec![bound];
    let issues = ir.validate(1);
    assert!(issues.iter().any(|issue| issue.contains("invalid signal")));
    assert!(issues
        .iter()
        .any(|issue| issue.contains("to SignalId(0) more than once")));

    // A value referencing storage that was never declared.
    ir.storages = Vec::new();
    ir.values = vec![ProcessValue {
        span,
        ty: None,
        bit_width: None,
        kind: ProcessValueKind::Storage(ProcessStorageId(0)),
    }];
    assert!(ir
        .validate(1)
        .iter()
        .any(|issue| issue.contains("references invalid storage")));

    // A scalar width must always be usable as an LLVM integer width.
    ir.values[0].bit_width = Some(0);
    assert!(ir
        .validate(1)
        .iter()
        .any(|issue| issue.contains("zero packed width")));
}

#[test]
/// A statically sized aggregate is one packed Process value. Its recursive
/// layout describes labels and offsets, but cannot substitute for the value's
/// canonical width or silently disagree with it.
fn process_aggregate_layout_requires_canonical_packed_width() {
    let span = crate::diag::Span::new(FileId(0), 0..1);
    let byte = SourceLayout {
        span,
        kind: LayoutKind::Scalar {
            width: 8,
            domain: ScalarDomain::Bits,
            nominal: None,
            value_range: None,
        },
    };
    let pair = SourceLayout {
        span,
        kind: LayoutKind::Array {
            range: Some(LayoutRange { left: 1, right: 0 }),
            element: Box::new(byte),
        },
    };
    assert_eq!(pair.bit_width(), Some(16));
    assert_eq!(pair.packed_width(), Some(16));

    let mut ir = ProcessIr {
        values: vec![ProcessValue {
            span,
            ty: None,
            bit_width: None,
            kind: ProcessValueKind::Default,
        }],
        value_layouts: vec![Some(pair)],
        ..ProcessIr::default()
    };
    assert!(ir
        .validate(0)
        .iter()
        .any(|issue| issue.contains("declares packed width None")));

    ir.values[0].bit_width = Some(15);
    assert!(ir
        .validate(0)
        .iter()
        .any(|issue| issue.contains("layout requires 16")));

    ir.values[0].bit_width = Some(16);
    assert!(ir.validate(0).is_empty());

    let overwide = SourceLayout {
        span,
        kind: LayoutKind::Array {
            range: Some(LayoutRange {
                left: 0,
                right: i64::from(u32::MAX),
            }),
            element: Box::new(SourceLayout {
                span,
                kind: LayoutKind::Scalar {
                    width: 1,
                    domain: ScalarDomain::Bits,
                    nominal: None,
                    value_range: None,
                },
            }),
        },
    };
    assert_eq!(overwide.bit_width(), Some(u64::from(u32::MAX) + 1));
    assert_eq!(overwide.packed_width(), None);
}

#[test]
/// The target projection and right-hand value are one assignment snapshot.
/// Every runtime index and the value therefore need a packed representation
/// before a backend may begin mutating the selected root.
fn process_assignment_snapshot_requires_capturable_operands() {
    let span = crate::diag::Span::new(FileId(0), 0..1);
    let mut ir = ProcessIr {
        values: vec![
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(16),
                kind: ProcessValueKind::Local {
                    process: ProcessId(0),
                    local: ProcessLocalId(0),
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: None,
                kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![1])),
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Index {
                    base: ProcessValueId(0),
                    index: ProcessValueId(1),
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![7])),
            },
        ],
        processes: vec![ProcessCfg {
            id: ProcessId(0),
            root: crate::elab::InstanceId(0),
            owner: crate::elab::InstanceId(0),
            label: None,
            span,
            activation: ProcessActivation::TimeZero,
            region: ProcessRegion::Procedural,
            entry: ProcessBlockId(0),
            locals: vec![ProcessLocal {
                id: ProcessLocalId(0),
                name: "values".into(),
                source: None,
                span,
                ty: None,
                layout: None,
            }],
            blocks: vec![ProcessBlock {
                id: ProcessBlockId(0),
                instructions: vec![ProcessInstruction::Assign {
                    semantics: ProcessAssignment::ImmediateLocal,
                    driver_context: None,
                    target: ProcessValueId(2),
                    value: ProcessValueId(3),
                    span,
                }],
                terminator: ProcessTerminator::Return {
                    value: None,
                    span: Some(span),
                },
            }],
        }],
        ..ProcessIr::default()
    };

    let issues = ir.validate(0);
    assert!(
        issues
            .iter()
            .any(|issue| issue.contains("assignment snapshot index ProcessValueId(1)")),
        "{issues:?}"
    );

    ir.values[1].bit_width = Some(64);
    assert!(ir.validate(0).is_empty(), "{:#?}", ir.validate(0));

    ir.values[3].bit_width = None;
    let issues = ir.validate(0);
    assert!(
        issues
            .iter()
            .any(|issue| issue.contains("assignment snapshot value ProcessValueId(3)")),
        "{issues:?}"
    );

    ir.values[3].bit_width = Some(8);
    ir.values[2].bit_width = None;
    let issues = ir.validate(0);
    assert!(
        issues
            .iter()
            .any(|issue| issue.contains("assignment snapshot target ProcessValueId(2)")),
        "{issues:?}"
    );
}

#[test]
/// Validation accepts well-formed IR and flags each malformed shape.
fn validate_accepts_good_and_flags_bad_ir() {
    // A lowered counter is well-formed.
    assert!(lower_src(COUNTER).validate().is_empty());

    let sig = |w: u32| Signal {
        path: "s".into(),
        declaration_span: crate::diag::Span::new(crate::diag::FileId(0), 0..0),
        width: w,
        real: false,
        integer: false,
        char: false,
        range: None,
        init: vec![0],
        enum_type: None,
    };
    // Out-of-range signal id, an Unknown, a bad slice, and a width-0 signal.
    let bad = Design {
        signals: vec![sig(0)], // width 0 -> flagged
        drivers: vec![Driver {
            span: None,
            target: SignalId(9), // out of range
            cond: Some(Expr::Unknown),
            expr: Expr::Slice {
                base: Box::new(Expr::Current(SignalId(0))),
                hi: 1,
                lo: 3,
            },
            meta: None,
            ctx: 0,
        }],
        event_blocks: vec![],
        process_ir: Default::default(),
        process_labels: Default::default(),
        resolved_process_labels: Default::default(),
        enum_bases: HashMap::new(),
        enum_syms: HashMap::new(),
        new_defaults: Default::default(),
        logic_encodings: Default::default(),
        lookup_tables: Default::default(),
        base_dir: Default::default(),
        meta_of: Default::default(),
        metavalue_temps: Default::default(),
        array_element_enums: Default::default(),
        array_element_of_family: Default::default(),
        source_layouts: Default::default(),
    };
    let issues = bad.validate();
    assert!(
        issues.iter().any(|i| i.contains("unknown width")),
        "{issues:?}"
    );
    assert!(
        issues.iter().any(|i| i.contains("out of range")),
        "{issues:?}"
    );
    assert!(issues.iter().any(|i| i.contains("Unknown")), "{issues:?}");
    assert!(
        issues.iter().any(|i| i.contains("slice bounds")),
        "{issues:?}"
    );
    // Each issue names the signal it concerns. A driver's index in the
    // vector is an artefact of lowering: "driver 0 expr" sent the reader
    // to an IR dump to find out which line of their design it meant.
    assert!(
        issues
            .iter()
            .all(|i| i.contains("`s`") || i.contains("signal id")),
        "every issue names a signal: {issues:?}"
    );
}

#[test]
/// Packed logic tables are interned into compact lookups, with identical
/// tables shared.
fn packed_logic_tables_are_interned_as_compact_lookups() {
    let table = HashMap::from([((0, 0), 1), ((0, 1), 0), ((1, 0), 2), ((1, 1), 3)]);
    let lookup =
        |left, right| logic_binary_table_result(Expr::Current(left), Expr::Current(right), &table);
    let mut design = Design {
        drivers: vec![
            Driver {
                target: SignalId(2),
                cond: None,
                expr: lookup(SignalId(0), SignalId(1)),
                meta: None,
                ctx: 0,
                span: None,
            },
            Driver {
                target: SignalId(3),
                cond: None,
                expr: lookup(SignalId(1), SignalId(0)),
                meta: None,
                ctx: 0,
                span: None,
            },
        ],
        ..Design::default()
    };

    compact_lookup_writes(
        &mut design.drivers,
        &mut design.event_blocks,
        &mut design.lookup_tables,
    );

    assert_eq!(design.lookup_tables.len(), 1);
    assert_eq!(design.lookup_tables[0].element_width, 4);
    assert_eq!(design.lookup_tables[0].values, vec![1, 0, 2, 3]);
    for driver in &design.drivers {
        assert!(matches!(
            driver.expr,
            Expr::TableLookup {
                table: LookupTableId(0),
                ..
            }
        ));
    }
}

#[test]
/// The persisted leaf layout, not the source type, is what the backend
/// treats as authoritative for width.
fn persisted_leaf_layout_is_the_backend_width_authority() {
    let mut design = lower_src("module m; entity E { value: unsigned[8] in } impl E {}");
    let id = SignalId(
        design
            .signals
            .iter()
            .position(|signal| signal.path == "E.value")
            .expect("value signal") as u32,
    );
    assert_eq!(design.signal_width(id), Some(8));

    design.signals[id.0 as usize].width = 9;
    assert_eq!(
        design.signal_width(id),
        Some(8),
        "the persisted source layout, not duplicated Signal metadata, owns backend width"
    );
    let issues = design.validate();
    assert!(
        issues
            .iter()
            .any(|issue| issue.contains("disagrees with its source layout width 8")),
        "{issues:#?}"
    );
}

#[test]
/// Scheduled processes carry their sensitivity and write sets.
fn processes_carry_sensitivity_and_write_sets() {
    let d = lower_src(COUNTER);
    let sig = |path: &str| SignalId(d.signals.iter().position(|s| s.path == path).unwrap() as u32);
    let procs = d.processes();
    // A combinational process for `count = value` and one event process.
    let comb = procs
            .iter()
            .find(|p| matches!(&p.kind, ProcessKind::Comb { target, .. } if *target == sig("H.dut.count")))
            .unwrap();
    assert_eq!(comb.reads, vec![sig("H.dut.value")]);
    assert_eq!(comb.writes, vec![sig("H.dut.count")]);

    let event = procs
        .iter()
        .find(|p| matches!(p.kind, ProcessKind::Event { .. }))
        .unwrap();
    assert_eq!(event.labels, vec!["H.dut::update"]);
    // Sensitive to clk (edge condition), rst and en (update guards),
    // value (increment). Writes value.
    for s in ["H.dut.clk", "H.dut.rst", "H.dut.en", "H.dut.value"] {
        assert!(event.reads.contains(&sig(s)), "event not sensitive to {s}");
    }
    assert_eq!(event.writes, vec![sig("H.dut.value")]);
}

#[test]
/// A resolved process keeps every contributing driver's label, so a
/// diagnostic can name all of them.
fn resolved_process_keeps_every_contributing_label() {
    let d = lower_src(
        "module m;\n\
             impl<T: Resolve> Resolve for T[] {\n\
               fn resolve(self, rhs: T[]) -> T[] { return self; }\n\
             }\n\
             impl Resolve for Logic {\n\
               fn resolve(self, rhs: Logic) -> Logic {\n\
                 if self == 'Z' { return rhs; }\n\
                 return self;\n\
               }\n\
             }\n\
             entity E { q: unsigned[8] out }\n\
             impl E {\n\
               bit_one: process { q[1] = '1'; }\n\
               bit_three: process { q[3] = '1'; }\n\
             }\n",
    );
    assert!(d.validate().is_empty(), "{:?}", d.validate());
    let target = SignalId(
        d.signals
            .iter()
            .position(|signal| signal.path == "E.q")
            .unwrap() as u32,
    );
    let process = d
        .processes()
        .into_iter()
        .find(|process| process.writes == [target])
        .unwrap();
    assert_eq!(process.labels, ["E::bit_one", "E::bit_three"]);
    let rendered = d.to_ir_string();
    assert!(
        rendered.contains("[E::bit_one, E::bit_three]"),
        "{rendered}"
    );
}

#[test]
/// Composite and enum signals flatten into leaves with correct widths.
fn composite_and_enum_signals_flatten_with_widths() {
    let d = lower_src(
            "module m;\n\
             enum S { A, B, C }\n\
             struct P { flag: Bit, val: unsigned[8] }\n\
             entity E { p: P in, a: Bit[3] in, s: S out }\n\
             impl E {}\n\
             entity H {}\n\
             impl H { let p: P; let a: Bit[3]; let s: S; let dut: E = { .p = p, .a = a, .s = s }; }\n",
        );
    let width = |path: &str| d.signals.iter().find(|x| x.path == path).map(|x| x.width);
    assert_eq!(width("H.dut.p.flag"), Some(1)); // struct field
    assert_eq!(width("H.dut.p.val"), Some(8));
    assert_eq!(width("H.dut.a[0]"), Some(1)); // array element
    assert_eq!(width("H.dut.a[2]"), Some(1));
    assert_eq!(width("H.dut.s"), Some(2)); // enum repr width
}

#[test]
/// A partial bit-slice write updates only the addressed bits.
fn partial_bit_slice_write() {
    // `y = 0; y[3..0] = a` merges: low nibble = a, high bits held from 0.
    let d = lower_src(
        "module m;\n\
             entity E { a: unsigned[4] in, y: unsigned[8] out }\n\
             impl E { process { y = 0; y[3..0] = a; } }\n\
             entity H {}\n\
             impl H { let a: unsigned[4]; let y: unsigned[8]; let dut: E = { .a = a, .y = y }; }\n",
    );
    // The y driver should be a read-modify-write (an Or of a masked base
    // and a shifted value), not a bare assignment.
    let dr = d
        .drivers
        .iter()
        .find(|dr| d.signals[dr.target.0 as usize].path == "H.dut.y")
        .unwrap();
    assert!(
        matches!(dr.expr, Expr::Binary { op: BinOp::Or, .. }),
        "slice write merges: {:?}",
        dr.expr
    );
}

#[test]
/// Concurrent resolved slices lower without the expression growth that
/// resolution folding used to produce.
fn concurrent_resolved_slices_lower_without_expression_explosion() {
    // Each bare assignment is its own concurrent driver context. Folding
    // three contexts used to repeatedly inline and clone Logic::resolve:
    // two produced about 500 KiB of IR and the third exhausted the host.
    let d = lower_src(
        "module m;\n\
             impl<T: Resolve> Resolve for T[] {\n\
               fn resolve(self, rhs: T[]) -> T[] { return self; }\n\
             }\n\
             impl Resolve for Logic {\n\
               fn resolve(self, rhs: Logic) -> Logic {\n\
                 if self == 'Z' { return rhs; }\n\
                 if rhs == 'Z' { return self; }\n\
                 if self == rhs { return self; }\n\
                 return 'X';\n\
               }\n\
             }\n\
             entity E { q: unsigned[8] out }\n\
             impl E { q[1] = '1'; q[3] = '1'; q[5] = '1'; }\n",
    );
    assert!(d.validate().is_empty());
    let rendered = d.to_ir_string();
    // Source hardware lowering now includes the canonical arena dump too.
    // Keep the original bound on the expanded expression representation,
    // rather than confusing verbose per-node CFG metadata with tree growth.
    let canonical = d.process_ir.to_ir_string();
    let expressions = rendered
        .strip_suffix(&canonical)
        .expect("canonical dump suffix");
    assert!(
        expressions.len() < 250_000,
        "three resolved slice contexts expanded to {} bytes",
        expressions.len()
    );
    // CFG construction contributes one place per write and exactly one node
    // per normalized expression node. It must not expand the source draft a
    // second time when constructing sensitivities or attaching test metadata.
    let mut expected_nodes = d.drivers.len();
    let mut pending = d
        .drivers
        .iter()
        .flat_map(|driver| std::iter::once(&driver.expr).chain(driver.cond.iter()))
        .collect::<Vec<_>>();
    while let Some(expression) = pending.pop() {
        expected_nodes += 1;
        match expression {
            Expr::Canonical { .. } => panic!("legacy shape fixture was not expanded"),
            Expr::Unary { rhs, .. }
            | Expr::Slice { base: rhs, .. }
            | Expr::TableLookup { index: rhs, .. } => pending.push(rhs),
            Expr::Binary { lhs, rhs, .. } => pending.extend([lhs.as_ref(), rhs.as_ref()]),
            Expr::Select { cond, then, els } => {
                pending.extend([cond.as_ref(), then.as_ref(), els.as_ref()])
            }
            Expr::CheckedIndex { index, valid, .. } => {
                pending.extend([index.as_ref(), valid.as_ref()])
            }
            Expr::MetaCmp {
                inner, operands, ..
            } => {
                pending.push(inner);
                pending.extend(operands);
            }
            Expr::CCall { args, .. } => pending.extend(args),
            Expr::Const(_)
            | Expr::WideConst(_)
            | Expr::Real(_)
            | Expr::Logic(_)
            | Expr::Current(_)
            | Expr::Old(_)
            | Expr::Event(_)
            | Expr::Unknown => {}
        }
    }
    assert_eq!(
        d.process_ir.values.len(),
        expected_nodes,
        "canonical construction must remain linear in normalized source writes"
    );
}

/// A derived enum width has to hold every *value*, not one code per
/// variant. `Hi = 9` in a two-variant enum needs four bits; counting
/// variants alone gave one, silently truncating the value to 1. The old
/// `: unsigned[4]` repr annotation used to paper over this.
#[test]
fn enum_width_covers_explicit_discriminants() {
    let d = lower_src(
        "module m;\n\
             enum Code { Lo = 1, Hi = 9 }\n\
             entity E { c: Code out }\n\
             impl E { c = Code::Hi; }\n\
             entity H {}\n\
             impl H { let c: Code; let dut: E = { .c = c }; }\n",
    );
    let sig = d.signals.iter().find(|s| s.path == "H.dut.c").unwrap();
    assert_eq!(sig.width, 4, "width must hold the largest discriminant");
}

#[test]
/// A newtype enum takes its base's width.
fn newtype_enum_takes_its_base_width() {
    // A derived enum is a newtype (§3.28) — same variants, so the same
    // width. Four variants in the base, two bits in the derived type.
    let d = lower_src(
        "module m;\n\
             enum Base { A, B, C, D }\n\
             enum Ext(Base);\n\
             entity E { x: Ext out }\n\
             impl E { x = Ext::A; }\n\
             entity H {}\n\
             impl H { let x: Ext; let dut: E = { .x = x }; }\n",
    );
    let sig = d.signals.iter().find(|s| s.path == "H.dut.x").unwrap();
    assert_eq!(sig.width, 2, "the newtype carries the base's width");
}
