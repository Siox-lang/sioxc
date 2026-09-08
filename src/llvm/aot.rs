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
        BinOp, Driver, Expr, LookupTable, LookupTableId, ProcessActivation, ProcessAssignment,
        ProcessBinaryOp, ProcessBlock, ProcessBlockId, ProcessCfg, ProcessId, ProcessInstruction,
        ProcessIr, ProcessNumber, ProcessSignalState, ProcessSuspendOp, ProcessTerminator,
        ProcessValue, ProcessValueId, ProcessValueKind, Signal, SignalId,
    };
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
    if (sx_process_commit() != 1 || sx_process_changed(0) != 1) return 3;
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
}
