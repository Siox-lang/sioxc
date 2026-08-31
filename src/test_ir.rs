//! Transitional Siox-AST lowering into the canonical process IR.
//!
//! Process/CFG types, validation, test descriptors, and ownership live in
//! [`crate::ir::Design`]. This module remains only while native test statements
//! are translated separately from hardware behavior. The generated-C backend
//! consumes `Design::process_ir` metadata and never owns another program.

use crate::elab::Hierarchy;
use crate::ir::{
    DeferredProcessControl, Design, ProcessActivation, ProcessAssignment, ProcessBlock,
    ProcessBlockId, ProcessCfg, ProcessId, ProcessInstruction, ProcessIr, ProcessLocal,
    ProcessLocalId, ProcessRuntimeOp, ProcessSuspendOp, ProcessTerminator, ProcessTest,
    ProcessValue, SourceLayout,
};
use crate::resolve::Resolved;
use crate::syntax::ast::{self, ElseBranch, ImplItem, Stmt};
use crate::syntax::Module;
use crate::testbench::TestPlan;
use crate::types::Typed;

/// Fill the canonical process product from the same resolved roots and layouts
/// used by the compatibility backend.
///
/// One explicit source process becomes one CFG. Legacy impl-scope test
/// statements remain one implicit foreground process so their existing
/// sequential/`await` behavior is preserved until the syntax is retired.
pub fn lower(
    modules: &[Module],
    resolved: &Resolved,
    typed: &Typed,
    hierarchy: &Hierarchy,
    plan: &TestPlan,
    design: &mut Design,
) {
    let mut process_ir = ProcessIr::default();

    for test in &plan.tests {
        let root_path = hierarchy.root_path(test.root);
        let items = crate::testbench::implementation_items(modules, resolved, test.entity);
        let mut test_processes = Vec::new();
        let mut legacy_statements = Vec::new();

        for item in items {
            match item {
                ImplItem::Process(process) => {
                    let id = ProcessId(process_ir.processes.len() as u32);
                    let label = process
                        .name
                        .as_ref()
                        .map(|name| format!("{root_path}::{}", name.text));
                    let activation = process_activation(&process.body.stmts, &root_path, design);
                    process_ir.processes.push(lower_process(
                        id,
                        test.root,
                        label,
                        process.span,
                        activation,
                        &process.body.stmts,
                        typed,
                    ));
                    test_processes.push(id);
                }
                ImplItem::Stmt(statement) => legacy_statements.push(statement.clone()),
                ImplItem::Const(_)
                | ImplItem::Fn(_)
                | ImplItem::ModeField { .. }
                | ImplItem::Let(_) => {}
            }
        }

        if !legacy_statements.is_empty() {
            let id = ProcessId(process_ir.processes.len() as u32);
            let span = legacy_statements
                .first()
                .map(ast::stmt_span)
                .unwrap_or(test.span);
            process_ir.processes.push(lower_process(
                id,
                test.root,
                Some(format!("{root_path}::<legacy>")),
                span,
                ProcessActivation::TimeZero,
                &legacy_statements,
                typed,
            ));
            test_processes.push(id);
        }

        process_ir.tests.push(ProcessTest {
            entity: test.entity,
            root: test.root,
            qualified_name: test.qualified_name.clone(),
            span: test.span,
            processes: test_processes,
        });
    }

    design.process_ir = process_ir;
}

fn process_activation(statements: &[Stmt], root_path: &str, design: &Design) -> ProcessActivation {
    if !crate::testbench::is_clock_process(statements) {
        return ProcessActivation::TimeZero;
    }
    let Stmt::Assign { target, .. } = &statements[0] else {
        unreachable!("is_clock_process accepted a non-assignment")
    };
    let target = crate::syntax::pretty::expr_string(target);
    let qualified = format!("{root_path}.{target}");
    let sensitivity = design
        .signals
        .iter()
        .position(|signal| signal.path == qualified || signal.path == target)
        .and_then(|index| u32::try_from(index).ok())
        .map(crate::ir::SignalId)
        .into_iter()
        .collect();
    ProcessActivation::Reactive { sensitivity }
}

