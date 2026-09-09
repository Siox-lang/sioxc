//! Ahead-of-time object emission (stage B5).
//!
//! Emits the design module as a native object file via `TargetMachine`. The
//! object exports the `sx_*` C ABI, so a runtime `main` (generated from the
//! testbench, or hand-written) links against it to form a
//! standalone native simulator. Compiling the testbench stimulus into that
//! `main` is the follow-on increment.

use std::path::Path;

use inkwell::context::Context;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::OptimizationLevel;

use siox::ir::Design;

use super::emit::build_module;

/// The `(cpu, features)` the target machine is built for. With the `simd`
/// feature it is the host's own CPU and native feature set — so the backend may
/// use the widest vector registers the machine has (AVX / AVX-512 → 256 / 512-
/// bit). Without it, a portable baseline (`generic` x86-64, SSE2 128-bit), so
/// objects run anywhere.
fn target_cpu_features() -> (String, String) {
    if cfg!(feature = "simd") {
        (
            TargetMachine::get_host_cpu_name()
                .to_str()
                .unwrap_or("generic")
                .to_string(),
            TargetMachine::get_host_cpu_features()
                .to_str()
                .unwrap_or("")
                .to_string(),
        )
    } else {
        ("generic".to_string(), String::new())
    }
}

/// A `TargetMachine` for codegen, tuned per the `simd` feature (see
/// [`target_cpu_features`]).
pub fn host_target_machine() -> Result<TargetMachine, String> {
    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| format!("target init failed: {e}"))?;
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
    let (cpu, features) = target_cpu_features();
    target
        .create_target_machine(
            &triple,
            &cpu,
            &features,
            OptimizationLevel::Default,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| "failed to create target machine".to_string())
}

