//! Transitional Siox-AST lowering into the canonical process IR.
//!
//! Process/CFG types, validation, test descriptors, and ownership live in
//! [`crate::ir::Design`]. This module remains only while native test statements
//! are translated separately from hardware behavior. The generated-C backend
//! consumes `Design::process_ir` metadata and never owns another program.

use crate::elab::Hierarchy;
use crate::ir::{
    Design, ProcessActivation, ProcessAssignment, ProcessBlock, ProcessBlockId, ProcessCfg,
    ProcessId, ProcessInstruction, ProcessIr, ProcessLocal, ProcessLocalId, ProcessMatchArm,
    ProcessPattern, ProcessRuntimeOp, ProcessSuspendOp, ProcessTerminator, ProcessTest,
    ProcessValue, SourceLayout,
};
use crate::resolve::Resolved;
use crate::syntax::ast::{self, ElseBranch, ImplItem, Stmt};
use crate::syntax::Module;
use crate::testbench::TestPlan;
use crate::types::Typed;

struct LoweringContext<'a> {
    resolved: &'a Resolved,
    typed: &'a Typed,
    process_ir: &'a mut ProcessIr,
}

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
                    let lowered = {
                        let mut context = LoweringContext {
                            resolved,
                            typed,
                            process_ir: &mut process_ir,
                        };
                        lower_process(
                            id,
                            test.root,
                            label,
                            process.span,
                            activation,
                            &process.body.stmts,
                            &mut context,
                        )
                    };
                    process_ir.processes.push(lowered);
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
            let lowered = {
                let mut context = LoweringContext {
                    resolved,
                    typed,
                    process_ir: &mut process_ir,
                };
                lower_process(
                    id,
                    test.root,
                    Some(format!("{root_path}::<legacy>")),
                    span,
                    ProcessActivation::TimeZero,
                    &legacy_statements,
                    &mut context,
                )
            };
            process_ir.processes.push(lowered);
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
    context: &mut LoweringContext<'_>,
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
    lower_statements(
        statements,
        context.resolved,
        context.typed,
        context.process_ir,
        &mut process,
        ProcessBlockId(0),
    );
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
    resolved: &Resolved,
    typed: &Typed,
    process_ir: &mut ProcessIr,
    process: &mut ProcessCfg,
    entry: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let mut current = Some(entry);
    for statement in statements {
        let Some(block) = current else { break };
        current = lower_statement(statement, resolved, typed, process_ir, process, block);
    }
    current
}