fn lower_process(
    id: ProcessId,
    owner: crate::elab::InstanceId,
    label: Option<String>,
    span: crate::diag::Span,
    activation: ProcessActivation,
    statements: &[Stmt],
    typed: &Typed,
) -> ProcessCfg {
    let mut process = ProcessCfg {
        id,
        owner,
        label,
        span,
        activation,
        entry: ProcessBlockId(0),
        locals: Vec::new(),
        blocks: vec![empty_block(ProcessBlockId(0))],
    };
    lower_statements(statements, typed, &mut process, ProcessBlockId(0));
    process
}

fn empty_block(id: ProcessBlockId) -> ProcessBlock {
    ProcessBlock {
        id,
        instructions: Vec::new(),
        terminator: ProcessTerminator::Return {
            value: None,
            span: None,
        },
    }
}

fn push_block(process: &mut ProcessCfg) -> ProcessBlockId {
    let id = ProcessBlockId(process.blocks.len() as u32);
    process.blocks.push(empty_block(id));
    id
}

/// Returns the still-open tail block. `None` means control terminated and
/// following statements in the source block are unreachable.
fn lower_statements(
    statements: &[Stmt],
    typed: &Typed,
    process: &mut ProcessCfg,
    entry: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let mut current = Some(entry);
    for statement in statements {
        let Some(block) = current else { break };
        current = lower_statement(statement, typed, process, block);
    }
    current
}

fn lower_statement(
    statement: &Stmt,
    typed: &Typed,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    match statement {
        Stmt::Let(declaration) => {
            let local = push_local(process, declaration, None, typed);
            process.blocks[block.0 as usize]
                .instructions
                .push(ProcessInstruction::Declare {
                    local,
                    initializer: declaration
                        .value
                        .as_ref()
                        .map(|value| value_ref(value, typed)),
                    span: declaration.span,
                });
            Some(block)
        }
        Stmt::Assign {
            target,
            value,
            after,
            span,
        } => {
            let semantics = assignment_semantics(target, process);
            process.blocks[block.0 as usize]
                .instructions
                .push(ProcessInstruction::Assign {
                    semantics,
                    target: value_ref(target, typed),
                    value: value_ref(value, typed),
                    delay: after.as_ref().map(|delay| value_ref(delay, typed)),
                    span: *span,
                });
            Some(block)
        }
        Stmt::Expr(ast::Expr::Call {
            callee, args, span, ..
        }) => lower_call(callee, args, *span, typed, process, block),
        Stmt::Expr(expression) => {
            process.blocks[block.0 as usize]
                .instructions
                .push(ProcessInstruction::Runtime {
                    operation: ProcessRuntimeOp::Call("<expression>".to_string()),
                    arguments: vec![value_ref(expression, typed)],
                    span: ast::expr_span(expression),
                });
            Some(block)
        }
        Stmt::If(statement) => lower_if(statement, typed, process, block),
        Stmt::Match(statement) => {
            process.blocks[block.0 as usize].instructions.push(
                ProcessInstruction::DeferredControl {
                    kind: DeferredProcessControl::Match,
                    span: statement.span,
                },
            );
            Some(block)
        }
        Stmt::For { span, .. } => {
            process.blocks[block.0 as usize].instructions.push(
                ProcessInstruction::DeferredControl {
                    kind: DeferredProcessControl::For,
                    span: *span,
                },
            );
            Some(block)
        }
        Stmt::Return { value, span } => {
            process.blocks[block.0 as usize].terminator = ProcessTerminator::Return {
                value: value.as_ref().map(|value| value_ref(value, typed)),
                span: Some(*span),
            };
            None
        }
    }
}

