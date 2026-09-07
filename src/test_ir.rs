//! Transitional Siox-AST lowering into the canonical process IR.
//!
//! Process/CFG types, validation, test descriptors, and ownership live in
//! [`crate::ir::Design`]. This module remains only while native test statements
//! are translated separately from hardware behavior. The generated-C backend
//! consumes `Design::process_ir` metadata and never owns another program.

use crate::elab::Hierarchy;
use crate::ir::{
    Design, LayoutDirection, LayoutKind, ProcessActivation, ProcessAggregateField,
    ProcessAssignment, ProcessBinaryOp, ProcessBlock, ProcessBlockId, ProcessCfg, ProcessId,
    ProcessInstruction, ProcessIr, ProcessLocal, ProcessLocalId, ProcessMatchArm, ProcessNumber,
    ProcessPattern, ProcessRuntimeOp, ProcessSensitivity, ProcessSignalState, ProcessStorage,
    ProcessStorageBinding, ProcessStorageId, ProcessSuspendOp, ProcessTerminator, ProcessTest,
    ProcessUnaryOp, ProcessValue, ProcessValueKind, ProcessValueMatchArm, SignalId,
};
use crate::resolve::Resolved;
use crate::syntax::ast::{self, ElseBranch, ImplItem, Stmt};
use crate::syntax::Module;
use crate::testbench::TestPlan;
use crate::types::Typed;