/// Emit `design` as a native object file at `path` (`.o`). The object exports
/// `sx_reset`/`sx_set`/`sx_read`/`sx_settle`.
pub fn emit_object(design: &Design, path: &Path) -> Result<(), String> {
    let tm = host_target_machine()?;
    let ctx = Context::create();
    let module = build_module(&ctx, design)?;
    super::emit::optimize_module(&module, &tm)?;
    tm.write_to_file(&module, FileType::Object, path)
        .map_err(|e| format!("object emission failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use siox::diag::{FileId, Span};
    use siox::elab::InstanceId;
    use siox::ir::{
        BinOp, Driver, Expr, LayoutDirection, LayoutField, LayoutKind, LookupTable, LookupTableId,
        ProcessActivation, ProcessAggregateField, ProcessAssignment, ProcessBinaryOp, ProcessBlock,
        ProcessBlockId, ProcessCfg, ProcessId, ProcessInstruction, ProcessIr, ProcessLocal,
        ProcessLocalId, ProcessNumber, ProcessSensitivity, ProcessSignalState, ProcessStorage,
        ProcessStorageBinding, ProcessStorageId, ProcessSuspendOp, ProcessTerminator, ProcessTest,
        ProcessValue, ProcessValueId, ProcessValueKind, ScalarDomain, Signal, SignalId,
        SourceLayout,
    };
    use siox::resolve::DefId;
    use std::process::Command;

    /// A minimal test signal: a plain bit vector of `width` at `path`, with no
    /// range, enum, or initializer.
    fn sig(path: &str, width: u32) -> Signal {
        Signal {
            path: path.into(),
            declaration_span: siox::diag::Span::new(siox::diag::FileId(0), 0..0),
            width,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: vec![0],
            enum_type: None,
        }
    }

    /// Emit an adder to a native object, link a C `main` that drives it, and
    /// run — proving AOT object emission + linking + native execution.
    #[test]
    fn object_links_and_runs() {
        // clang is required to link/run; skip cleanly if it is absent.
        if Command::new("clang").arg("--version").output().is_err() {
            eprintln!("skipping object_links_and_runs: clang not found");
            return;
        }

        let span = Span::new(FileId(0), 0..0);
        let process_ir = ProcessIr {
            processes: vec![ProcessCfg {
                id: ProcessId(0),
                root: InstanceId(0),
                owner: InstanceId(0),
                label: Some("a-gt-127".into()),
                span,
                activation: ProcessActivation::TimeZero,
                entry: ProcessBlockId(0),
                locals: vec![],
                blocks: vec![
                    ProcessBlock {
                        id: ProcessBlockId(0),
                        instructions: vec![],
                        terminator: ProcessTerminator::Branch {
                            condition: ProcessValueId(14),
                            then_block: ProcessBlockId(1),
                            else_block: ProcessBlockId(2),
                        },
                    },
                    ProcessBlock {
                        id: ProcessBlockId(1),
                        instructions: vec![],
                        terminator: ProcessTerminator::Stop { span },
                    },
                    ProcessBlock {
                        id: ProcessBlockId(2),
                        instructions: vec![],
                        terminator: ProcessTerminator::Finish { span },
                    },
                ],
            }],
            values: vec![
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(8),
                    kind: ProcessValueKind::Signal {
                        signals: vec![SignalId(0)],
                        state: ProcessSignalState::Current,
                    },
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(8),
                    kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![127])),
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(8),
                    kind: ProcessValueKind::ForeignCall {
                        name: "sx_test_threshold".into(),
                        arguments: vec![ProcessValueId(1)],
                        float_arguments: vec![false],
                        integer_arguments: vec![false],
                        float_result: false,
                        integer_result: false,
                    },
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(1),
                    kind: ProcessValueKind::Binary {
                        operation: ProcessBinaryOp::Gt,
                        left: ProcessValueId(0),
                        right: ProcessValueId(2),
                    },
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(1),
                    kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![1])),
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(4),
                    kind: ProcessValueKind::TableLookup {
                        table: LookupTableId(0),
                        index: ProcessValueId(4),
                    },
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(4),
                    kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![9])),
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(1),
                    kind: ProcessValueKind::Binary {
                        operation: ProcessBinaryOp::Eq,
                        left: ProcessValueId(5),
                        right: ProcessValueId(6),
                    },
                },
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
                    kind: ProcessValueKind::Select {
                        condition: ProcessValueId(7),
                        then_value: ProcessValueId(3),
                        else_value: ProcessValueId(8),
                    },
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(2),
                    kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![3])),
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(1),
                    kind: ProcessValueKind::Binary {
                        operation: ProcessBinaryOp::SignedGt,
                        left: ProcessValueId(10),
                        right: ProcessValueId(8),
                    },
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(3),
                    kind: ProcessValueKind::Concat(vec![
                        ProcessValueId(9),
                        ProcessValueId(7),
                        ProcessValueId(11),
                    ]),
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(3),
                    kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![7])),
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(1),
                    kind: ProcessValueKind::Binary {
                        operation: ProcessBinaryOp::Eq,
                        left: ProcessValueId(12),
                        right: ProcessValueId(13),
                    },
                },
            ],
            ..ProcessIr::default()
        };
        let design = Design {
            signals: vec![sig("E.a", 8), sig("E.b", 8), sig("E.y", 8)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(2),
                cond: None,
                expr: Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::Current(SignalId(0))),
                    rhs: Box::new(Expr::Current(SignalId(1))),
                },
                meta: None,
            }],
            event_blocks: vec![],
            process_ir,
            process_labels: Default::default(),
            resolved_process_labels: Default::default(),
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            logic_encodings: Default::default(),
            lookup_tables: vec![LookupTable {
                element_width: 4,
                values: vec![7, 9],
            }],
            base_dir: Default::default(),
            meta_of: Default::default(),
            metavalue_temps: Default::default(),
            array_element_enums: Default::default(),
            array_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };

        let dir = std::env::temp_dir().join(format!("siox_aot_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let obj = dir.join("design.o");
        let main_c = dir.join("main.c");
        let bin = dir.join("sim");

        emit_object(&design, &obj).unwrap();
        assert!(
            obj.exists() && std::fs::metadata(&obj).unwrap().len() > 0,
            "empty object"
        );

        std::fs::write(
            &main_c,
            r#"
extern void sx_reset(void);
extern void sx_set(unsigned, unsigned long long);
extern unsigned long long sx_read(unsigned);
extern void sx_settle(void);
typedef unsigned char (*sx_process_entry)(unsigned resume_block);
extern sx_process_entry const sx_process_entries[];
extern const unsigned sx_process_initial_blocks[];
unsigned long long sx_test_threshold(unsigned long long value) { return value; }
signed main(void) {
    sx_reset();
    sx_set(0, 30); sx_set(1, 12); sx_settle();
    if (sx_read(2) != 42) return 1;
    if (sx_process_entries[0](sx_process_initial_blocks[0]) != 3) return 3;
    sx_set(0, 200); sx_set(1, 100); sx_settle();   /* wraps at 8 bits */
    if (sx_read(2) != (300 % 256)) return 2;
    if (sx_process_entries[0](sx_process_initial_blocks[0]) != 2) return 4;
    return 0;
}
"#,
        )
        .unwrap();

        let link = Command::new("clang")
            .args([
                main_c.to_str().unwrap(),
                obj.to_str().unwrap(),
                "-o",
                bin.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            link.status.success(),
            "link failed: {}",
            String::from_utf8_lossy(&link.stderr)
        );

        let run = Command::new(&bin).status().unwrap();
        assert!(run.success(), "native sim returned {:?}", run.code());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    /// Direct process assignments remain invisible until the scheduler's
    /// commit boundary, override in source order, and update old/event state
    /// over every ABI word. An unsupported block must publish no partial write.
    fn process_staged_writes_link_and_commit_atomically() {
        if Command::new("clang").arg("--version").output().is_err() {
            eprintln!("skipping process_staged_writes_link_and_commit_atomically: clang not found");
            return;
        }

        let span = Span::new(FileId(0), 0..0);
        let return_none = || ProcessTerminator::Return {
            value: None,
            span: None,
        };
        let assignment = |value| ProcessInstruction::Assign {
            semantics: ProcessAssignment::StagedSignal,
            driver_context: Some(7),
            target: ProcessValueId(0),
            value: ProcessValueId(value),
            span,
        };
        let process_ir = ProcessIr {
            processes: vec![
                ProcessCfg {
                    id: ProcessId(0),
                    root: InstanceId(0),
                    owner: InstanceId(0),
                    label: Some("wide-writer".into()),
                    span,
                    activation: ProcessActivation::TimeZero,
                    entry: ProcessBlockId(0),
                    locals: vec![],
                    blocks: vec![ProcessBlock {
                        id: ProcessBlockId(0),
                        instructions: vec![assignment(1), assignment(2)],
                        terminator: return_none(),
                    }],
                },
                ProcessCfg {
                    id: ProcessId(1),
                    root: InstanceId(0),
                    owner: InstanceId(0),
                    label: Some("edge-observer".into()),
                    span,
                    activation: ProcessActivation::TimeZero,
                    entry: ProcessBlockId(0),
                    locals: vec![],
                    blocks: vec![
                        ProcessBlock {
                            id: ProcessBlockId(0),
                            instructions: vec![],
                            terminator: ProcessTerminator::Branch {
                                condition: ProcessValueId(10),
                                then_block: ProcessBlockId(1),
                                else_block: ProcessBlockId(2),
                            },
                        },
                        ProcessBlock {
                            id: ProcessBlockId(1),
                            instructions: vec![],
                            terminator: ProcessTerminator::Stop { span },
                        },
                        ProcessBlock {
                            id: ProcessBlockId(2),
                            instructions: vec![],
                            terminator: ProcessTerminator::Finish { span },
                        },
                    ],
                },
                ProcessCfg {
                    id: ProcessId(2),
                    root: InstanceId(0),
                    owner: InstanceId(0),
                    label: Some("unsupported-is-transactional".into()),
                    span,
                    activation: ProcessActivation::TimeZero,
                    entry: ProcessBlockId(0),
                    locals: vec![],
                    blocks: vec![
                        ProcessBlock {
                            id: ProcessBlockId(0),
                            instructions: vec![assignment(2)],
                            terminator: ProcessTerminator::Suspend {
                                operation: ProcessSuspendOp::Await,
                                arguments: vec![],
                                resume: ProcessBlockId(1),
                                span,
                            },
                        },
                        ProcessBlock {
                            id: ProcessBlockId(1),
                            instructions: vec![],
                            terminator: return_none(),
                        },
                    ],
                },
            ],
            values: vec![
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(65),
                    kind: ProcessValueKind::Signal {
                        signals: vec![SignalId(0)],
                        state: ProcessSignalState::Current,
                    },
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(65),
                    kind: ProcessValueKind::BitString {
                        width: 65,
                        words: vec![5, 0],
                    },
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(65),
                    kind: ProcessValueKind::BitString {
                        width: 65,
                        words: vec![0x0123_4567_89ab_cdef, 1],
                    },
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(65),
                    kind: ProcessValueKind::Signal {
                        signals: vec![SignalId(0)],
                        state: ProcessSignalState::Old,
                    },
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(65),
                    kind: ProcessValueKind::Signal {
                        signals: vec![SignalId(0)],
                        state: ProcessSignalState::Current,
                    },
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(1),
                    kind: ProcessValueKind::Signal {
                        signals: vec![SignalId(0)],
                        state: ProcessSignalState::Event,
                    },
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(65),
                    kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![0])),
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(1),
                    kind: ProcessValueKind::Binary {
                        operation: ProcessBinaryOp::Eq,
                        left: ProcessValueId(3),
                        right: ProcessValueId(6),
                    },
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(1),
                    kind: ProcessValueKind::BitSlice {
                        base: ProcessValueId(4),
                        high: 64,
                        low: 64,
                    },
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(1),
                    kind: ProcessValueKind::Binary {
                        operation: ProcessBinaryOp::And,
                        left: ProcessValueId(5),
                        right: ProcessValueId(7),
                    },
                },
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(1),
                    kind: ProcessValueKind::Binary {
                        operation: ProcessBinaryOp::And,
                        left: ProcessValueId(8),
                        right: ProcessValueId(9),
                    },
                },
            ],
            ..ProcessIr::default()
        };
        let design = Design {
            signals: vec![sig("Wide.value", 65)],
            process_ir,
            ..Design::default()
        };

        let dir = std::env::temp_dir().join(format!("siox_process_stage_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let object = dir.join("design.o");
        let main_c = dir.join("main.c");
        let binary = dir.join("sim");
        emit_object(&design, &object).unwrap();
        std::fs::write(
            &main_c,
            r#"
extern void sx_reset(void);
extern unsigned long long sx_read_word(unsigned, unsigned);
extern unsigned char sx_process_commit(void);
extern unsigned char sx_process_changed(unsigned);
typedef unsigned char (*sx_process_entry)(unsigned resume_block);
extern sx_process_entry const sx_process_entries[];
extern const unsigned sx_process_initial_blocks[];
signed main(void) {
    sx_reset();
    if (sx_process_entries[0](sx_process_initial_blocks[0]) != 0) return 1;
    if (sx_read_word(0, 0) != 0 || sx_read_word(0, 1) != 0) return 2;
    if (sx_process_commit() != 1 || sx_process_changed(0) != 1 || sx_process_changed(99) != 0) return 3;
    if (sx_read_word(0, 0) != 0x0123456789abcdefULL || sx_read_word(0, 1) != 1) return 4;
    if (sx_process_entries[1](sx_process_initial_blocks[1]) != 2) return 5;
    if (sx_process_entries[0](sx_process_initial_blocks[0]) != 0) return 6;
    if (sx_process_commit() != 0 || sx_process_changed(0) != 0) return 7;
    if (sx_process_entries[1](sx_process_initial_blocks[1]) != 3) return 8;
    if (sx_process_entries[2](sx_process_initial_blocks[2]) != 255) return 9;
    if (sx_process_commit() != 0) return 10;
    return 0;
}
"#,
        )
        .unwrap();
        let link = Command::new("clang")
            .args([
                main_c.to_str().unwrap(),
                object.to_str().unwrap(),
                "-o",
                binary.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            link.status.success(),
            "link failed: {}",
            String::from_utf8_lossy(&link.stderr)
        );
        let run = Command::new(&binary).status().unwrap();
        assert!(
            run.success(),
            "native staged process returned {:?}",
            run.code()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    /// Scalar process locals update immediately, persistent storage survives
    /// entry calls, and an input binding reaches the DUT only at commit.
    fn process_locals_and_storage_execute_with_immediate_semantics() {
        if Command::new("clang").arg("--version").output().is_err() {
            eprintln!(
                "skipping process_locals_and_storage_execute_with_immediate_semantics: clang not found"
            );
            return;
        }

        let span = Span::new(FileId(0), 0..0);
        let packed = || SourceLayout {
            span,
            kind: LayoutKind::Packed {
                width: 8,
                family: "unsigned".into(),
                range: None,
                element_enum: None,
            },
        };
        let values = vec![
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![3])),
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Local {
                    process: ProcessId(0),
                    local: ProcessLocalId(0),
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Storage(ProcessStorageId(0)),
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![5])),
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(1),
                kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![1])),
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Binary {
                    operation: ProcessBinaryOp::Add,
                    left: ProcessValueId(1),
                    right: ProcessValueId(4),
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(3),
                kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![6])),
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(1),
                kind: ProcessValueKind::Binary {
                    operation: ProcessBinaryOp::Eq,
                    left: ProcessValueId(2),
                    right: ProcessValueId(6),
                },
            },
        ];
        let process_ir = ProcessIr {
            storages: vec![ProcessStorage {
                id: ProcessStorageId(0),
                owner: InstanceId(0),
                name: "input".into(),
                source: None,
                span,
                ty: None,
                layout: Some(packed()),
                initializer: Some(ProcessValueId(0)),
                bindings: vec![ProcessStorageBinding {
                    projection: String::new(),
                    signal: SignalId(0),
                    direction: LayoutDirection::In,
                }],
            }],
            processes: vec![ProcessCfg {
                id: ProcessId(0),
                root: InstanceId(0),
                owner: InstanceId(0),
                label: Some("immediate-state".into()),
                span,
                activation: ProcessActivation::TimeZero,
                entry: ProcessBlockId(0),
                locals: vec![ProcessLocal {
                    id: ProcessLocalId(0),
                    name: "temporary".into(),
                    source: None,
                    span,
                    ty: None,
                    layout: Some(packed()),
                }],
                blocks: vec![
                    ProcessBlock {
                        id: ProcessBlockId(0),
                        instructions: vec![
                            ProcessInstruction::Declare {
                                local: ProcessLocalId(0),
                                initializer: Some(ProcessValueId(3)),
                                span,
                            },
                            ProcessInstruction::Assign {
                                semantics: ProcessAssignment::ImmediateLocal,
                                driver_context: None,
                                target: ProcessValueId(1),
                                value: ProcessValueId(5),
                                span,
                            },
                            ProcessInstruction::Assign {
                                semantics: ProcessAssignment::ImmediateStorage,
                                driver_context: None,
                                target: ProcessValueId(2),
                                value: ProcessValueId(1),
                                span,
                            },
                        ],
                        terminator: ProcessTerminator::Branch {
                            condition: ProcessValueId(7),
                            then_block: ProcessBlockId(1),
                            else_block: ProcessBlockId(2),
                        },
                    },
                    ProcessBlock {
                        id: ProcessBlockId(1),
                        instructions: vec![],
                        terminator: ProcessTerminator::Stop { span },
                    },
                    ProcessBlock {
                        id: ProcessBlockId(2),
                        instructions: vec![],
                        terminator: ProcessTerminator::Finish { span },
                    },
                ],
            }],
            values,
            tests: vec![],
        };
        let design = Design {
            signals: vec![sig("D.input", 8)],
            process_ir,
            ..Design::default()
        };

        let dir = std::env::temp_dir().join(format!("siox_process_state_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let object = dir.join("design.o");
        let main_c = dir.join("main.c");
        let binary = dir.join("sim");
        emit_object(&design, &object).unwrap();
        std::fs::write(
            &main_c,
            r#"
extern void sx_reset(void);
extern unsigned long long sx_read(unsigned);
extern unsigned char sx_process_commit(void);
extern unsigned char sx_process_changed(unsigned);
extern unsigned char sx_process_storage_changed(unsigned);
typedef unsigned char (*sx_process_entry)(unsigned resume_block);
extern sx_process_entry const sx_process_entries[];
extern const unsigned sx_process_initial_blocks[];
signed main(void) {
    sx_reset();
    if (sx_read(0) != 0) return 1;
    if (sx_process_commit() != 1 || sx_read(0) != 3) return 2;
    if (sx_process_changed(0) != 1 || sx_process_storage_changed(0) != 0 ||
        sx_process_storage_changed(99) != 0) return 3;

    if (sx_process_entries[0](sx_process_initial_blocks[0]) != 2) return 4;
    if (sx_read(0) != 3) return 5;
    if (sx_process_commit() != 1 || sx_read(0) != 6) return 6;
    if (sx_process_changed(0) != 1 || sx_process_storage_changed(0) != 1) return 7;
    if (sx_process_commit() != 0) return 8;
    if (sx_process_changed(0) != 0 || sx_process_storage_changed(0) != 0) return 9;

    sx_reset();
    if (sx_process_commit() != 1 || sx_read(0) != 3) return 10;
    return 0;
}
"#,
        )
        .unwrap();
        let link = Command::new("clang")
            .args([
                main_c.to_str().unwrap(),
                object.to_str().unwrap(),
                "-o",
                binary.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            link.status.success(),
            "link failed: {}",
            String::from_utf8_lossy(&link.stderr)
        );
        let run = Command::new(&binary).status().unwrap();
        assert!(
            run.success(),
            "native process state returned {:?}",
            run.code()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    /// Recursive testbench storage is packed only inside the design object:
    /// constructors initialize and replace it field-by-field, input bindings
    /// stage the corresponding flattened leaves, and a following field read
    /// observes an immediate whole-aggregate assignment.
    fn process_aggregate_storage_executes_through_flattened_bindings() {
        if Command::new("clang").arg("--version").output().is_err() {
            eprintln!(
                "skipping process_aggregate_storage_executes_through_flattened_bindings: clang not found"
            );
            return;
        }

        let span = Span::new(FileId(0), 0..0);
        let byte = || SourceLayout {
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
            kind: LayoutKind::Struct {
                name: "Pair".into(),
                view: None,
                fields: vec![
                    LayoutField {
                        name: "a".into(),
                        direction: None,
                        layout: byte(),
                    },
                    LayoutField {
                        name: "b".into(),
                        direction: None,
                        layout: byte(),
                    },
                ],
            },
        };
        let output = SourceLayout {
            span,
            kind: LayoutKind::Struct {
                name: "Envelope".into(),
                view: None,
                fields: vec![
                    LayoutField {
                        name: "pad".into(),
                        direction: None,
                        layout: byte(),
                    },
                    LayoutField {
                        name: "payload".into(),
                        direction: None,
                        layout: pair.clone(),
                    },
                ],
            },
        };
        let pair_ty = siox::types::Ty::Named(siox::resolve::DefId(41));
        let number = |value| ProcessValue {
            span,
            ty: None,
            bit_width: Some(8),
            kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![value])),
        };
        let construct = |left, right| ProcessValue {
            span,
            ty: Some(pair_ty.clone()),
            bit_width: None,
            kind: ProcessValueKind::Construct {
                ty: Some(pair_ty.clone()),
                fields: vec![
                    ProcessAggregateField {
                        name: Some("a".into()),
                        value: Some(ProcessValueId(left)),
                        span,
                    },
                    ProcessAggregateField {
                        name: Some("b".into()),
                        value: Some(ProcessValueId(right)),
                        span,
                    },
                ],
                spread: None,
            },
        };
        let values = vec![
            number(1),
            number(2),
            construct(0, 1),
            ProcessValue {
                span,
                ty: Some(pair_ty.clone()),
                bit_width: Some(16),
                kind: ProcessValueKind::Storage(ProcessStorageId(0)),
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Field {
                    base: ProcessValueId(3),
                    field: "b".into(),
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Signal {
                    signals: vec![SignalId(2)],
                    state: ProcessSignalState::Current,
                },
            },
            number(3),
            number(4),
            construct(6, 7),
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Field {
                    base: ProcessValueId(3),
                    field: "a".into(),
                },
            },
            number(5),
            ProcessValue {
                span,
                ty: Some(pair_ty.clone()),
                bit_width: Some(16),
                kind: ProcessValueKind::Signal {
                    signals: vec![SignalId(3), SignalId(4)],
                    state: ProcessSignalState::Current,
                },
            },
            ProcessValue {
                span,
                ty: Some(pair_ty.clone()),
                bit_width: Some(16),
                kind: ProcessValueKind::Local {
                    process: ProcessId(0),
                    local: ProcessLocalId(0),
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Field {
                    base: ProcessValueId(12),
                    field: "a".into(),
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Field {
                    base: ProcessValueId(12),
                    field: "b".into(),
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Signal {
                    signals: vec![SignalId(5)],
                    state: ProcessSignalState::Current,
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Signal {
                    signals: vec![SignalId(6)],
                    state: ProcessSignalState::Current,
                },
            },
        ];
        let process_ir = ProcessIr {
            storages: vec![ProcessStorage {
                id: ProcessStorageId(0),
                owner: InstanceId(0),
                name: "pair".into(),
                source: None,
                span,
                ty: Some(pair_ty.clone()),
                layout: Some(pair.clone()),
                initializer: Some(ProcessValueId(2)),
                bindings: vec![
                    ProcessStorageBinding {
                        projection: ".a".into(),
                        signal: SignalId(0),
                        direction: LayoutDirection::In,
                    },
                    ProcessStorageBinding {
                        projection: ".b".into(),
                        signal: SignalId(1),
                        direction: LayoutDirection::In,
                    },
                ],
            }],
            values,
            processes: vec![ProcessCfg {
                id: ProcessId(0),
                root: InstanceId(0),
                owner: InstanceId(0),
                label: Some("aggregate".into()),
                span,
                activation: ProcessActivation::TimeZero,
                entry: ProcessBlockId(0),
                locals: vec![ProcessLocal {
                    id: ProcessLocalId(0),
                    name: "local_pair".into(),
                    source: None,
                    span,
                    ty: Some(pair_ty),
                    layout: Some(pair.clone()),
                }],
                blocks: vec![ProcessBlock {
                    id: ProcessBlockId(0),
                    instructions: vec![
                        ProcessInstruction::Declare {
                            local: ProcessLocalId(0),
                            initializer: Some(ProcessValueId(2)),
                            span,
                        },
                        ProcessInstruction::Assign {
                            semantics: ProcessAssignment::ImmediateLocal,
                            driver_context: None,
                            target: ProcessValueId(13),
                            value: ProcessValueId(10),
                            span,
                        },
                        ProcessInstruction::Assign {
                            semantics: ProcessAssignment::ImmediateStorage,
                            driver_context: None,
                            target: ProcessValueId(3),
                            value: ProcessValueId(8),
                            span,
                        },
                        ProcessInstruction::Assign {
                            semantics: ProcessAssignment::ImmediateStorage,
                            driver_context: None,
                            target: ProcessValueId(9),
                            value: ProcessValueId(10),
                            span,
                        },
                        ProcessInstruction::Assign {
                            semantics: ProcessAssignment::StagedSignal,
                            driver_context: None,
                            target: ProcessValueId(5),
                            value: ProcessValueId(4),
                            span,
                        },
                        ProcessInstruction::Assign {
                            semantics: ProcessAssignment::StagedSignal,
                            driver_context: None,
                            target: ProcessValueId(11),
                            value: ProcessValueId(8),
                            span,
                        },
                        ProcessInstruction::Assign {
                            semantics: ProcessAssignment::StagedSignal,
                            driver_context: None,
                            target: ProcessValueId(15),
                            value: ProcessValueId(13),
                            span,
                        },
                        ProcessInstruction::Assign {
                            semantics: ProcessAssignment::StagedSignal,
                            driver_context: None,
                            target: ProcessValueId(16),
                            value: ProcessValueId(14),
                            span,
                        },
                    ],
                    terminator: ProcessTerminator::Return {
                        value: None,
                        span: Some(span),
                    },
                }],
            }],
            ..ProcessIr::default()
        };
        let design = Design {
            signals: vec![
                sig("D.a", 8),
                sig("D.b", 8),
                sig("D.observed", 8),
                sig("D.out.payload.a", 8),
                sig("D.out.payload.b", 8),
                sig("D.local.a", 8),
                sig("D.local.b", 8),
                sig("D.out.pad", 8),
            ],
            source_layouts: std::collections::HashMap::from([("D.out".into(), output)]),
            process_ir,
            ..Design::default()
        };

        let dir =
            std::env::temp_dir().join(format!("siox_process_aggregate_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let object = dir.join("design.o");
        let main_c = dir.join("main.c");
        let binary = dir.join("sim");
        emit_object(&design, &object).unwrap();
        std::fs::write(
            &main_c,
            r#"
extern void sx_reset(void);
extern unsigned long long sx_read(unsigned);
extern unsigned char sx_process_commit(void);
typedef unsigned char (*sx_process_entry)(unsigned resume_block);
extern sx_process_entry const sx_process_entries[];
extern const unsigned sx_process_initial_blocks[];
signed main(void) {
    sx_reset();
    if (sx_process_commit() != 1 || sx_read(0) != 1 || sx_read(1) != 2) return 1;
    if (sx_process_entries[0](sx_process_initial_blocks[0]) != 0) return 2;
    if (sx_read(0) != 1 || sx_read(1) != 2 || sx_read(2) != 0 ||
        sx_read(3) != 0 || sx_read(4) != 0 || sx_read(5) != 0 || sx_read(6) != 0) return 3;
    if (sx_process_commit() != 1) return 4;
    if (sx_read(0) != 5 || sx_read(1) != 4 || sx_read(2) != 4 ||
        sx_read(3) != 3 || sx_read(4) != 4) return 5;
    if (sx_read(5) != 5 || sx_read(6) != 2) return 6;
    if (sx_read(7) != 0) return 7;
    return 0;
}
"#,
        )
        .unwrap();
        let link = Command::new("clang")
            .args([
                main_c.to_str().unwrap(),
                object.to_str().unwrap(),
                "-o",
                binary.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            link.status.success(),
            "link failed: {}",
            String::from_utf8_lossy(&link.stderr)
        );
        let run = Command::new(&binary).status().unwrap();
        assert!(
            run.success(),
            "native aggregate probe returned {:?}",
            run.code()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    /// A mixed concatenation evaluates once, then updates locals and storage
    /// immediately while keeping signal writes staged until commit.
    fn process_per_place_assignment_preserves_each_destination_class() {
        if Command::new("clang").arg("--version").output().is_err() {
            eprintln!(
                "skipping process_per_place_assignment_preserves_each_destination_class: clang not found"
            );
            return;
        }

        let span = Span::new(FileId(0), 0..0);
        let byte = || SourceLayout {
            span,
            kind: LayoutKind::Packed {
                width: 8,
                family: "unsigned".into(),
                range: None,
                element_enum: None,
            },
        };
        let number = |width, value| ProcessValue {
            span,
            ty: None,
            bit_width: Some(width),
            kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![value])),
        };
        let values = vec![
            number(8, 0),
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Local {
                    process: ProcessId(0),
                    local: ProcessLocalId(0),
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Storage(ProcessStorageId(0)),
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Signal {
                    signals: vec![SignalId(0)],
                    state: ProcessSignalState::Current,
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(24),
                kind: ProcessValueKind::Concat(vec![
                    ProcessValueId(1),
                    ProcessValueId(2),
                    ProcessValueId(3),
                ]),
            },
            number(24, 0x11_22_33),
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Signal {
                    signals: vec![SignalId(1)],
                    state: ProcessSignalState::Current,
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(8),
                kind: ProcessValueKind::Signal {
                    signals: vec![SignalId(2)],
                    state: ProcessSignalState::Current,
                },
            },
        ];
        let process_ir = ProcessIr {
            storages: vec![ProcessStorage {
                id: ProcessStorageId(0),
                owner: InstanceId(0),
                name: "persistent".into(),
                source: None,
                span,
                ty: None,
                layout: Some(byte()),
                initializer: Some(ProcessValueId(0)),
                bindings: vec![],
            }],
            values,
            processes: vec![ProcessCfg {
                id: ProcessId(0),
                root: InstanceId(0),
                owner: InstanceId(0),
                label: Some("per-place".into()),
                span,
                activation: ProcessActivation::TimeZero,
                entry: ProcessBlockId(0),
                locals: vec![ProcessLocal {
                    id: ProcessLocalId(0),
                    name: "temporary".into(),
                    source: None,
                    span,
                    ty: None,
                    layout: Some(byte()),
                }],
                blocks: vec![ProcessBlock {
                    id: ProcessBlockId(0),
                    instructions: vec![
                        ProcessInstruction::Declare {
                            local: ProcessLocalId(0),
                            initializer: Some(ProcessValueId(0)),
                            span,
                        },
                        ProcessInstruction::Assign {
                            semantics: ProcessAssignment::PerPlace,
                            driver_context: Some(0),
                            target: ProcessValueId(4),
                            value: ProcessValueId(5),
                            span,
                        },
                        ProcessInstruction::Assign {
                            semantics: ProcessAssignment::StagedSignal,
                            driver_context: Some(0),
                            target: ProcessValueId(6),
                            value: ProcessValueId(1),
                            span,
                        },
                        ProcessInstruction::Assign {
                            semantics: ProcessAssignment::StagedSignal,
                            driver_context: Some(0),
                            target: ProcessValueId(7),
                            value: ProcessValueId(2),
                            span,
                        },
                    ],
                    terminator: ProcessTerminator::Return {
                        value: None,
                        span: Some(span),
                    },
                }],
            }],
            ..ProcessIr::default()
        };
        let design = Design {
            signals: vec![sig("D.staged", 8), sig("D.local", 8), sig("D.storage", 8)],
            process_ir,
            ..Design::default()
        };

        let dir =
            std::env::temp_dir().join(format!("siox_process_per_place_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let object = dir.join("design.o");
        let main_c = dir.join("main.c");
        let binary = dir.join("sim");
        emit_object(&design, &object).unwrap();
        std::fs::write(
            &main_c,
            r#"
extern void sx_reset(void);
extern unsigned long long sx_read(unsigned);
extern unsigned char sx_process_commit(void);
typedef unsigned char (*sx_process_entry)(unsigned resume_block);
extern sx_process_entry const sx_process_entries[];
extern const unsigned sx_process_initial_blocks[];
signed main(void) {
    sx_reset();
    if (sx_process_commit() != 0) return 1;
    if (sx_process_entries[0](sx_process_initial_blocks[0]) != 0) return 2;
    if (sx_read(0) != 0 || sx_read(1) != 0 || sx_read(2) != 0) return 3;
    if (sx_process_commit() != 1) return 4;
    if (sx_read(0) != 0x33 || sx_read(1) != 0x11 || sx_read(2) != 0x22) return 5;
    return 0;
}
"#,
        )
        .unwrap();
        let link = Command::new("clang")
            .args([
                main_c.to_str().unwrap(),
                object.to_str().unwrap(),
                "-o",
                binary.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            link.status.success(),
            "link failed: {}",
            String::from_utf8_lossy(&link.stderr)
        );
        let run = Command::new(&binary).status().unwrap();
        assert!(
            run.success(),
            "native per-place probe returned {:?}",
            run.code()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    /// Process-only diagnostic sites reach the established failure ABI. An
    /// invalid checked access in an untaken select arm must remain inactive,
    /// while an evaluated access records both its index and the ranged write's
    /// pre-truncation value/source site.
    fn process_checked_index_and_range_failures_follow_control_flow() {
        if Command::new("clang").arg("--version").output().is_err() {
            eprintln!(
                "skipping process_checked_index_and_range_failures_follow_control_flow: clang not found"
            );
            return;
        }

        let span = Span::new(FileId(0), 0..0);
        let quiet_write = Span::new(FileId(0), 10..11);
        let checked_site = Span::new(FileId(0), 20..21);
        let failing_write = Span::new(FileId(0), 30..31);
        let values = vec![
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(64),
                kind: ProcessValueKind::Signal {
                    signals: vec![SignalId(0)],
                    state: ProcessSignalState::Current,
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(64),
                kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![7])),
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(1),
                kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![0])),
            },
            ProcessValue {
                span: checked_site,
                ty: None,
                bit_width: Some(64),
                kind: ProcessValueKind::CheckedIndex {
                    index: ProcessValueId(1),
                    valid: ProcessValueId(2),
                    left: 0,
                    right: 3,
                    span: checked_site,
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(64),
                kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![1])),
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(64),
                kind: ProcessValueKind::Select {
                    condition: ProcessValueId(2),
                    then_value: ProcessValueId(3),
                    else_value: ProcessValueId(4),
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(1),
                kind: ProcessValueKind::Binary {
                    operation: ProcessBinaryOp::Eq,
                    left: ProcessValueId(3),
                    right: ProcessValueId(1),
                },
            },
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(1),
                kind: ProcessValueKind::Binary {
                    operation: ProcessBinaryOp::And,
                    left: ProcessValueId(2),
                    right: ProcessValueId(6),
                },
            },
        ];
        let assignment = |value, span| ProcessInstruction::Assign {
            semantics: ProcessAssignment::StagedSignal,
            driver_context: None,
            target: ProcessValueId(0),
            value: ProcessValueId(value),
            span,
        };
        let process = |id, value, write_span| ProcessCfg {
            id: ProcessId(id),
            root: InstanceId(0),
            owner: InstanceId(0),
            label: Some(format!("failure-{id}")),
            span,
            activation: ProcessActivation::TimeZero,
            entry: ProcessBlockId(0),
            locals: vec![],
            blocks: vec![ProcessBlock {
                id: ProcessBlockId(0),
                instructions: vec![assignment(value, write_span)],
                terminator: ProcessTerminator::Return {
                    value: None,
                    span: Some(span),
                },
            }],
        };
        let guarded = ProcessCfg {
            id: ProcessId(2),
            root: InstanceId(0),
            owner: InstanceId(0),
            label: Some("guarded-failure".into()),
            span,
            activation: ProcessActivation::TimeZero,
            entry: ProcessBlockId(0),
            locals: vec![],
            blocks: vec![
                ProcessBlock {
                    id: ProcessBlockId(0),
                    instructions: vec![],
                    terminator: ProcessTerminator::Branch {
                        condition: ProcessValueId(7),
                        then_block: ProcessBlockId(1),
                        else_block: ProcessBlockId(2),
                    },
                },
                ProcessBlock {
                    id: ProcessBlockId(1),
                    instructions: vec![],
                    terminator: ProcessTerminator::Return {
                        value: None,
                        span: Some(span),
                    },
                },
                ProcessBlock {
                    id: ProcessBlockId(2),
                    instructions: vec![],
                    terminator: ProcessTerminator::Return {
                        value: None,
                        span: Some(span),
                    },
                },
            ],
        };
        let process_ir = ProcessIr {
            processes: vec![
                process(0, 5, quiet_write),
                process(1, 3, failing_write),
                guarded,
            ],
            values,
            ..ProcessIr::default()
        };
        let mut ranged = sig("D.ranged", 64);
        ranged.integer = true;
        ranged.range = Some((-2, 2));
        let design = Design {
            signals: vec![ranged],
            process_ir,
            ..Design::default()
        };
        assert_eq!(design.index_sites().len(), 1);
        assert_eq!(design.range_sites(), vec![quiet_write, failing_write]);

        let dir = std::env::temp_dir().join(format!("siox_process_fail_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let object = dir.join("design.o");
        let main_c = dir.join("main.c");
        let binary = dir.join("sim");
        emit_object(&design, &object).unwrap();
        std::fs::write(
            &main_c,
            r#"
extern void sx_reset(void);
extern unsigned long long sx_read(unsigned);
extern unsigned sx_index_error(void);
extern long long sx_index_value(void);
extern unsigned sx_range_error(void);
extern long long sx_range_value(void);
extern unsigned sx_range_site(void);
extern unsigned char sx_process_commit(void);
typedef unsigned char (*sx_process_entry)(unsigned resume_block);
extern sx_process_entry const sx_process_entries[];
extern const unsigned sx_process_initial_blocks[];
signed main(void) {
    sx_reset();
    if (sx_process_entries[0](sx_process_initial_blocks[0]) != 0) return 1;
    if (sx_index_error() != 0 || sx_range_error() != 0) return 2;
    if (sx_process_commit() != 1 || sx_read(0) != 1) return 3;

    sx_reset();
    if (sx_process_entries[2](sx_process_initial_blocks[2]) != 0) return 4;
    if (sx_index_error() != 0 || sx_range_error() != 0) return 5;

    sx_reset();
    if (sx_process_entries[1](sx_process_initial_blocks[1]) != 0) return 6;
    if (sx_index_error() != 1 || sx_index_value() != 7) return 7;
    if (sx_range_error() != 1 || sx_range_value() != 7 || sx_range_site() != 2) return 8;
    if (sx_process_commit() != 1 || sx_read(0) != 7) return 9;
    return 0;
}
"#,
        )
        .unwrap();
        let link = Command::new("clang")
            .args([
                main_c.to_str().unwrap(),
                object.to_str().unwrap(),
                "-o",
                binary.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            link.status.success(),
            "link failed: {}",
            String::from_utf8_lossy(&link.stderr)
        );
        let run = Command::new(&binary).status().unwrap();
        assert!(
            run.success(),
            "native process failure probe returned {:?}",
            run.code()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(not(feature = "bitpack"))]
    /// An object whose signals span eight ABI words must still link and carry
    /// between words correctly. Skipped when `clang` is unavailable.
    fn eight_word_object_links_and_carries() {
        if Command::new("clang").arg("--version").output().is_err() {
            eprintln!("skipping eight_word_object_links_and_carries: clang not found");
            return;
        }

        let design = Design {
            signals: vec![sig("E.a", 512), sig("E.b", 512), sig("E.y", 512)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(2),
                cond: None,
                expr: Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::Current(SignalId(0))),
                    rhs: Box::new(Expr::Current(SignalId(1))),
                },
                meta: None,
            }],
            event_blocks: vec![],
            process_ir: Default::default(),
            process_labels: Default::default(),
            resolved_process_labels: Default::default(),
            enum_syms: Default::default(),
            enum_bases: Default::default(),
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

        let dir = std::env::temp_dir().join(format!("siox_aot_wide_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let obj = dir.join("design.o");
        let main_c = dir.join("main.c");
        let bin = dir.join("sim");
        emit_object(&design, &obj).unwrap();
        std::fs::write(
            &main_c,
            r#"
extern void sx_reset(void);
extern void sx_set_word(unsigned, unsigned, unsigned long long);
extern unsigned long long sx_read_word(unsigned, unsigned);
extern void sx_settle(void);
signed main(void) {
    sx_reset();
    sx_set_word(0, 6, ~0ULL);
    sx_set_word(0, 7, 0x0123456789abcdefULL);
    sx_set_word(1, 6, 1);
    sx_settle();
    if (sx_read_word(2, 6) != 0) return 1;
    if (sx_read_word(2, 7) != 0x0123456789abcdf0ULL) return 2;
    return 0;
}
"#,
        )
        .unwrap();
        let link = Command::new("clang")
            .args([
                main_c.to_str().unwrap(),
                obj.to_str().unwrap(),
                "-o",
                bin.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            link.status.success(),
            "link failed: {}",
            String::from_utf8_lossy(&link.stderr)
        );
        let run = Command::new(&bin).status().unwrap();
        assert!(run.success(), "native wide sim returned {:?}", run.code());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    /// The reusable runtime, rather than generated per-design C, owns delta
    /// scheduling. A reactive process sees the time-zero writer only after
    /// commit, is requeued from its sensitivity table, and publishes its own
    /// staged result at the following commit.
    fn fixed_process_runtime_schedules_reactive_delta() {
        if Command::new("clang").arg("--version").output().is_err() {
            eprintln!("skipping fixed_process_runtime_schedules_reactive_delta: clang not found");
            return;
        }

        let span = Span::new(FileId(0), 0..0);
        let assignment = |target, value| ProcessInstruction::Assign {
            semantics: ProcessAssignment::StagedSignal,
            driver_context: None,
            target: ProcessValueId(target),
            value: ProcessValueId(value),
            span,
        };
        let process = |id, activation, instruction| ProcessCfg {
            id: ProcessId(id),
            root: InstanceId(0),
            owner: InstanceId(0),
            label: Some(format!("delta-{id}")),
            span,
            activation,
            entry: ProcessBlockId(0),
            locals: vec![],
            blocks: vec![ProcessBlock {
                id: ProcessBlockId(0),
                instructions: vec![instruction],
                terminator: ProcessTerminator::Return {
                    value: None,
                    span: Some(span),
                },
            }],
        };
        let mut ranged = sig("T.ranged", 64);
        ranged.integer = true;
        ranged.range = Some((-2, 2));
        let mut range_failure = process(2, ProcessActivation::TimeZero, assignment(3, 4));
        range_failure.root = InstanceId(1);
        range_failure.owner = InstanceId(1);
        let design = Design {
            signals: vec![sig("T.trigger", 1), sig("T.observed", 1), ranged],
            process_ir: ProcessIr {
                processes: vec![
                    process(0, ProcessActivation::TimeZero, assignment(0, 1)),
                    process(
                        1,
                        ProcessActivation::Reactive {
                            sensitivity: vec![ProcessSensitivity::Signal(SignalId(0))],
                        },
                        assignment(2, 0),
                    ),
                    range_failure,
                ],
                tests: vec![
                    ProcessTest {
                        entity: DefId(0),
                        root: InstanceId(0),
                        qualified_name: "runtime::delta".into(),
                        span,
                        processes: vec![ProcessId(0), ProcessId(1)],
                    },
                    ProcessTest {
                        entity: DefId(1),
                        root: InstanceId(1),
                        qualified_name: "runtime::range_failure".into(),
                        span,
                        processes: vec![ProcessId(2)],
                    },
                ],
                values: vec![
                    ProcessValue {
                        span,
                        ty: None,
                        bit_width: Some(1),
                        kind: ProcessValueKind::Signal {
                            signals: vec![SignalId(0)],
                            state: ProcessSignalState::Current,
                        },
                    },
                    ProcessValue {
                        span,
                        ty: None,
                        bit_width: Some(1),
                        kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![1])),
                    },
                    ProcessValue {
                        span,
                        ty: None,
                        bit_width: Some(1),
                        kind: ProcessValueKind::Signal {
                            signals: vec![SignalId(1)],
                            state: ProcessSignalState::Current,
                        },
                    },
                    ProcessValue {
                        span,
                        ty: None,
                        bit_width: Some(64),
                        kind: ProcessValueKind::Signal {
                            signals: vec![SignalId(2)],
                            state: ProcessSignalState::Current,
                        },
                    },
                    ProcessValue {
                        span,
                        ty: None,
                        bit_width: Some(64),
                        kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![7])),
                    },
                ],
                ..ProcessIr::default()
            },
            ..Design::default()
        };
        let issues = design.validate();
        assert!(issues.is_empty(), "invalid runtime fixture: {issues:?}");

        let dir = std::env::temp_dir().join(format!("siox_fixed_runtime_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let object = dir.join("design.o");
        let probe = dir.join("probe.c");
        let binary = dir.join("sim");
        emit_object(&design, &object).unwrap();
        std::fs::write(
            &probe,
            r#"
#include "process.h"
#include <string.h>
extern unsigned long long sx_read(unsigned);
signed main(void) {
    if (sx_runtime_run_test(0)) return 1;
    if (sx_runtime_error()) return 2;
    if (sx_read(1) != 1) return 3;
    if (!sx_runtime_run_test(1)) return 4;
    if (!sx_runtime_error() || !strstr(sx_runtime_error(), "range failure")) return 5;
    return 0;
}
"#,
        )
        .unwrap();
        let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime");
        let link = Command::new("clang")
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
            .arg(&probe)
            .arg(runtime.join("process.c"))
            .arg("-I")
            .arg(&runtime)
            .arg(&object)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            link.status.success(),
            "link failed: {}",
            String::from_utf8_lossy(&link.stderr)
        );
        let run = Command::new(&binary).status().unwrap();
        assert!(run.success(), "fixed runtime returned {:?}", run.code());
        let _ = std::fs::remove_dir_all(dir);
    }
}