fn assignment_semantics(target: &ast::Expr, process: &ProcessCfg) -> ProcessAssignment {
    let target = crate::syntax::pretty::expr_string(target);
    let is_local = process.locals.iter().any(|local| {
        target == local.name
            || target
                .strip_prefix(&local.name)
                .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
    });
    if is_local {
        ProcessAssignment::ImmediateLocal
    } else {
        ProcessAssignment::StagedSignal
    }
}

fn lower_call(
    callee: &ast::Expr,
    arguments: &[ast::Expr],
    span: crate::diag::Span,
    typed: &Typed,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let name = callee_name(callee);
    let arguments = arguments
        .iter()
        .map(|argument| value_ref(argument, typed))
        .collect::<Vec<_>>();
    match name.as_str() {
        "await" => {
            let resume = push_block(process);
            process.blocks[block.0 as usize].terminator = ProcessTerminator::Suspend {
                operation: ProcessSuspendOp::Await,
                arguments,
                resume,
                span,
            };
            Some(resume)
        }
        "stop" => {
            process.blocks[block.0 as usize].terminator = ProcessTerminator::Stop { span };
            None
        }
        "finish" => {
            process.blocks[block.0 as usize].terminator = ProcessTerminator::Finish { span };
            None
        }
        _ => {
            let operation = match name.as_str() {
                "assert" => ProcessRuntimeOp::Assert,
                "warn" => ProcessRuntimeOp::Warn,
                "print" => ProcessRuntimeOp::Print,
                _ => ProcessRuntimeOp::Call(name),
            };
            process.blocks[block.0 as usize]
                .instructions
                .push(ProcessInstruction::Runtime {
                    operation,
                    arguments,
                    span,
                });
            Some(block)
        }
    }
}

fn lower_if(
    statement: &ast::IfStmt,
    typed: &Typed,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let then_block = push_block(process);
    let else_block = push_block(process);
    process.blocks[block.0 as usize].terminator = ProcessTerminator::Branch {
        condition: value_ref(&statement.cond, typed),
        then_block,
        else_block,
    };

    let then_tail = lower_statements(&statement.then.stmts, typed, process, then_block);
    let else_tail = match statement.else_.as_deref() {
        Some(ElseBranch::Block(block)) => {
            lower_statements(&block.stmts, typed, process, else_block)
        }
        Some(ElseBranch::If(statement)) => lower_if(statement, typed, process, else_block),
        None => Some(else_block),
    };

    if then_tail.is_none() && else_tail.is_none() {
        return None;
    }
    let join = push_block(process);
    if let Some(tail) = then_tail {
        process.blocks[tail.0 as usize].terminator = ProcessTerminator::Goto(join);
    }
    if let Some(tail) = else_tail {
        process.blocks[tail.0 as usize].terminator = ProcessTerminator::Goto(join);
    }
    Some(join)
}

fn push_local(
    process: &mut ProcessCfg,
    declaration: &ast::LetDecl,
    layout: Option<SourceLayout>,
    typed: &Typed,
) -> ProcessLocalId {
    let id = ProcessLocalId(process.locals.len() as u32);
    process.locals.push(ProcessLocal {
        id,
        name: declaration.name.text.clone(),
        span: declaration.span,
        ty: declaration
            .value
            .as_ref()
            .and_then(|value| typed.expr_type(ast::expr_span(value)))
            .cloned(),
        layout,
    });
    id
}

fn value_ref(expression: &ast::Expr, typed: &Typed) -> ProcessValue {
    let span = ast::expr_span(expression);
    ProcessValue {
        span,
        ty: typed.expr_type(span).cloned(),
        text: crate::syntax::pretty::expr_string(expression),
    }
}