struct LoweringContext<'a> {
    resolved: &'a Resolved,
    typed: &'a Typed,
    design: &'a Design,
    root_path: &'a str,
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

        register_test_storages(
            &items,
            modules,
            resolved,
            typed,
            hierarchy,
            test.root,
            &root_path,
            design,
            &mut process_ir,
        );

        // Initializers execute before any process starts. They use the same
        // value lowering with an empty lexical scope: persistent storage and
        // declarations resolve normally, while process locals cannot appear.
        let initializer_process = ProcessCfg {
            id: ProcessId(u32::MAX),
            owner: test.root,
            label: Some(format!("{root_path}::<initializers>")),
            span: test.span,
            activation: ProcessActivation::TimeZero,
            entry: ProcessBlockId(0),
            locals: Vec::new(),
            blocks: Vec::new(),
        };
        let initializers = items
            .iter()
            .filter_map(|item| match item {
                ImplItem::Let(declaration) => Some((
                    resolved.declared(declaration.name.span)?,
                    declaration.value.as_ref()?,
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        {
            let mut context = LoweringContext {
                resolved,
                typed,
                design,
                root_path: &root_path,
                process_ir: &mut process_ir,
            };
            for (definition, initializer) in initializers {
                let Some(storage) = context
                    .process_ir
                    .storages
                    .iter()
                    .find(|storage| {
                        storage.owner == test.root && storage.source == Some(definition)
                    })
                    .map(|storage| storage.id)
                else {
                    continue;
                };
                let value = value_ref(initializer, &initializer_process, &mut context);
                context.process_ir.storages[storage.0 as usize].initializer = Some(value);
            }
        }

        for item in &items {
            match item {
                ImplItem::Process(process) => {
                    let id = ProcessId(process_ir.processes.len() as u32);
                    let label = process
                        .name
                        .as_ref()
                        .map(|name| format!("{root_path}::{}", name.text));
                    let activation = process_activation(
                        &process.body.stmts,
                        &root_path,
                        design,
                        resolved,
                        test.root,
                        &process_ir,
                    );
                    let lowered = {
                        let mut context = LoweringContext {
                            resolved,
                            typed,
                            design,
                            root_path: &root_path,
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
                    design,
                    root_path: &root_path,
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

/// Register persistent state declared by one test root and connect each
/// flattened storage projection to the DUT port leaves elaboration produced.
#[allow(clippy::too_many_arguments)]
fn register_test_storages(
    items: &[&ImplItem],
    modules: &[Module],
    resolved: &Resolved,
    typed: &Typed,
    hierarchy: &Hierarchy,
    root: crate::elab::InstanceId,
    root_path: &str,
    design: &Design,
    process_ir: &mut ProcessIr,
) {
    for item in items {
        let ImplItem::Let(declaration) = item else {
            continue;
        };
        let name = &declaration.name.text;
        let Some(layout) = design.source_layouts.get(&format!("{root_path}.{name}")) else {
            // Entity instance declarations deliberately have no testbench
            // storage layout and are represented by the hierarchy instead.
            continue;
        };
        let id = ProcessStorageId(process_ir.storages.len() as u32);
        process_ir.storages.push(ProcessStorage {
            id,
            owner: root,
            name: name.clone(),
            source: resolved.declared(declaration.name.span),
            span: declaration.span,
            ty: declaration
                .value
                .as_ref()
                .and_then(|value| typed.expr_type(ast::expr_span(value)))
                .cloned(),
            layout: Some(layout.clone()),
            initializer: None,
            bindings: testbench_bindings(
                name, modules, resolved, hierarchy, root, root_path, design,
            ),
        });
    }
}

/// Collect every direct DUT port leaf connected to one testbench storage
/// object. Fan-out deliberately retains several bindings with one projection.
fn testbench_bindings(
    storage: &str,
    modules: &[Module],
    resolved: &Resolved,
    hierarchy: &Hierarchy,
    root: crate::elab::InstanceId,
    root_path: &str,
    design: &Design,
) -> Vec<ProcessStorageBinding> {
    let mut bindings = Vec::new();
    for &child_id in &hierarchy.instance(root).children {
        let child = hierarchy.instance(child_id);
        for connection in &child.connections {
            let Some(storage_prefix) = storage_projection(&connection.signal, storage) else {
                continue;
            };
            let port_path = format!("{root_path}.{}.{}", child.name, connection.port);
            for (index, signal) in design.signals.iter().enumerate() {
                let port_projection = if signal.path == port_path {
                    ""
                } else if let Some(suffix) = signal.path.strip_prefix(&port_path) {
                    if !suffix.starts_with('.') && !suffix.starts_with('[') {
                        continue;
                    }
                    suffix
                } else {
                    continue;
                };
                let Ok(index) = u32::try_from(index) else {
                    continue;
                };
                if is_representation_signal(design, index) {
                    continue;
                }
                let Some(direction) = port_direction(
                    modules,
                    resolved,
                    child.entity_id,
                    &connection.port,
                    port_projection,
                    &port_path,
                    design,
                ) else {
                    continue;
                };
                bindings.push(ProcessStorageBinding {
                    projection: format!("{storage_prefix}{port_projection}"),
                    signal: SignalId(index),
                    direction,
                });
            }
        }
    }
    bindings.sort_by(|left, right| {
        left.projection
            .cmp(&right.projection)
            .then_with(|| left.signal.0.cmp(&right.signal.0))
    });
    bindings
        .dedup_by(|left, right| left.projection == right.projection && left.signal == right.signal);
    bindings
}

/// Suffix of a connected source path relative to a storage root.
fn storage_projection<'a>(connected: &'a str, storage: &str) -> Option<&'a str> {
    if connected == storage {
        return Some("");
    }
    connected
        .strip_prefix(storage)
        .filter(|suffix| suffix.starts_with('.') || suffix.starts_with('['))
}

/// Direction of one flattened child port leaf.
fn port_direction(
    modules: &[Module],
    resolved: &Resolved,
    entity: crate::resolve::DefId,
    port: &str,
    projection: &str,
    port_path: &str,
    design: &Design,
) -> Option<LayoutDirection> {
    let declaration = modules
        .iter()
        .flat_map(|module| &module.items)
        .find_map(|item| {
            let ast::Item::Entity(declaration) = item else {
                return None;
            };
            (resolved.declared(declaration.name.span) == Some(entity)).then_some(declaration)
        })?;
    let port = declaration
        .ports
        .iter()
        .find(|candidate| candidate.name.text == port)?;
    if let Some(direction) = port.dir {
        return Some(lower_direction(direction));
    }
    let layout = design.source_layouts.get(port_path)?;
    layout_direction(layout, projection)
}

/// Follow a flattened field/index suffix through a source layout to the view
/// field that supplies its direction.
fn layout_direction(layout: &crate::ir::SourceLayout, projection: &str) -> Option<LayoutDirection> {
    match &layout.kind {
        LayoutKind::Struct { fields, .. } => {
            let field_path = projection.strip_prefix('.')?;
            let boundary = field_path.find(['.', '[']).unwrap_or(field_path.len());
            let (name, rest) = field_path.split_at(boundary);
            let field = fields.iter().find(|field| field.name == name)?;
            field
                .direction
                .clone()
                .or_else(|| layout_direction(&field.layout, rest))
        }
        LayoutKind::Array { element, .. } => {
            let rest = projection.strip_prefix('[')?.split_once(']')?.1;
            layout_direction(element, rest)
        }
        LayoutKind::Scalar { .. } | LayoutKind::Packed { .. } | LayoutKind::Opaque { .. } => None,
    }
}

/// Convert source port direction into the frontend-independent IR spelling.
fn lower_direction(direction: ast::Direction) -> LayoutDirection {
    match direction {
        ast::Direction::In => LayoutDirection::In,
        ast::Direction::Out => LayoutDirection::Out,
        ast::Direction::Inout => LayoutDirection::InOut,
    }
}

/// Whether a signal is an internal metavalue representation leaf rather than
/// source-visible storage.
fn is_representation_signal(design: &Design, id: u32) -> bool {
    design.meta_of.values().any(|companion| *companion == id)
        || design.metavalue_temps.contains(&id)
}

/// How a test process is activated: a canonical clock process becomes
/// reactive on the signal it toggles, everything else starts at time zero.
fn process_activation(
    statements: &[Stmt],
    root_path: &str,
    design: &Design,
    resolved: &Resolved,
    owner: crate::elab::InstanceId,
    process_ir: &ProcessIr,
) -> ProcessActivation {
    if !crate::testbench::is_clock_process(statements) {
        return ProcessActivation::TimeZero;
    }
    let Stmt::Assign { target, .. } = &statements[0] else {
        unreachable!("is_clock_process accepted a non-assignment")
    };
    if let Some(storage) = assignment_base(target)
        .and_then(|path| resolved.resolved(path.span))
        .and_then(|definition| {
            process_ir
                .storages
                .iter()
                .find(|storage| storage.owner == owner && storage.source == Some(definition))
                .map(|storage| storage.id)
        })
    {
        return ProcessActivation::Reactive {
            sensitivity: vec![ProcessSensitivity::Storage(storage)],
        };
    }
    let target = crate::syntax::pretty::expr_string(target);
    let qualified = format!("{root_path}.{target}");
    let sensitivity = design
        .signals
        .iter()
        .position(|signal| signal.path == qualified || signal.path == target)
        .and_then(|index| u32::try_from(index).ok())
        .map(crate::ir::SignalId)
        .map(ProcessSensitivity::Signal)
        .into_iter()
        .collect();
    ProcessActivation::Reactive { sensitivity }
}

/// Lower one process body into a control-flow graph, returning the finished
/// [`ProcessCfg`].
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
    lower_statements(statements, context, &mut process, ProcessBlockId(0));
    process
}

/// A block with no instructions that simply returns; blocks are created
/// empty and filled in as lowering proceeds.
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

/// Append a fresh empty block and return its id.
fn push_block(process: &mut ProcessCfg) -> ProcessBlockId {
    let id = ProcessBlockId(process.blocks.len() as u32);
    process.blocks.push(empty_block(id));
    id
}

/// Returns the still-open tail block. `None` means control terminated and
/// following statements in the source block are unreachable.
fn lower_statements(
    statements: &[Stmt],
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    entry: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let mut current = Some(entry);
    for statement in statements {
        let Some(block) = current else { break };
        current = lower_statement(statement, context, process, block);
    }
    current
}

/// Lower one statement into `block`, returning the block that execution
/// continues in -- the same one for straight-line statements, a new join
/// block for anything that branches or suspends.
fn lower_statement(
    statement: &Stmt,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    match statement {
        Stmt::Let(declaration) => {
            let local = push_local(process, declaration, context);
            let initializer = declaration
                .value
                .as_ref()
                .map(|value| value_ref(value, process, context));
            process.blocks[block.0 as usize]
                .instructions
                .push(ProcessInstruction::Declare {
                    local,
                    initializer,
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
            let semantics = assignment_semantics(target, process, context);
            let target = value_ref(target, process, context);
            let value = value_ref(value, process, context);
            let instruction = match after {
                Some(delay) => ProcessInstruction::Schedule {
                    target,
                    value,
                    delay: value_ref(delay, process, context),
                    span: *span,
                },
                None => ProcessInstruction::Assign {
                    semantics,
                    target,
                    value,
                    span: *span,
                },
            };
            process.blocks[block.0 as usize]
                .instructions
                .push(instruction);
            Some(block)
        }
        Stmt::Expr(ast::Expr::Call {
            callee, args, span, ..
        }) => lower_call(callee, args, *span, context, process, block),
        Stmt::Expr(expression) => {
            let argument = value_ref(expression, process, context);
            process.blocks[block.0 as usize]
                .instructions
                .push(ProcessInstruction::Runtime {
                    operation: ProcessRuntimeOp::Call("<expression>".to_string()),
                    arguments: vec![argument],
                    span: ast::expr_span(expression),
                });
            Some(block)
        }
        Stmt::If(statement) => lower_if(statement, context, process, block),
        Stmt::Match(statement) => lower_match(statement, context, process, block),
        Stmt::For {
            var,
            range,
            body,
            span,
        } => lower_for(var, range, body, *span, context, process, block),
        Stmt::Return { value, span } => {
            process.blocks[block.0 as usize].terminator = ProcessTerminator::Return {
                value: value
                    .as_ref()
                    .map(|value| value_ref(value, process, context)),
                span: Some(*span),
            };
            None
        }
    }
}

/// Whether a write updates a process local immediately or stages a signal
/// write for the next delta. Classification uses the target's resolved
/// declaration, so a local shadowing a signal name still writes the local.
fn assignment_semantics(
    target: &ast::Expr,
    process: &ProcessCfg,
    context: &LoweringContext<'_>,
) -> ProcessAssignment {
    if let ast::Expr::Concat { parts, .. } = target {
        let mut parts = parts
            .iter()
            .map(|part| assignment_semantics(part, process, context));
        let first = parts.next().unwrap_or(ProcessAssignment::PerPlace);
        return if parts.all(|semantics| semantics == first) {
            first
        } else {
            ProcessAssignment::PerPlace
        };
    }
    let path = assignment_base(target);
    let target = path.and_then(|path| context.resolved.resolved(path.span));
    let is_local = target.is_some_and(|target| {
        process
            .locals
            .iter()
            .any(|local| local.source == Some(target))
    });
    if is_local {
        ProcessAssignment::ImmediateLocal
    } else if path
        .and_then(|path| testbench_storage(path, process.owner, context))
        .is_some()
    {
        ProcessAssignment::ImmediateStorage
    } else {
        ProcessAssignment::StagedSignal
    }
}

/// The path at the base of an assignment target, looking through field and
/// index access. `None` when the target is not a place.
fn assignment_base(target: &ast::Expr) -> Option<&ast::Path> {
    match target {
        ast::Expr::Path(path) => Some(path),
        ast::Expr::Field { base, .. } | ast::Expr::Index { base, .. } => assignment_base(base),
        _ => None,
    }
}

/// Lower a call: either a runtime operation (`assert!`, `print!`) or an
/// ordinary named call that lowering did not inline.
fn lower_call(
    callee: &ast::Expr,
    arguments: &[ast::Expr],
    span: crate::diag::Span,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let name = callee_name(callee);
    let arguments = arguments
        .iter()
        .map(|argument| value_ref(argument, process, context))
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

/// Lower an `if` chain into a two-way branch plus a join block, returning
/// the join.
fn lower_if(
    statement: &ast::IfStmt,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let then_block = push_block(process);
    let else_block = push_block(process);
    process.blocks[block.0 as usize].terminator = ProcessTerminator::Branch {
        condition: value_ref(&statement.cond, process, context),
        then_block,
        else_block,
    };

    let then_tail = lower_statements(&statement.then.stmts, context, process, then_block);
    let else_tail = match statement.else_.as_deref() {
        Some(ElseBranch::Block(block)) => {
            lower_statements(&block.stmts, context, process, else_block)
        }
        Some(ElseBranch::If(statement)) => lower_if(statement, context, process, else_block),
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

/// Lower a `match` into one block per arm plus a join block. A statement
/// match with no wildcard arm can fall through, so the terminator keeps a
/// continuation block for that case.
fn lower_match(
    statement: &ast::MatchStmt,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let mut arms = Vec::with_capacity(statement.arms.len());
    for arm in &statement.arms {
        arms.push(ProcessMatchArm {
            pattern: lower_pattern(&arm.pattern, context.resolved),
            block: push_block(process),
            span: arm.span,
        });
    }
    let exhaustive = statement
        .arms
        .iter()
        .any(|arm| pattern_has_wildcard(&arm.pattern));
    let fallback = (!exhaustive).then(|| push_block(process));
    let scrutinee = value_ref(&statement.scrutinee, process, context);
    process.blocks[block.0 as usize].terminator = ProcessTerminator::Match {
        scrutinee,
        arms: arms.clone(),
        fallback,
    };

    let mut tails = Vec::new();
    for (source, lowered) in statement.arms.iter().zip(&arms) {
        if let Some(tail) = lower_statements(&source.body.stmts, context, process, lowered.block) {
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

/// Convert an AST pattern into its process-IR form.
fn lower_pattern(pattern: &ast::Pattern, resolved: &Resolved) -> ProcessPattern {
    match pattern {
        ast::Pattern::Wildcard => ProcessPattern::Wildcard,
        ast::Pattern::Path(path) => ProcessPattern::Path {
            definition: resolved.resolved(path.span),
            segments: path
                .segments
                .iter()
                .map(|segment| segment.text.clone())
                .collect(),
        },
        ast::Pattern::BitPattern { text, .. } => ProcessPattern::BitPattern(text.clone()),
        ast::Pattern::Or { alts, .. } => ProcessPattern::Or(
            alts.iter()
                .map(|pattern| lower_pattern(pattern, resolved))
                .collect(),
        ),
        ast::Pattern::Range { lo, hi, .. } => ProcessPattern::Range {
            left: *lo,
            right: *hi,
        },
        ast::Pattern::CharLit { ch, .. } => ProcessPattern::Char(*ch),
    }
}

/// Whether a pattern matches everything, so a match needs no fall-through
/// continuation. An `Or` counts when any alternative does.
fn pattern_has_wildcard(pattern: &ast::Pattern) -> bool {
    match pattern {
        ast::Pattern::Wildcard => true,
        ast::Pattern::Or { alts, .. } => alts.iter().any(pattern_has_wildcard),
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
/// Lower a `for` loop into a dedicated header block plus body and exit
/// blocks. The header exists so the body's back-edge does not re-enter
/// instructions that ran before the loop.
fn lower_for(
    variable: &ast::Ident,
    iterable: &ast::Expr,
    body: &ast::Block,
    span: crate::diag::Span,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let local = ProcessLocalId(process.locals.len() as u32);
    let ty = if matches!(iterable, ast::Expr::Range { .. }) {
        Some(crate::types::Ty::Integer)
    } else {
        match context.typed.expr_type(ast::expr_span(iterable)) {
            Some(crate::types::Ty::Array { elem, .. }) => Some((**elem).clone()),
            _ => None,
        }
    };
    process.locals.push(ProcessLocal {
        id: local,
        name: variable.text.clone(),
        source: context.resolved.declared(variable.span),
        span: variable.span,
        ty,
        layout: None,
    });

    let iterable = value_ref(iterable, process, context);
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
    if let Some(tail) = lower_statements(&body.stmts, context, process, body_block) {
        process.blocks[tail.0 as usize].terminator = ProcessTerminator::Goto(header);
    }
    Some(exit)
}

/// Add a local to the process, recording its resolved declaration so that
/// equal spellings in nested scopes stay distinct.
fn push_local(
    process: &mut ProcessCfg,
    declaration: &ast::LetDecl,
    context: &LoweringContext<'_>,
) -> ProcessLocalId {
    let id = ProcessLocalId(process.locals.len() as u32);
    process.locals.push(ProcessLocal {
        id,
        name: declaration.name.text.clone(),
        source: context.resolved.declared(declaration.name.span),
        span: declaration.span,
        ty: declaration
            .value
            .as_ref()
            .and_then(|value| context.typed.expr_type(ast::expr_span(value)))
            .cloned(),
        layout: None,
    });
    id
}

/// Lower an expression recursively into the process operand arena. Children
/// are inserted before their parent, so ids form a directly executable DAG.
fn value_ref(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> crate::ir::ProcessValueId {
    let span = ast::expr_span(expression);
    let ty = context.typed.expr_type(span).cloned();

    let kind = match expression {
        ast::Expr::Path(path) => {
            if let Some(local) = process_local(path, process, context.resolved) {
                ProcessValueKind::Local {
                    process: process.id,
                    local,
                }
            } else if let Some(signals) = signal_reference(expression, process, context) {
                ProcessValueKind::Signal {
                    signals,
                    state: ProcessSignalState::Current,
                }
            } else if let Some(storage) = testbench_storage(path, process.owner, context) {
                ProcessValueKind::Storage(storage)
            } else if let Some(definition) = context.resolved.resolved(path.span) {
                ProcessValueKind::Definition(definition)
            } else {
                ProcessValueKind::Intrinsic(path_name(path))
            }
        }
        ast::Expr::Int { text, .. } => ProcessValueKind::Number(parse_number(text, ty.as_ref())),
        ast::Expr::SuffixLit { text, suffix, .. } => ProcessValueKind::Suffixed {
            number: parse_number(text, ty.as_ref()),
            suffix: suffix.text.clone(),
        },
        ast::Expr::BitStrLit { base, digits, .. } => {
            let radix = crate::syntax::radix_of(*base);
            let width = crate::syntax::radix_digits(digits)
                .count()
                .saturating_mul(radix.ilog2() as usize)
                .try_into()
                .unwrap_or(u32::MAX);
            ProcessValueKind::BitString {
                width,
                words: parse_digits_words(digits, radix),
            }
        }
        ast::Expr::CharLit { ch, .. } => ProcessValueKind::Char(*ch),
        ast::Expr::StrLit { text, .. } => ProcessValueKind::String(text.clone()),
        ast::Expr::Field { base, field, .. } => {
            if let Some(signals) = signal_reference(expression, process, context) {
                ProcessValueKind::Signal {
                    signals,
                    state: ProcessSignalState::Current,
                }
            } else {
                let base = value_ref(base, process, context);
                ProcessValueKind::Field {
                    base,
                    field: field.text.clone(),
                }
            }
        }
        ast::Expr::SysAttr { base, attr, .. } => {
            let state = match attr.text.as_str() {
                "old" => Some(ProcessSignalState::Old),
                "event" => Some(ProcessSignalState::Event),
                _ => None,
            };
            if let Some((state, signals)) = state.zip(signal_reference(base, process, context)) {
                ProcessValueKind::Signal { signals, state }
            } else {
                let base = value_ref(base, process, context);
                ProcessValueKind::Attribute {
                    base,
                    attribute: attr.text.clone(),
                }
            }
        }
        ast::Expr::Index { base, index, .. } => {
            if let Some(signals) = signal_reference(expression, process, context) {
                ProcessValueKind::Signal {
                    signals,
                    state: ProcessSignalState::Current,
                }
            } else {
                let base = value_ref(base, process, context);
                let index = value_ref(index, process, context);
                ProcessValueKind::Index { base, index }
            }
        }
        ast::Expr::Range { lo, hi, .. } => ProcessValueKind::Range {
            left: Some(value_ref(lo, process, context)),
            right: Some(value_ref(hi, process, context)),
        },
        ast::Expr::PartialRange { lo, hi, .. } => ProcessValueKind::Range {
            left: lo
                .as_deref()
                .map(|bound| value_ref(bound, process, context)),
            right: hi
                .as_deref()
                .map(|bound| value_ref(bound, process, context)),
        },
        ast::Expr::Unary { op, rhs, .. } => ProcessValueKind::Unary {
            operation: match op {
                ast::UnOp::Neg => ProcessUnaryOp::Neg,
                ast::UnOp::Not => ProcessUnaryOp::Not,
            },
            operand: value_ref(rhs, process, context),
        },
        ast::Expr::Binary { op, lhs, rhs, .. } => ProcessValueKind::Binary {
            operation: lower_binary_operator(op),
            left: value_ref(lhs, process, context),
            right: value_ref(rhs, process, context),
        },
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => ProcessValueKind::Select {
            condition: value_ref(cond, process, context),
            then_value: value_ref(then, process, context),
            else_value: value_ref(els, process, context),
        },
        ast::Expr::Match {
            scrutinee, arms, ..
        } => {
            let scrutinee = value_ref(scrutinee, process, context);
            let arms = arms
                .iter()
                .map(|arm| {
                    let value = match arm.value_expr() {
                        Some(value) => value_ref(value, process, context),
                        None => missing_value(arm.span, context),
                    };
                    ProcessValueMatchArm {
                        pattern: lower_pattern(&arm.pattern, context.resolved),
                        value,
                        span: arm.span,
                    }
                })
                .collect();
            ProcessValueKind::Match { scrutinee, arms }
        }
        ast::Expr::Call {
            callee,
            type_args,
            args,
            bang,
            ..
        } => {
            let callee = value_ref(callee, process, context);
            let arguments = args
                .iter()
                .map(|argument| value_ref(argument, process, context))
                .collect();
            let type_arguments = if type_args.is_empty() {
                Vec::new()
            } else {
                // Explicit type arguments are currently accepted only by
                // `read<T>`, whose expression type is exactly `T`.
                ty.iter().cloned().collect()
            };
            ProcessValueKind::Call {
                callee,
                type_arguments,
                arguments,
                bang: *bang,
            }
        }
        ast::Expr::Construct { args, spread, .. } => {
            let fields = args
                .iter()
                .map(|field| ProcessAggregateField {
                    name: field.field.as_ref().map(|name| name.text.clone()),
                    value: field
                        .value
                        .as_ref()
                        .map(|value| value_ref(value, process, context)),
                    span: field.span,
                })
                .collect();
            let spread = spread
                .as_deref()
                .map(|value| value_ref(value, process, context));
            ProcessValueKind::Construct {
                ty: ty.clone(),
                fields,
                spread,
            }
        }
        ast::Expr::Concat { parts, .. } => ProcessValueKind::Concat(
            parts
                .iter()
                .map(|part| value_ref(part, process, context))
                .collect(),
        ),
        ast::Expr::Array { elems, .. } => ProcessValueKind::Array(
            elems
                .iter()
                .map(|element| value_ref(element, process, context))
                .collect(),
        ),
    };

    push_value(span, ty, kind, context)
}

/// Insert one already-lowered value node.
fn push_value(
    span: crate::diag::Span,
    ty: Option<crate::types::Ty>,
    kind: ProcessValueKind,
    context: &mut LoweringContext<'_>,
) -> crate::ir::ProcessValueId {
    let id = crate::ir::ProcessValueId(context.process_ir.values.len() as u32);
    context
        .process_ir
        .values
        .push(ProcessValue { span, ty, kind });
    id
}

/// Error-recovery value for a malformed value-level match arm. Correct source
/// never contains this node because type checking rejects a missing arm value.
fn missing_value(
    span: crate::diag::Span,
    context: &mut LoweringContext<'_>,
) -> crate::ir::ProcessValueId {
    push_value(
        span,
        None,
        ProcessValueKind::Intrinsic("<missing-match-value>".to_string()),
        context,
    )
}

/// Find the process-local declaration selected by a path.
fn process_local(
    path: &ast::Path,
    process: &ProcessCfg,
    resolved: &Resolved,
) -> Option<ProcessLocalId> {
    let definition = resolved.resolved(path.span)?;
    process
        .locals
        .iter()
        .find(|local| local.source == Some(definition))
        .map(|local| local.id)
}

/// The persistent testbench storage a path names, if any.
///
/// Tried only after process locals and hardware signals, so lexical shadowing
/// and DUT references keep their existing meaning; this catches the entity-level
/// `let`s of a test entity, which digital lowering deliberately gives no signal.
/// Matching is by resolved declaration, so two roots declaring the same name
/// stay distinct.
fn testbench_storage(
    path: &ast::Path,
    owner: crate::elab::InstanceId,
    context: &LoweringContext<'_>,
) -> Option<ProcessStorageId> {
    let declaration = context.resolved.resolved(path.span)?;
    context
        .process_ir
        .storages
        .iter()
        .find(|storage| storage.owner == owner && storage.source == Some(declaration))
        .map(|storage| storage.id)
}

/// Resolve a source value to its flattened storage leaves. Process locals win
/// over equal signal spellings, preserving lexical shadowing.
fn signal_reference(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &LoweringContext<'_>,
) -> Option<Vec<SignalId>> {
    if assignment_base(expression)
        .and_then(|path| process_local(path, process, context.resolved))
        .is_some()
    {
        return None;
    }
    if !matches!(
        expression,
        ast::Expr::Path(_) | ast::Expr::Field { .. } | ast::Expr::Index { .. }
    ) {
        return None;
    }

    let source_path = crate::syntax::pretty::expr_string(expression);
    let qualified = format!("{}.{}", context.root_path, source_path);
    let exact = context
        .design
        .signals
        .iter()
        .enumerate()
        .find(|(_, signal)| signal.path == qualified)
        .and_then(|(index, _)| u32::try_from(index).ok())
        .filter(|id| !is_representation_signal(context.design, *id));
    if let Some(id) = exact {
        return Some(vec![SignalId(id)]);
    }

    let field_prefix = format!("{qualified}.");
    let index_prefix = format!("{qualified}[");
    let signals = context
        .design
        .signals
        .iter()
        .enumerate()
        .filter(|(_, signal)| {
            signal.path.starts_with(&field_prefix) || signal.path.starts_with(&index_prefix)
        })
        .filter_map(|(index, _)| u32::try_from(index).ok())
        .filter(|id| !is_representation_signal(context.design, *id))
        .map(SignalId)
        .collect::<Vec<_>>();
    (!signals.is_empty()).then_some(signals)
}

/// Turn an integer/real spelling into a source-independent numeric payload.
fn parse_number(text: &str, ty: Option<&crate::types::Ty>) -> ProcessNumber {
    let normalized = text.trim().replace('_', "");
    if normalized.contains('.') || matches!(ty, Some(crate::types::Ty::Real)) {
        return ProcessNumber::Real(normalized.parse::<f64>().unwrap_or(0.0).to_bits());
    }
    let (digits, radix) = if let Some(digits) = normalized
        .strip_prefix("0x")
        .or_else(|| normalized.strip_prefix("0X"))
    {
        (digits, 16)
    } else if let Some(digits) = normalized
        .strip_prefix("0b")
        .or_else(|| normalized.strip_prefix("0B"))
    {
        (digits, 2)
    } else if let Some(digits) = normalized
        .strip_prefix("0o")
        .or_else(|| normalized.strip_prefix("0O"))
    {
        (digits, 8)
    } else {
        (normalized.as_str(), 10)
    };
    ProcessNumber::Integer(parse_digits_words(digits, radix))
}

/// Accumulate arbitrary-width digits into low-word-first storage.
fn parse_digits_words(digits: &str, radix: u32) -> Vec<u64> {
    let mut words = Vec::<u64>::new();
    for digit in crate::syntax::radix_digits(digits) {
        let Some(digit) = digit.to_digit(radix) else {
            return vec![0];
        };
        let mut carry = u128::from(digit);
        for word in &mut words {
            let next = u128::from(*word) * u128::from(radix) + carry;
            *word = next as u64;
            carry = next >> 64;
        }
        if carry != 0 || words.is_empty() {
            words.push(carry as u64);
        }
    }
    words
}

/// Convert a parsed operator to its precedence-free process form.
fn lower_binary_operator(operator: &ast::BinOp) -> ProcessBinaryOp {
    match operator {
        ast::BinOp::Add => ProcessBinaryOp::Add,
        ast::BinOp::Sub => ProcessBinaryOp::Sub,
        ast::BinOp::Mul => ProcessBinaryOp::Mul,
        ast::BinOp::Div => ProcessBinaryOp::Div,
        ast::BinOp::And => ProcessBinaryOp::And,
        ast::BinOp::Or => ProcessBinaryOp::Or,
        ast::BinOp::Custom { symbol, .. } => ProcessBinaryOp::Custom(symbol.clone()),
        ast::BinOp::Shl => ProcessBinaryOp::Shl,
        ast::BinOp::Shr => ProcessBinaryOp::Shr,
        ast::BinOp::Eq => ProcessBinaryOp::Eq,
        ast::BinOp::Ne => ProcessBinaryOp::Ne,
        ast::BinOp::Lt => ProcessBinaryOp::Lt,
        ast::BinOp::Le => ProcessBinaryOp::Le,
        ast::BinOp::Gt => ProcessBinaryOp::Gt,
        ast::BinOp::Ge => ProcessBinaryOp::Ge,
    }
}

/// Namespace-qualified spelling of a path used as an intrinsic dispatch key.
fn path_name(path: &ast::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join("::")
}

/// The callee's rendered name: a `::`-joined path, or the method name for a
/// `.method` call.
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
    /// End-to-end check that a test body fills `Design::process_ir` with the
    /// branches and suspension points its source implies.
    fn fills_design_process_cfg_with_branches_and_suspension() {
        let sources = [
            "module tests;\n\
             enum Mode { Off, On }\n\
             entity Device { input: Bool in, output: Bool out }\n\
             impl Device { output = input; }\n\
             #[std::attrs::test] entity Smoke {}\n\
             impl Smoke {\n\
               let flag: Bool = true;\n\
               let i: integer = 9;\n\
               let observed: Bool;\n\
               let dut: Device = { .input = flag, .output = observed };\n\
               process clock { flag = not flag after 1; }\n\
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
                 flag = false after 1;\n\
                 flag = false;\n\
                 await true;\n\
                 assert!(flag == false, \"done\");\n\
                 finish();\n\
               }\n\
             }",
            "module std::logic; pub enum Bool { false, true }",
            "module std::attrs; using std::logic::{Bool}; pub attr test: Bool for entity;",
            "module std::ops; using std::logic::{Bool}; \
             pub trait Boolean { fn as_bool(self) -> Bool; } \
             pub trait Operator<op: string, input, output> { fn apply(self, rhs: input) -> output {} } \
             impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } } \
             impl Operator<\"not\", Bool, Bool> for Bool { fn apply(self) -> Bool { return self; } }",
            "module std::prelude; pub using std::logic::{Bool}; pub using std::attrs::{test}; \
             pub using std::ops::{Boolean, Operator};",
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
        assert_eq!(design.process_ir.processes.len(), 2);
        assert!(!design.process_ir.values.is_empty());
        let descriptor = &design.process_ir.tests[0];
        assert_eq!(descriptor.qualified_name, "tests::Smoke");
        assert_eq!(descriptor.processes, [ProcessId(0), ProcessId(1)]);
        let flag_storage = design
            .process_ir
            .storages
            .iter()
            .find(|storage| storage.name == "flag")
            .expect("flag storage")
            .id;
        let clock = &design.process_ir.processes[0];
        assert_eq!(clock.label.as_deref(), Some("Smoke::clock"));
        assert_eq!(
            clock.activation,
            ProcessActivation::Reactive {
                sensitivity: vec![ProcessSensitivity::Storage(flag_storage)]
            },
            "a self-toggle clock wakes on its persistent testbench storage"
        );
        assert!(clock.blocks.iter().any(|block| block
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, ProcessInstruction::Schedule { .. }))));
        let process = &design.process_ir.processes[1];
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
        assert!(process.blocks.iter().any(|block| block
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, ProcessInstruction::Schedule { .. }))));
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
        assert!(assignments.contains(&ProcessAssignment::ImmediateStorage));
        let post_loop_i = process
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match instruction {
                ProcessInstruction::Assign {
                    semantics, target, ..
                } if matches!(
                    &design.process_ir.values[target.0 as usize].kind,
                    ProcessValueKind::Storage(storage)
                        if design.process_ir.storages[storage.0 as usize].name == "i"
                ) => Some(*semantics),
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!(
                    "missing post-loop write to the shadowed entity-level `i`; storages={:?}, values={:?}",
                    design
                        .process_ir
                        .storages
                        .iter()
                        .map(|storage| &storage.name)
                        .collect::<Vec<_>>(),
                    design.process_ir.values
                )
            });
        // `i` inside the loop is a process local and `i` after it is the
        // entity-level declaration, so finding storage here is also the
        // shadowing check.
        assert_eq!(post_loop_i, ProcessAssignment::ImmediateStorage);
        assert!(
            design
                .process_ir
                .storages
                .iter()
                .any(|storage| storage.name == "i" && storage.owner == descriptor.root),
            "the entity-level `let i` should be testbench storage owned by the test root"
        );
        let flag = design
            .process_ir
            .storages
            .iter()
            .find(|storage| storage.name == "flag")
            .expect("flag storage");
        assert!(flag.initializer.is_some());
        assert!(flag.bindings.iter().any(|binding| {
            binding.direction == LayoutDirection::In
                && design.signals[binding.signal.0 as usize]
                    .path
                    .ends_with(".dut.input")
        }));
        let observed = design
            .process_ir
            .storages
            .iter()
            .find(|storage| storage.name == "observed")
            .expect("observed storage");
        assert!(observed.bindings.iter().any(|binding| {
            binding.direction == LayoutDirection::Out
                && design.signals[binding.signal.0 as usize]
                    .path
                    .ends_with(".dut.output")
        }));
        let dump = design.process_ir.to_ir_string();
        assert!(dump.contains("value %v0"));
        assert!(dump.contains(&format!(
            "test @tests::Smoke root {} processes [%p0, %p1]",
            descriptor.root.0
        )));
        // The storage arena is part of the product, so `--emit ir` style dumps
        // have to show it or a migration bug there is invisible.
        assert!(
            dump.contains(&format!("storage %g1 root {} i", descriptor.root.0)),
            "the dump should name the entity-level `i` storage:\n{dump}"
        );
        assert!(dump.contains(&format!(
            "process %p1 root {} [Smoke::stimulus]",
            descriptor.root.0
        )));
        assert!(design.process_ir.values.iter().any(|value| matches!(
            value.kind,
            ProcessValueKind::Binary {
                operation: ProcessBinaryOp::Eq,
                ..
            }
        )));
        for (index, value) in design.process_ir.values.iter().enumerate() {
            assert!(
                crate::ir::process_value_dependencies(&value.kind)
                    .into_iter()
                    .all(|dependency| dependency.0 < index as u32),
                "value %{index} is not in dependency order: {value:?}"
            );
        }
    }

    #[test]
    /// Storage bindings retain the direction of each applied-view field. A
    /// whole bus connection can therefore drive some DUT leaves and observe
    /// others without inventing a direction for the backing struct itself.
    fn storage_bindings_follow_applied_view_directions() {
        let sources = [
            "module tests;\n\
             struct Link { pub request: Bool, pub response: Bool }\n\
             view Slave for Link { request in, response out }\n\
             entity Device { bus: Link Slave }\n\
             impl Device { bus.response = bus.request; }\n\
             #[std::attrs::test] entity Smoke {}\n\
             impl Smoke {\n\
               let link: Link;\n\
               let dut: Device = { .bus = link };\n\
               process stimulus {}\n\
             }",
            "module std::logic; pub enum Bool { false, true }",
            "module std::attrs; using std::logic::{Bool}; pub attr test: Bool for entity;",
            "module std::ops; using std::logic::{Bool}; pub trait Boolean { fn as_bool(self) -> Bool; } \
             impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } }",
            "module std::prelude; pub using std::logic::{Bool}; pub using std::attrs::{test}; \
             pub using std::ops::{Boolean};",
        ];
        let mut sink = DiagnosticSink::new();
        let modules = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                crate::syntax::parse_module(FileId(index as u32), source, &mut sink)
            })
            .collect::<Vec<_>>();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        let (hierarchy, plan) = crate::testbench::elaborate(&modules, &resolved, &typed, &mut sink);
        let mut design = crate::ir::lower(&modules, &resolved, &hierarchy, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());

        lower(&modules, &resolved, &typed, &hierarchy, &plan, &mut design);
        let link = design
            .process_ir
            .storages
            .iter()
            .find(|storage| storage.name == "link")
            .expect("link storage");
        let binding = |projection: &str, direction| {
            link.bindings.iter().any(|binding| {
                binding.projection == projection
                    && binding.direction == direction
                    && design.signals[binding.signal.0 as usize]
                        .path
                        .contains(".dut.bus.")
            })
        };
        assert!(binding(".request", LayoutDirection::In));
        assert!(binding(".response", LayoutDirection::Out));
        assert!(design
            .process_ir
            .validate(design.signals.len() as u32)
            .is_empty());
    }

    #[test]
    /// Validation must reject a process whose owner or entry block does not
    /// exist, since neither backend could execute one.
    fn design_validator_rejects_invalid_process_ownership_and_entry() {
        let span = crate::diag::Span::new(FileId(0), 0..1);
        let design = Design {
            process_ir: ProcessIr {
                storages: Vec::new(),
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