fn lower_statement(
    statement: &Stmt,
    resolved: &Resolved,
    typed: &Typed,
    process_ir: &mut ProcessIr,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    match statement {
        Stmt::Let(declaration) => {
            let local = push_local(process, declaration, None, resolved, typed);
            process.blocks[block.0 as usize]
                .instructions
                .push(ProcessInstruction::Declare {
                    local,
                    initializer: declaration
                        .value
                        .as_ref()
                        .map(|value| value_ref(value, typed, process_ir)),
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
            let semantics = assignment_semantics(target, process, resolved);
            process.blocks[block.0 as usize]
                .instructions
                .push(ProcessInstruction::Assign {
                    semantics,
                    target: value_ref(target, typed, process_ir),
                    value: value_ref(value, typed, process_ir),
                    delay: after
                        .as_ref()
                        .map(|delay| value_ref(delay, typed, process_ir)),
                    span: *span,
                });
            Some(block)
        }
        Stmt::Expr(ast::Expr::Call {
            callee, args, span, ..
        }) => lower_call(callee, args, *span, typed, process_ir, process, block),
        Stmt::Expr(expression) => {
            process.blocks[block.0 as usize]
                .instructions
                .push(ProcessInstruction::Runtime {
                    operation: ProcessRuntimeOp::Call("<expression>".to_string()),
                    arguments: vec![value_ref(expression, typed, process_ir)],
                    span: ast::expr_span(expression),
                });
            Some(block)
        }
        Stmt::If(statement) => lower_if(statement, resolved, typed, process_ir, process, block),
        Stmt::Match(statement) => {
            lower_match(statement, resolved, typed, process_ir, process, block)
        }
        Stmt::For {
            var,
            range,
            body,
            span,
        } => lower_for(
            var, range, body, *span, resolved, typed, process_ir, process, block,
        ),
        Stmt::Return { value, span } => {
            process.blocks[block.0 as usize].terminator = ProcessTerminator::Return {
                value: value
                    .as_ref()
                    .map(|value| value_ref(value, typed, process_ir)),
                span: Some(*span),
            };
            None
        }
    }
}

fn assignment_semantics(
    target: &ast::Expr,
    process: &ProcessCfg,
    resolved: &Resolved,
) -> ProcessAssignment {
    let target = assignment_base(target).and_then(|path| resolved.resolved(path.span));
    let is_local = target.is_some_and(|target| {
        process
            .locals
            .iter()
            .any(|local| local.source == Some(target))
    });
    if is_local {
        ProcessAssignment::ImmediateLocal
    } else {
        ProcessAssignment::StagedSignal
    }
}

fn assignment_base(target: &ast::Expr) -> Option<&ast::Path> {
    match target {
        ast::Expr::Path(path) => Some(path),
        ast::Expr::Field { base, .. } | ast::Expr::Index { base, .. } => assignment_base(base),
        _ => None,
    }
}

fn lower_call(
    callee: &ast::Expr,
    arguments: &[ast::Expr],
    span: crate::diag::Span,
    typed: &Typed,
    process_ir: &mut ProcessIr,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let name = callee_name(callee);
    let arguments = arguments
        .iter()
        .map(|argument| value_ref(argument, typed, process_ir))
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
    resolved: &Resolved,
    typed: &Typed,
    process_ir: &mut ProcessIr,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let then_block = push_block(process);
    let else_block = push_block(process);
    process.blocks[block.0 as usize].terminator = ProcessTerminator::Branch {
        condition: value_ref(&statement.cond, typed, process_ir),
        then_block,
        else_block,
    };

    let then_tail = lower_statements(
        &statement.then.stmts,
        resolved,
        typed,
        process_ir,
        process,
        then_block,
    );
    let else_tail = match statement.else_.as_deref() {
        Some(ElseBranch::Block(block)) => lower_statements(
            &block.stmts,
            resolved,
            typed,
            process_ir,
            process,
            else_block,
        ),
        Some(ElseBranch::If(statement)) => {
            lower_if(statement, resolved, typed, process_ir, process, else_block)
        }
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

fn lower_match(
    statement: &ast::MatchStmt,
    resolved: &Resolved,
    typed: &Typed,
    process_ir: &mut ProcessIr,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let mut arms = Vec::with_capacity(statement.arms.len());
    for arm in &statement.arms {
        arms.push(ProcessMatchArm {
            pattern: lower_pattern(&arm.pattern),
            block: push_block(process),
            span: arm.span,
        });
    }
    let exhaustive = statement
        .arms
        .iter()
        .any(|arm| pattern_has_wildcard(&arm.pattern));
    let fallback = (!exhaustive).then(|| push_block(process));
    let scrutinee = value_ref(&statement.scrutinee, typed, process_ir);
    process.blocks[block.0 as usize].terminator = ProcessTerminator::Match {
        scrutinee,
        arms: arms.clone(),
        fallback,
    };

    let mut tails = Vec::new();
    for (source, lowered) in statement.arms.iter().zip(&arms) {
        if let Some(tail) = lower_statements(
            &source.body.stmts,
            resolved,
            typed,
            process_ir,
            process,
            lowered.block,
        ) {
            tails.push(tail);
        }
    }
    if let Some(fallback) = fallback {
        tails.push(fallback);
    }
    if tails.is_empty() {
        return None;
    }

    let join = push_block(process);
    for tail in tails {
        process.blocks[tail.0 as usize].terminator = ProcessTerminator::Goto(join);
    }
    Some(join)
}

fn lower_pattern(pattern: &ast::Pattern) -> ProcessPattern {
    match pattern {
        ast::Pattern::Wildcard => ProcessPattern::Wildcard,
        ast::Pattern::Path(path) => ProcessPattern::Path(
            path.segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join("::"),
        ),
        ast::Pattern::BitPattern { text, .. } => ProcessPattern::BitPattern(text.clone()),
        ast::Pattern::Or { alts, .. } => {
            ProcessPattern::Or(alts.iter().map(lower_pattern).collect())
        }
        ast::Pattern::Range { lo, hi, .. } => ProcessPattern::Range {
            left: *lo,
            right: *hi,
        },
        ast::Pattern::CharLit { ch, .. } => ProcessPattern::Char(*ch),
    }
}

fn pattern_has_wildcard(pattern: &ast::Pattern) -> bool {
    match pattern {
        ast::Pattern::Wildcard => true,
        ast::Pattern::Or { alts, .. } => alts.iter().any(pattern_has_wildcard),
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_for(
    variable: &ast::Ident,
    iterable: &ast::Expr,
    body: &ast::Block,
    span: crate::diag::Span,
    resolved: &Resolved,
    typed: &Typed,
    process_ir: &mut ProcessIr,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let local = ProcessLocalId(process.locals.len() as u32);
    let ty = if matches!(iterable, ast::Expr::Range { .. }) {
        Some(crate::types::Ty::Integer)
    } else {
        match typed.expr_type(ast::expr_span(iterable)) {
            Some(crate::types::Ty::Array { elem, .. }) => Some((**elem).clone()),
            _ => None,
        }
    };
    process.locals.push(ProcessLocal {
        id: local,
        name: variable.text.clone(),
        source: resolved.declared(variable.span),
        span: variable.span,
        ty,
        layout: None,
    });

    let iterable = value_ref(iterable, typed, process_ir);
    // Keep the loop control on a dedicated header. Reusing `block` here makes
    // the body back-edge replay every instruction that appeared before the
    // loop in that source block.
    let header = push_block(process);
    let body_block = push_block(process);
    let exit = push_block(process);
    process.blocks[block.0 as usize].terminator = ProcessTerminator::Goto(header);
    process.blocks[header.0 as usize].terminator = ProcessTerminator::For {
        local,
        iterable,
        body: body_block,
        exit,
        span,
    };
    if let Some(tail) = lower_statements(
        &body.stmts,
        resolved,
        typed,
        process_ir,
        process,
        body_block,
    ) {
        process.blocks[tail.0 as usize].terminator = ProcessTerminator::Goto(header);
    }
    Some(exit)
}

fn push_local(
    process: &mut ProcessCfg,
    declaration: &ast::LetDecl,
    layout: Option<SourceLayout>,
    resolved: &Resolved,
    typed: &Typed,
) -> ProcessLocalId {
    let id = ProcessLocalId(process.locals.len() as u32);
    process.locals.push(ProcessLocal {
        id,
        name: declaration.name.text.clone(),
        source: resolved.declared(declaration.name.span),
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

fn value_ref(
    expression: &ast::Expr,
    typed: &Typed,
    process_ir: &mut ProcessIr,
) -> crate::ir::ProcessValueId {
    let span = ast::expr_span(expression);
    let id = crate::ir::ProcessValueId(process_ir.values.len() as u32);
    process_ir.values.push(ProcessValue {
        span,
        ty: typed.expr_type(span).cloned(),
        text: crate::syntax::pretty::expr_string(expression),
    });
    id
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
             enum Mode { Off, On }\n\
             #[std::attrs::test] entity Smoke {}\n\
             impl Smoke {\n\
               let flag: Bool = true;\n\
               let i: integer = 9;\n\
               process stimulus {\n\
                 let seen: Bool = flag;\n\
                 let mode: Mode = Mode::On;\n\
                 if seen { print!(\"set\"); } else { warn!(true, \"clear\"); }\n\
                 match mode {\n\
                   Mode::Off => { warn!(true, \"off\"); }\n\
                   Mode::On => { print!(\"on\"); }\n\
                 }\n\
                 print!(\"before loop\");\n\
                 for i in 0..2 { print!(\"loop {}\", i); }\n\
                 i = 7;\n\
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
        assert!(!design.process_ir.values.is_empty());
        let descriptor = &design.process_ir.tests[0];
        assert_eq!(descriptor.qualified_name, "tests::Smoke");
        assert_eq!(descriptor.processes, [ProcessId(0)]);
        let process = &design.process_ir.processes[0];
        assert_eq!(process.label.as_deref(), Some("Smoke::stimulus"));
        assert_eq!(process.locals.len(), 3);
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
            .any(|block| matches!(block.terminator, ProcessTerminator::Match { .. })));
        assert!(process
            .blocks
            .iter()
            .any(|block| matches!(block.terminator, ProcessTerminator::For { .. })));
        let loop_header = process
            .blocks
            .iter()
            .find(|block| matches!(block.terminator, ProcessTerminator::For { .. }))
            .expect("missing loop header");
        assert!(
            loop_header.instructions.is_empty(),
            "loop back-edge would replay pre-loop instructions: {loop_header:?}"
        );
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
        let post_loop_i = process
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match instruction {
                ProcessInstruction::Assign {
                    semantics, target, ..
                } if design.process_ir.values[target.0 as usize].text == "i" => Some(*semantics),
                _ => None,
            })
            .expect("missing post-loop write to shadowed entity signal");
        assert_eq!(post_loop_i, ProcessAssignment::StagedSignal);
        let dump = design.process_ir.to_ir_string();
        assert!(dump.contains("value %v0"));
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
                    blocks: vec![ProcessBlock {
                        id: ProcessBlockId(0),
                        instructions: vec![ProcessInstruction::Runtime {
                            operation: ProcessRuntimeOp::Print,
                            arguments: vec![crate::ir::ProcessValueId(9)],
                            span,
                        }],
                        terminator: ProcessTerminator::Return {
                            value: None,
                            span: None,
                        },
                    }],
                }],
                tests: vec![ProcessTest {
                    entity: crate::resolve::DefId(0),
                    root: crate::elab::InstanceId(0),
                    qualified_name: "tests::Broken".to_string(),
                    span,
                    processes: vec![ProcessId(0)],
                }],
                values: Vec::new(),
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
        assert!(
            issues.iter().any(|issue| issue.contains("invalid value")),
            "{issues:?}"
        );
    }
}