fn callee_name(callee: &ast::Expr) -> String {
    match callee {
        ast::Expr::Path(path) => path
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join("::"),
        _ => crate::syntax::pretty::expr_string(callee),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{DiagnosticSink, FileId};

    #[test]
    fn fills_design_process_cfg_with_branches_and_suspension() {
        let sources = [
            "module tests;\n\
             #[std::attrs::test] entity Smoke {}\n\
             impl Smoke {\n\
               let flag: Bool = true;\n\
               process stimulus {\n\
                 let seen: Bool = flag;\n\
                 if seen { print!(\"set\"); } else { warn!(true, \"clear\"); }\n\
                 seen = false;\n\
                 flag = false;\n\
                 await true;\n\
                 assert!(flag == false, \"done\");\n\
                 finish();\n\
               }\n\
             }",
            "module std::logic; pub enum Bool { false, true }",
            "module std::attrs; using std::logic::{Bool}; pub attr test: Bool for entity;",
            "module std::ops; using std::logic::{Bool}; pub trait Boolean { fn as_bool(self) -> Bool; } \
             impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } }",
            "module std::prelude; pub using std::logic::{Bool}; pub using std::attrs::{test}; \
             pub using std::ops::{Boolean};",
        ];
        let mut sink = DiagnosticSink::new();
        let modules: Vec<Module> = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                crate::syntax::parse_module(FileId(index as u32), source, &mut sink)
            })
            .collect();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        let (hierarchy, plan) = crate::testbench::elaborate(&modules, &resolved, &typed, &mut sink);
        let mut design = crate::ir::lower(&modules, &resolved, &hierarchy, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());

        lower(&modules, &resolved, &typed, &hierarchy, &plan, &mut design);
        assert!(design
            .process_ir
            .validate(design.signals.len() as u32)
            .is_empty());
        assert_eq!(design.process_ir.tests.len(), 1);
        assert_eq!(design.process_ir.processes.len(), 1);
        let descriptor = &design.process_ir.tests[0];
        assert_eq!(descriptor.qualified_name, "tests::Smoke");
        assert_eq!(descriptor.processes, [ProcessId(0)]);
        let process = &design.process_ir.processes[0];
        assert_eq!(process.label.as_deref(), Some("Smoke::stimulus"));
        assert_eq!(process.locals.len(), 1);
        assert!(process
            .blocks
            .iter()
            .any(|block| matches!(block.terminator, ProcessTerminator::Branch { .. })));
        assert!(process
            .blocks
            .iter()
            .any(|block| matches!(block.terminator, ProcessTerminator::Suspend { .. })));
        assert!(process
            .blocks
            .iter()
            .any(|block| matches!(block.terminator, ProcessTerminator::Finish { .. })));
        let assignments = process
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction {
                ProcessInstruction::Assign { semantics, .. } => Some(*semantics),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(assignments.contains(&ProcessAssignment::ImmediateLocal));
        assert!(assignments.contains(&ProcessAssignment::StagedSignal));
        let dump = design.process_ir.to_ir_string();
        assert!(dump.contains("test @tests::Smoke root 0 processes [%p0]"));
        assert!(dump.contains("process %p0 root 0 [Smoke::stimulus]"));
    }

    #[test]
    fn design_validator_rejects_invalid_process_ownership_and_entry() {
        let span = crate::diag::Span::new(FileId(0), 0..1);
        let design = Design {
            process_ir: ProcessIr {
                processes: vec![ProcessCfg {
                    id: ProcessId(0),
                    owner: crate::elab::InstanceId(1),
                    label: None,
                    span,
                    activation: ProcessActivation::TimeZero,
                    entry: ProcessBlockId(1),
                    locals: Vec::new(),
                    blocks: vec![empty_block(ProcessBlockId(0))],
                }],
                tests: vec![ProcessTest {
                    entity: crate::resolve::DefId(0),
                    root: crate::elab::InstanceId(0),
                    qualified_name: "tests::Broken".to_string(),
                    span,
                    processes: vec![ProcessId(0)],
                }],
            },
            ..Design::default()
        };
        let issues = design.validate();
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("invalid entry block")),
            "{issues:?}"
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("owned by another root")),
            "{issues:?}"
        );
    }
}
