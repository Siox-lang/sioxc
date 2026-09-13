//! Transitional lowering into the canonical process IR.
//!
//! Process/CFG types, validation, test descriptors, and ownership live in
//! [`crate::ir::Design`]. Test stimulus still enters from typed Siox AST;
//! hardware enters through the elaborated, normalized digital scheduler graph
//! so generic/generate/std semantics are not repeated. This module disappears
//! once Process IR becomes the lowering authority for both and the optimized
//! digital forms are derived from it.

use crate::elab::Hierarchy;
use crate::ir::{
    Design, LayoutDirection, LayoutKind, ProcessActivation, ProcessAggregateField,
    ProcessAssignment, ProcessBinaryOp, ProcessBlock, ProcessBlockId, ProcessCfg, ProcessId,
    ProcessInstruction, ProcessIr, ProcessLocal, ProcessLocalId, ProcessMatchArm, ProcessNumber,
    ProcessPattern, ProcessRuntimeOp, ProcessSensitivity, ProcessSignalState, ProcessStorage,
    ProcessStorageBinding, ProcessStorageId, ProcessSuspendOp, ProcessTerminator, ProcessTest,
    ProcessUnaryOp, ProcessValue, ProcessValueId, ProcessValueKind, ProcessValueMatchArm, SignalId,
};
use crate::resolve::Resolved;
use crate::syntax::ast::{self, ElseBranch, ImplItem, Stmt};
use crate::syntax::Module;
use crate::testbench::TestPlan;
use crate::types::Typed;

#[derive(Clone)]
struct ConstantSuffix {
    target: String,
    parameter: String,
    body: ast::Block,
}

struct LoweringContext<'a> {
    resolved: &'a Resolved,
    typed: &'a Typed,
    design: &'a Design,
    root_path: &'a str,
    process_ir: &'a mut ProcessIr,
    suffixes: &'a std::collections::HashMap<String, Vec<ConstantSuffix>>,
    constants: &'a std::collections::HashMap<crate::resolve::DefId, &'a ast::Expr>,
    constant_stack: std::collections::HashSet<crate::resolve::DefId>,
    functions: &'a crate::ir::FunctionIndex<'a>,
    constant_integers: &'a std::collections::HashMap<String, i64>,
    value_bindings: Vec<std::collections::HashMap<crate::resolve::DefId, ProcessValueId>>,
    inline_return_types: Vec<Option<crate::types::Ty>>,
    inline_functions: std::collections::HashSet<crate::diag::Span>,
}

/// Module constants indexed by resolver identity. Their initializers are
/// lowered at each use while syntax and type information are still available,
/// so no backend has to interpret a frontend `Definition` node.
fn module_constants<'a>(
    modules: &'a [Module],
    resolved: &Resolved,
) -> std::collections::HashMap<crate::resolve::DefId, &'a ast::Expr> {
    modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| {
            let ast::Item::Const(constant) = item else {
                return None;
            };
            Some((resolved.declared(constant.name.span)?, &constant.value))
        })
        .collect()
}

/// Functions whose bodies may be evaluated while their arguments are constant
/// or inlined symbolically into a caller's Process value graph.
fn process_functions<'a>(
    modules: &'a [Module],
    resolved: &'a Resolved,
) -> crate::ir::FunctionIndex<'a> {
    let mut functions = crate::ir::FunctionIndex::new(resolved);
    for item in modules.iter().flat_map(|module| &module.items) {
        if let ast::Item::Fn(function) = item {
            functions.insert_free(function);
        }
    }
    for implementation in modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            ast::Item::Impl(implementation) if implementation.trait_.is_none() => {
                Some(implementation)
            }
            _ => None,
        })
    {
        let Some(owner) = functions.type_head_key(&implementation.target) else {
            continue;
        };
        for item in &implementation.items {
            if let ast::ImplItem::Fn(function) = item {
                functions.insert_associated(format!("{owner}::{}", function.name.text), function);
            }
        }
    }
    functions
}

/// Fold module integer constants to seed const-evaluable function calls. The
/// fixed point makes declaration order irrelevant and stops naturally when a
/// rejected cycle or non-integer constant cannot make progress.
fn module_constant_integers(
    modules: &[Module],
    functions: &crate::ir::FunctionIndex<'_>,
) -> std::collections::HashMap<String, i64> {
    let constants = modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            ast::Item::Const(constant) => Some(constant),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut values = std::collections::HashMap::new();
    loop {
        let previous = values.len();
        for constant in &constants {
            let key = functions.constant_decl_key(constant);
            if values.contains_key(&key) {
                continue;
            }
            if let Some(value) = crate::ir::eval_const_fns(&constant.value, &values, functions, 0) {
                values.insert(key, value);
            }
        }
        if values.len() == previous {
            return values;
        }
    }
}

fn type_leaf(ty: &ast::Type) -> Option<&str> {
    match ty {
        ast::Type::Path(path) => path.segments.last().map(|segment| segment.text.as_str()),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => type_leaf(base),
        ast::Type::View { target, .. } => type_leaf(target),
    }
}

/// Nominal type supplied by a declaration when expression-type persistence is
/// intentionally incomplete for constructor syntax. The declaration is the
/// authoritative context for a `let`; retaining it prevents the temporary
/// adapter from replacing a successfully checked newtype with `Ty::Error`.
fn declared_nominal_type(ty: Option<&ast::Type>, resolved: &Resolved) -> Option<crate::types::Ty> {
    let ast::Type::Path(path) = ty? else {
        return None;
    };
    let definition = resolved.resolved(path.span)?;
    matches!(
        resolved.def(definition)?.kind,
        crate::resolve::DefKind::Struct | crate::resolve::DefKind::Enum
    )
    .then_some(crate::types::Ty::Named(definition))
}

fn constant_suffixes(
    modules: &[Module],
    resolved: &Resolved,
) -> std::collections::HashMap<String, Vec<ConstantSuffix>> {
    let mut suffixes = std::collections::HashMap::<String, Vec<ConstantSuffix>>::new();
    for implementation in modules.iter().flat_map(|module| &module.items) {
        let ast::Item::Impl(implementation) = implementation else {
            continue;
        };
        let Some(trait_path) = &implementation.trait_ else {
            continue;
        };
        let canonical = resolved
            .resolved(trait_path.span)
            .and_then(|definition| resolved.def(definition));
        if !canonical.is_some_and(|definition| {
            definition.name == "Suffix"
                && (definition.kind == crate::resolve::DefKind::Builtin
                    || definition.kind == crate::resolve::DefKind::Trait
                        && definition.module.as_deref() == Some("std::ops"))
        }) {
            continue;
        }
        let Some(ast::GenericArg::Positional(ast::Expr::StrLit { text: symbol, .. })) =
            implementation.trait_args.first()
        else {
            continue;
        };
        let Some(target) = type_leaf(&implementation.target) else {
            continue;
        };
        for item in &implementation.items {
            let ast::ImplItem::Fn(function) = item else {
                continue;
            };
            let Some(body) = &function.body else { continue };
            let Some(parameter) = function.params.iter().find(|parameter| !parameter.is_self)
            else {
                continue;
            };
            if parameter.ty.as_ref().and_then(type_leaf) != Some("integer") {
                continue;
            }
            let Some(parameter_name) = parameter.name.as_ref() else {
                continue;
            };
            suffixes
                .entry(symbol.clone())
                .or_default()
                .push(ConstantSuffix {
                    target: target.to_string(),
                    parameter: parameter_name.text.clone(),
                    body: body.clone(),
                });
        }
    }
    suffixes
}

fn integer_literal_u64(text: &str) -> Option<u64> {
    let ProcessNumber::Integer(words) = parse_number(text, None) else {
        return None;
    };
    words
        .get(1..)
        .is_none_or(|rest| rest.iter().all(|word| *word == 0))
        .then(|| words.first().copied().unwrap_or(0))
}

fn eval_suffix_expr(expression: &ast::Expr, suffix: &ConstantSuffix, input: u64) -> Option<u64> {
    match expression {
        ast::Expr::Int { text, .. } => integer_literal_u64(text),
        ast::Expr::Path(path)
            if path.segments.len() == 1 && path.segments[0].text == suffix.parameter =>
        {
            Some(input)
        }
        ast::Expr::Call { callee, args, .. }
            if callee_name(callee) == suffix.target && args.len() == 1 =>
        {
            eval_suffix_expr(&args[0], suffix, input)
        }
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => {
            if eval_suffix_expr(cond, suffix, input)? != 0 {
                eval_suffix_expr(then, suffix, input)
            } else {
                eval_suffix_expr(els, suffix, input)
            }
        }
        ast::Expr::Unary {
            op: ast::UnOp::Not,
            rhs,
            ..
        } => Some(u64::from(eval_suffix_expr(rhs, suffix, input)? == 0)),
        ast::Expr::Unary {
            op: ast::UnOp::Neg, ..
        } => None,
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            let left = eval_suffix_expr(lhs, suffix, input)?;
            let right = eval_suffix_expr(rhs, suffix, input)?;
            match op {
                ast::BinOp::Add => left.checked_add(right),
                ast::BinOp::Sub => left.checked_sub(right),
                ast::BinOp::Mul => left.checked_mul(right),
                ast::BinOp::Div => left.checked_div(right),
                ast::BinOp::Shl => u32::try_from(right)
                    .ok()
                    .and_then(|shift| left.checked_shl(shift)),
                ast::BinOp::Shr => u32::try_from(right)
                    .ok()
                    .and_then(|shift| left.checked_shr(shift)),
                ast::BinOp::Eq => Some(u64::from(left == right)),
                ast::BinOp::Ne => Some(u64::from(left != right)),
                ast::BinOp::Lt => Some(u64::from(left < right)),
                ast::BinOp::Le => Some(u64::from(left <= right)),
                ast::BinOp::Gt => Some(u64::from(left > right)),
                ast::BinOp::Ge => Some(u64::from(left >= right)),
                ast::BinOp::And => Some(u64::from(left != 0 && right != 0)),
                ast::BinOp::Or => Some(u64::from(left != 0 || right != 0)),
                ast::BinOp::Custom { .. } => None,
            }
        }
        _ => None,
    }
}

fn eval_suffix_block(block: &ast::Block, suffix: &ConstantSuffix, input: u64) -> Option<u64> {
    for statement in &block.stmts {
        match statement {
            ast::Stmt::Return {
                value: Some(value), ..
            } => return eval_suffix_expr(value, suffix, input),
            ast::Stmt::If(branch) => {
                let selected = if eval_suffix_expr(&branch.cond, suffix, input)? != 0 {
                    Some(&branch.then)
                } else {
                    match branch.else_.as_deref() {
                        Some(ast::ElseBranch::Block(block)) => Some(block),
                        _ => None,
                    }
                };
                if let Some(value) =
                    selected.and_then(|block| eval_suffix_block(block, suffix, input))
                {
                    return Some(value);
                }
            }
            _ => return None,
        }
    }
    None
}

fn normalized_suffix(
    text: &str,
    symbol: &str,
    context: &LoweringContext<'_>,
) -> Option<ProcessNumber> {
    let [suffix] = context.suffixes.get(symbol)?.as_slice() else {
        return None;
    };
    let input = integer_literal_u64(text)?;
    Some(ProcessNumber::Integer(vec![eval_suffix_block(
        &suffix.body,
        suffix,
        input,
    )?]))
}

/// Fill the canonical process product from normalized hardware plus an
/// optional native-test plan.
///
/// One explicit test process becomes one CFG. Legacy impl-scope test statements
/// remain one implicit foreground process so their existing sequential/`await`
/// behavior is preserved until the syntax is retired. Hardware scheduler units
/// become CFGs after elaboration, including for non-test compiler outputs.
pub fn lower(
    modules: &[Module],
    resolved: &Resolved,
    typed: &Typed,
    hierarchy: &Hierarchy,
    plan: Option<&TestPlan>,
    design: &mut Design,
) {
    let mut process_ir = ProcessIr::default();
    let suffixes = constant_suffixes(modules, resolved);
    let constants = module_constants(modules, resolved);
    let functions = process_functions(modules, resolved);
    let constant_integers = module_constant_integers(modules, &functions);

    for test in plan.into_iter().flat_map(|plan| &plan.tests) {
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
            root: test.root,
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
                suffixes: &suffixes,
                constants: &constants,
                constant_stack: std::collections::HashSet::new(),
                functions: &functions,
                constant_integers: &constant_integers,
                value_bindings: Vec::new(),
                inline_return_types: Vec::new(),
                inline_functions: std::collections::HashSet::new(),
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
                let target = context.process_ir.storages[storage.0 as usize].ty.clone();
                let value = value_ref_with_type(
                    initializer,
                    &initializer_process,
                    &mut context,
                    target.as_ref(),
                );
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
                            suffixes: &suffixes,
                            constants: &constants,
                            constant_stack: std::collections::HashSet::new(),
                            functions: &functions,
                            constant_integers: &constant_integers,
                            value_bindings: Vec::new(),
                            inline_return_types: Vec::new(),
                            inline_functions: std::collections::HashSet::new(),
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
                ImplItem::Stmt(statement) if crate::testbench::is_clock_statement(statement) => {
                    // Legacy impl-scope syntax still denotes a concurrent
                    // clock process. Keeping it in the foreground statement
                    // list made Process IR lose the scheduling boundary even
                    // though the compatibility harness rediscovered it later
                    // by scanning AST. Give it an ordinary reactive CFG now.
                    let id = ProcessId(process_ir.processes.len() as u32);
                    let statements = std::slice::from_ref(statement);
                    let activation = process_activation(
                        statements,
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
                            suffixes: &suffixes,
                            constants: &constants,
                            constant_stack: std::collections::HashSet::new(),
                            functions: &functions,
                            constant_integers: &constant_integers,
                            value_bindings: Vec::new(),
                            inline_return_types: Vec::new(),
                            inline_functions: std::collections::HashSet::new(),
                        };
                        lower_process(
                            id,
                            test.root,
                            Some(format!("{root_path}::<clock:{}>", id.0)),
                            ast::stmt_span(statement),
                            activation,
                            statements,
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
                    suffixes: &suffixes,
                    constants: &constants,
                    constant_stack: std::collections::HashSet::new(),
                    functions: &functions,
                    constant_integers: &constant_integers,
                    value_bindings: Vec::new(),
                    inline_return_types: Vec::new(),
                    inline_functions: std::collections::HashSet::new(),
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

    import_hardware_processes(hierarchy, design, &mut process_ir);

    design.process_ir = process_ir;
}

/// One elaborated instance together with the root and path that own its
/// flattened signals.
struct InstanceLocation {
    id: crate::elab::InstanceId,
    root: crate::elab::InstanceId,
    path: String,
}

/// Convert the normalized hardware scheduler decomposition into ordinary
/// Process IR CFGs. This bridge consumes elaborated digital expressions, not
/// hardware AST, so generic substitution, generate unrolling, std operator
/// evaluation, resolution, and metavalue lowering cannot diverge from the
/// compatibility backend during migration.
fn import_hardware_processes(hierarchy: &Hierarchy, design: &Design, process_ir: &mut ProcessIr) {
    let locations = hierarchy_locations(hierarchy);
    for scheduled in design.processes() {
        let primary = match &scheduled.kind {
            crate::ir::ProcessKind::Comb { target, .. } => Some(*target),
            crate::ir::ProcessKind::Event { block } => design
                .event_blocks
                .get(*block)
                .and_then(|event| event.updates.first())
                .map(|update| update.target)
                .or_else(|| scheduled.reads.first().copied()),
        };
        let Some(primary) = primary else {
            continue;
        };
        let Some(location) = signal_location(primary, design, &locations) else {
            continue;
        };
        let id = ProcessId(process_ir.processes.len() as u32);
        let span = hardware_process_span(&scheduled.kind, primary, design);
        let label = if scheduled.labels.is_empty() {
            Some(format!(
                "{}::<hardware:{}>",
                location.path, design.signals[primary.0 as usize].path
            ))
        } else {
            Some(scheduled.labels.join(" + "))
        };
        let activation = ProcessActivation::Reactive {
            sensitivity: scheduled
                .reads
                .iter()
                .copied()
                .map(ProcessSensitivity::Signal)
                .collect(),
        };
        let mut process = ProcessCfg {
            id,
            root: location.root,
            owner: location.id,
            label,
            span,
            activation,
            entry: ProcessBlockId(0),
            locals: Vec::new(),
            blocks: vec![empty_block(ProcessBlockId(0))],
        };
        match scheduled.kind {
            crate::ir::ProcessKind::Comb { drivers, .. } => {
                let mut tail = ProcessBlockId(0);
                for driver in drivers {
                    let Some(driver) = design.drivers.get(driver) else {
                        continue;
                    };
                    let assignment_span = driver.span.unwrap_or(span);
                    tail = append_digital_assignment(
                        process_ir,
                        &mut process,
                        tail,
                        design,
                        ImportedAssignment {
                            signal: driver.target,
                            expression: &driver.expr,
                            condition: driver.cond.as_ref(),
                            driver_context: driver.ctx,
                            span: assignment_span,
                        },
                    );
                }
            }
            crate::ir::ProcessKind::Event { block } => {
                let Some(event) = design.event_blocks.get(block) else {
                    continue;
                };
                let body = push_block(&mut process);
                let exit = push_block(&mut process);
                let condition = push_normalized_value(process_ir, &event.condition, span, design);
                process.blocks[0].terminator = ProcessTerminator::Branch {
                    condition,
                    then_block: body,
                    else_block: exit,
                };
                let mut tail = body;
                for update in &event.updates {
                    tail = append_digital_assignment(
                        process_ir,
                        &mut process,
                        tail,
                        design,
                        ImportedAssignment {
                            signal: update.target,
                            expression: &update.expr,
                            condition: update.cond.as_ref(),
                            driver_context: event.ctx,
                            span: update.span.unwrap_or(span),
                        },
                    );
                }
                process.blocks[tail.0 as usize].terminator = ProcessTerminator::Goto(exit);
            }
        }
        process_ir.processes.push(process);
        if let Some(test) = process_ir
            .tests
            .iter_mut()
            .find(|test| test.root == location.root)
        {
            test.processes.push(id);
        }
    }
}

/// One assignment imported from the normalized digital compatibility product.
struct ImportedAssignment<'a> {
    signal: SignalId,
    expression: &'a crate::ir::Expr,
    condition: Option<&'a crate::ir::Expr>,
    driver_context: u32,
    span: crate::diag::Span,
}

/// Append one normalized signal assignment, spelling a guard as an explicit
/// branch so the resulting CFG needs no special conditional-write operation.
fn append_digital_assignment(
    process_ir: &mut ProcessIr,
    process: &mut ProcessCfg,
    tail: ProcessBlockId,
    design: &Design,
    assignment: ImportedAssignment<'_>,
) -> ProcessBlockId {
    let ImportedAssignment {
        signal,
        expression,
        condition,
        driver_context,
        span,
    } = assignment;
    let (assignment, next) = if let Some(condition) = condition {
        let assignment = push_block(process);
        let next = push_block(process);
        let condition = push_normalized_value(process_ir, condition, span, design);
        process.blocks[tail.0 as usize].terminator = ProcessTerminator::Branch {
            condition,
            then_block: assignment,
            else_block: next,
        };
        (assignment, Some(next))
    } else {
        (tail, None)
    };
    let target = ProcessValueId(process_ir.values.len() as u32);
    process_ir.values.push(ProcessValue {
        span,
        ty: None,
        bit_width: design.signal_width(signal),
        kind: ProcessValueKind::Signal {
            signals: vec![signal],
            state: ProcessSignalState::Current,
        },
    });
    let value = push_normalized_value(process_ir, expression, span, design);
    process.blocks[assignment.0 as usize]
        .instructions
        .push(ProcessInstruction::Assign {
            semantics: ProcessAssignment::StagedSignal,
            driver_context: Some(driver_context),
            target,
            value,
            span,
        });
    if let Some(next) = next {
        process.blocks[assignment.0 as usize].terminator = ProcessTerminator::Goto(next);
        next
    } else {
        assignment
    }
}

/// Append one already-normalized digital expression and annotate every new
/// arena node with its natural packed width. Normalized expressions have no
/// frontend `Ty`, so retaining this here lets direct backends operate per
/// value rather than falling back to a design-wide machine width.
fn push_normalized_value(
    process_ir: &mut ProcessIr,
    expression: &crate::ir::Expr,
    span: crate::diag::Span,
    design: &Design,
) -> ProcessValueId {
    let first = process_ir.values.len();
    let value = process_ir.push_digital_expr(expression, span);
    for index in first..process_ir.values.len() {
        let width = normalized_value_width(process_ir, ProcessValueId(index as u32), design);
        process_ir.values[index].bit_width = width;
    }
    value
}

/// Natural width of one dependency-ordered normalized value.
fn normalized_value_width(
    process_ir: &ProcessIr,
    id: ProcessValueId,
    design: &Design,
) -> Option<u32> {
    let value = process_ir.values.get(id.0 as usize)?;
    let width = |id: &ProcessValueId| process_ir.values.get(id.0 as usize)?.bit_width;
    let signal_width = |signals: &[SignalId]| {
        signals.iter().try_fold(0u32, |total, signal| {
            total.checked_add(design.signal_width(*signal)?)
        })
    };
    let width = match &value.kind {
        ProcessValueKind::Number(ProcessNumber::Integer(words)) => integer_words_width(words),
        ProcessValueKind::Number(ProcessNumber::Real(_)) | ProcessValueKind::ForeignCall { .. } => {
            Some(64)
        }
        ProcessValueKind::BitString { width, .. } => Some(*width),
        ProcessValueKind::Char(_) => Some(1),
        ProcessValueKind::Signal {
            state: ProcessSignalState::Event,
            ..
        } => Some(1),
        ProcessValueKind::Signal { signals, .. } => signal_width(signals),
        ProcessValueKind::BitSlice { high, low, .. } => high.checked_sub(*low)?.checked_add(1),
        ProcessValueKind::CheckedIndex { index, .. } => width(index),
        ProcessValueKind::TableLookup { table, .. } => design
            .lookup_tables
            .get(table.0)
            .map(|table| table.element_width),
        ProcessValueKind::Unary { operation, operand } => match operation {
            ProcessUnaryOp::RealToInteger => Some(64),
            ProcessUnaryOp::Neg | ProcessUnaryOp::Not => width(operand),
        },
        ProcessValueKind::RawResize { operand } => width(operand),
        ProcessValueKind::Binary {
            operation,
            left,
            right,
        } => match operation {
            ProcessBinaryOp::Eq
            | ProcessBinaryOp::Ne
            | ProcessBinaryOp::Lt
            | ProcessBinaryOp::Le
            | ProcessBinaryOp::Gt
            | ProcessBinaryOp::Ge
            | ProcessBinaryOp::SignedLt
            | ProcessBinaryOp::SignedLe
            | ProcessBinaryOp::SignedGt
            | ProcessBinaryOp::SignedGe
            | ProcessBinaryOp::FloatEq
            | ProcessBinaryOp::FloatNe
            | ProcessBinaryOp::FloatLt
            | ProcessBinaryOp::FloatLe
            | ProcessBinaryOp::FloatGt
            | ProcessBinaryOp::FloatGe => Some(1),
            ProcessBinaryOp::FloatAdd
            | ProcessBinaryOp::FloatSub
            | ProcessBinaryOp::FloatMul
            | ProcessBinaryOp::FloatDiv => Some(64),
            ProcessBinaryOp::Shl => shifted_arena_width(width(left)?, *right, &process_ir.values),
            _ => Some(width(left)?.max(width(right)?)),
        },
        ProcessValueKind::Select {
            then_value,
            else_value,
            ..
        } => Some(width(then_value)?.max(width(else_value)?)),
        ProcessValueKind::MetaCompare { .. } => Some(1),
        ProcessValueKind::Concat(values) => values
            .iter()
            .try_fold(0u32, |total, value| total.checked_add(width(value)?)),
        ProcessValueKind::Suffixed { .. }
        | ProcessValueKind::String(_)
        | ProcessValueKind::Local { .. }
        | ProcessValueKind::Storage(_)
        | ProcessValueKind::Definition(_)
        | ProcessValueKind::Intrinsic(_)
        | ProcessValueKind::Default
        | ProcessValueKind::Field { .. }
        | ProcessValueKind::Attribute { .. }
        | ProcessValueKind::Index { .. }
        | ProcessValueKind::Range { .. }
        | ProcessValueKind::Match { .. }
        | ProcessValueKind::Call { .. }
        | ProcessValueKind::Construct { .. }
        | ProcessValueKind::Array(_)
        | ProcessValueKind::Invalid => None,
    };
    width.filter(|width| *width != 0)
}

/// Width of an arbitrary-precision little-endian integer literal.
fn integer_words_width(words: &[u64]) -> Option<u32> {
    let high = words.last().copied().unwrap_or(0);
    let high_width = (64 - high.leading_zeros()).max(1);
    let lower = u32::try_from(words.len().saturating_sub(1))
        .ok()?
        .checked_mul(64)?;
    lower.checked_add(high_width)
}

/// Natural width after a possibly constant left shift in an arena value graph.
fn shifted_arena_width(left: u32, right: ProcessValueId, values: &[ProcessValue]) -> Option<u32> {
    let Some(shift) = arena_constant_integer(right, values).and_then(|value| value.try_into().ok())
    else {
        return Some(left);
    };
    left.checked_add(shift)
}

/// Conservatively fold an integer-only Process IR value graph. This exists to
/// retain the natural width of normalized expressions such as
/// `1 << (WIDTH - 1)`: treating a constant expression as a dynamic shift would
/// truncate the result before the direct backend ever sees it.
///
/// Dependencies precede their users in the arena, so generated Process IR is
/// acyclic. Values outside the integer subset deliberately return `None` and
/// keep the dynamic-shift width rule.
fn arena_constant_integer(id: ProcessValueId, values: &[ProcessValue]) -> Option<i128> {
    let value = values.get(id.0 as usize)?;
    match &value.kind {
        ProcessValueKind::Number(ProcessNumber::Integer(words)) => {
            let mut result = 0i128;
            for &word in words.iter().rev() {
                result = result.checked_shl(64)?.checked_add(i128::from(word))?;
            }
            Some(result)
        }
        ProcessValueKind::Unary {
            operation: ProcessUnaryOp::Neg,
            operand,
        } => arena_constant_integer(*operand, values)?.checked_neg(),
        ProcessValueKind::Binary {
            operation,
            left,
            right,
        } => {
            let left = arena_constant_integer(*left, values)?;
            let right = arena_constant_integer(*right, values)?;
            match operation {
                ProcessBinaryOp::Add | ProcessBinaryOp::SignedAdd => left.checked_add(right),
                ProcessBinaryOp::Sub | ProcessBinaryOp::SignedSub => left.checked_sub(right),
                ProcessBinaryOp::Mul | ProcessBinaryOp::SignedMul => left.checked_mul(right),
                ProcessBinaryOp::Div | ProcessBinaryOp::SignedDiv => left.checked_div(right),
                ProcessBinaryOp::Shl => left.checked_shl(right.try_into().ok()?),
                ProcessBinaryOp::Shr | ProcessBinaryOp::ArithmeticShr => {
                    left.checked_shr(right.try_into().ok()?)
                }
                ProcessBinaryOp::And => Some(left & right),
                ProcessBinaryOp::Or => Some(left | right),
                ProcessBinaryOp::Xor => Some(left ^ right),
                ProcessBinaryOp::Eq => Some(i128::from(left == right)),
                ProcessBinaryOp::Ne => Some(i128::from(left != right)),
                ProcessBinaryOp::Lt | ProcessBinaryOp::SignedLt => Some(i128::from(left < right)),
                ProcessBinaryOp::Le | ProcessBinaryOp::SignedLe => Some(i128::from(left <= right)),
                ProcessBinaryOp::Gt | ProcessBinaryOp::SignedGt => Some(i128::from(left > right)),
                ProcessBinaryOp::Ge | ProcessBinaryOp::SignedGe => Some(i128::from(left >= right)),
                ProcessBinaryOp::FloatAdd
                | ProcessBinaryOp::FloatSub
                | ProcessBinaryOp::FloatMul
                | ProcessBinaryOp::FloatDiv
                | ProcessBinaryOp::FloatEq
                | ProcessBinaryOp::FloatNe
                | ProcessBinaryOp::FloatLt
                | ProcessBinaryOp::FloatLe
                | ProcessBinaryOp::FloatGt
                | ProcessBinaryOp::FloatGe
                | ProcessBinaryOp::Custom(_) => None,
            }
        }
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        } => {
            let selected = if arena_constant_integer(*condition, values)? != 0 {
                then_value
            } else {
                else_value
            };
            arena_constant_integer(*selected, values)
        }
        _ => None,
    }
}

/// Flatten hierarchy ownership into stable instance paths.
fn hierarchy_locations(hierarchy: &Hierarchy) -> Vec<InstanceLocation> {
    fn visit(
        hierarchy: &Hierarchy,
        id: crate::elab::InstanceId,
        root: crate::elab::InstanceId,
        path: String,
        output: &mut Vec<InstanceLocation>,
    ) {
        output.push(InstanceLocation {
            id,
            root,
            path: path.clone(),
        });
        for &child in &hierarchy.instance(id).children {
            visit(
                hierarchy,
                child,
                root,
                format!("{path}.{}", hierarchy.instance(child).name),
                output,
            );
        }
    }

    let mut output = Vec::new();
    for &root in &hierarchy.roots {
        visit(
            hierarchy,
            root,
            root,
            hierarchy.root_path(root),
            &mut output,
        );
    }
    output
}

/// Find the deepest instance path containing one flattened signal.
fn signal_location<'a>(
    signal: SignalId,
    design: &Design,
    locations: &'a [InstanceLocation],
) -> Option<&'a InstanceLocation> {
    let path = &design.signals.get(signal.0 as usize)?.path;
    locations
        .iter()
        .filter(|location| {
            path == &location.path
                || path
                    .strip_prefix(&location.path)
                    .is_some_and(|rest| rest.starts_with('.'))
        })
        .max_by_key(|location| location.path.len())
}

/// Best source extent for a normalized hardware process.
fn hardware_process_span(
    kind: &crate::ir::ProcessKind,
    primary: SignalId,
    design: &Design,
) -> crate::diag::Span {
    match kind {
        crate::ir::ProcessKind::Comb { drivers, .. } => drivers
            .iter()
            .filter_map(|index| design.drivers.get(*index)?.span)
            .next(),
        crate::ir::ProcessKind::Event { block } => design
            .event_blocks
            .get(*block)
            .and_then(|event| event.updates.iter().find_map(|update| update.span)),
    }
    .unwrap_or(design.signals[primary.0 as usize].declaration_span)
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
        let ty = declaration
            .value
            .as_ref()
            .and_then(|value| typed.expr_type(ast::expr_span(value)))
            .filter(|ty| !matches!(ty, crate::types::Ty::Error))
            .cloned()
            .or_else(|| declared_nominal_type(declaration.ty.as_ref(), resolved));
        process_ir.storages.push(ProcessStorage {
            id,
            owner: root,
            name: name.clone(),
            source: resolved.declared(declaration.name.span),
            span: declaration.span,
            ty,
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
        root: owner,
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
            let target = process.locals[local.0 as usize].ty.clone();
            let initializer = declaration
                .value
                .as_ref()
                .map(|value| value_ref_with_type(value, process, context, target.as_ref()));
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
            let settle = after.is_none()
                && matches!(process.activation, ProcessActivation::TimeZero)
                && assignment_drives_design(target, process, context);
            let target_type = context.typed.expr_type(ast::expr_span(target)).cloned();
            let target = value_ref(target, process, context);
            let value = value_ref_with_type(value, process, context, target_type.as_ref());
            let instruction = match after {
                Some(delay) => ProcessInstruction::Schedule {
                    driver_context: matches!(
                        semantics,
                        ProcessAssignment::StagedSignal | ProcessAssignment::PerPlace
                    )
                    .then_some(process.id.0),
                    target,
                    value,
                    delay: value_ref(delay, process, context),
                    span: *span,
                },
                None => ProcessInstruction::Assign {
                    semantics,
                    driver_context: matches!(
                        semantics,
                        ProcessAssignment::StagedSignal | ProcessAssignment::PerPlace
                    )
                    .then_some(process.id.0),
                    target,
                    value,
                    span: *span,
                },
            };
            process.blocks[block.0 as usize]
                .instructions
                .push(instruction);
            if settle {
                let resume = push_block(process);
                process.blocks[block.0 as usize].terminator = ProcessTerminator::Suspend {
                    operation: ProcessSuspendOp::Settle,
                    arguments: Vec::new(),
                    resume,
                    span: *span,
                };
                Some(resume)
            } else {
                Some(block)
            }
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

/// Whether an immediate foreground assignment drives at least one DUT input.
/// The fixed runtime must publish that storage and reach a reactive fixed
/// point before the next source statement observes connected outputs.
fn assignment_drives_design(
    target: &ast::Expr,
    process: &ProcessCfg,
    context: &LoweringContext<'_>,
) -> bool {
    if let ast::Expr::Concat { parts, .. } = target {
        return parts
            .iter()
            .any(|part| assignment_drives_design(part, process, context));
    }
    assignment_base(target)
        .and_then(|path| testbench_storage(path, process.owner, context))
        .and_then(|storage| context.process_ir.storages.get(storage.0 as usize))
        .is_some_and(|storage| {
            storage.bindings.iter().any(|binding| {
                matches!(
                    binding.direction,
                    LayoutDirection::In | LayoutDirection::InOut
                )
            })
        })
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
            .filter(|ty| !matches!(ty, crate::types::Ty::Error))
            .cloned()
            .or_else(|| declared_nominal_type(declaration.ty.as_ref(), context.resolved)),
        layout: None,
    });
    id
}

/// Resolve an enum-variant declaration to its elaborated discriminant while
/// the resolver is still available. Backends must not rediscover std/user enum
/// values from a `DefId`, and the compiler must not hardcode Bool or logic
/// variant numbers.
fn definition_number(
    definition: crate::resolve::DefId,
    context: &LoweringContext<'_>,
) -> Option<ProcessNumber> {
    let variant = context.resolved.def(definition)?;
    if variant.kind != crate::resolve::DefKind::EnumVariant {
        return None;
    }
    let enumeration = context.resolved.def(variant.parent?)?;
    let qualified = context.resolved.qualified_name(variant.parent?)?;
    let symbols = context
        .design
        .enum_syms
        .get(&qualified)
        .or_else(|| context.design.enum_syms.get(&enumeration.name))?;
    let discriminant = symbols
        .iter()
        .find_map(|(discriminant, symbol)| (symbol == &variant.name).then_some(*discriminant))?;
    Some(ProcessNumber::Integer(vec![discriminant]))
}

/// Resolve a context-typed character literal through the enum declaration's
/// elaborated table. A kernel `Char` remains its Unicode scalar value; a
/// library enum such as `Bit` or `Logic` uses whatever discriminant std chose.
fn character_number(
    character: char,
    ty: Option<&crate::types::Ty>,
    context: &LoweringContext<'_>,
) -> Option<ProcessNumber> {
    let crate::types::Ty::Named(definition) = ty? else {
        return None;
    };
    let enumeration = context.resolved.def(*definition)?;
    let qualified = context.resolved.qualified_name(*definition)?;
    let symbols = context
        .design
        .enum_syms
        .get(&qualified)
        .or_else(|| context.design.enum_syms.get(&enumeration.name))?;
    let quoted = format!("'{character}'");
    let discriminant = symbols.iter().find_map(|(discriminant, symbol)| {
        (symbol == &quoted || symbol == &character.to_string()).then_some(*discriminant)
    })?;
    Some(ProcessNumber::Integer(vec![discriminant]))
}

/// Resolve a function parameter or function-local value in the innermost
/// active inline. Resolver identity keeps equal spellings in nested calls and
/// modules distinct without minting synthetic AST declarations.
fn inline_bound_value(path: &ast::Path, context: &LoweringContext<'_>) -> Option<ProcessValueId> {
    let definition = context.resolved.resolved(path.span)?;
    context
        .value_bindings
        .iter()
        .rev()
        .find_map(|bindings| bindings.get(&definition).copied())
}

/// Lower a value-transparent type application to the language's explicit raw
/// resize operation. Packed families (`unsigned[N](value)`) and nominal
/// one-field newtypes (`Byte(value)`) both preserve the operand's bits while
/// changing its declared type/width; resizing itself always truncates or
/// zero-extends.
fn lower_process_raw_resize(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    target: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let ast::Expr::Call {
        callee,
        type_args,
        args,
        bang: false,
        span,
    } = expression
    else {
        return None;
    };
    if !type_args.is_empty() || args.len() != 1 {
        return None;
    }
    let target = match callee.as_ref() {
        ast::Expr::Index { base, index, .. } => target.cloned().or_else(|| {
            let ast::Expr::Path(path) = base.as_ref() else {
                return None;
            };
            let definition = context.resolved.resolved(path.span)?;
            let family = context.resolved.qualified_name(definition)?;
            let family_known = context.design.array_element_of_family.contains_key(&family)
                || family
                    .rsplit("::")
                    .next()
                    .is_some_and(|leaf| context.design.array_element_of_family.contains_key(leaf));
            if !family_known {
                return None;
            }
            let len =
                crate::ir::eval_const_fns(index, context.constant_integers, context.functions, 0)
                    .and_then(|width| u32::try_from(width).ok())?;
            Some(crate::types::Ty::Array {
                // Packed families carry their width independently of the
                // element type. The family identity is sufficient until this
                // temporary adapter is removed in favour of canonical Process
                // lowering.
                elem: Box::new(crate::types::Ty::Error),
                family: Some(family),
                len,
            })
        })?,
        ast::Expr::Path(path) => {
            let target = target?.clone();
            let crate::types::Ty::Named(target_definition) = target else {
                return None;
            };
            let definition = context.resolved.resolved(path.span)?;
            if definition != target_definition
                || context.resolved.def(definition)?.kind != crate::resolve::DefKind::Struct
            {
                return None;
            }
            crate::types::Ty::Named(target_definition)
        }
        _ => return None,
    };
    if !matches!(
        target,
        crate::types::Ty::Array {
            family: Some(_),
            ..
        } | crate::types::Ty::Named(_)
    ) {
        return None;
    }
    let operand = value_ref(&args[0], process, context);
    let kind = ProcessValueKind::RawResize { operand };
    let width = source_value_width(&kind, Some(&target), process, context)?;
    Some(push_value(*span, Some(target), Some(width), kind, context))
}

/// Lower zero-argument type construction to the type's retained recursive
/// default rather than leaving `T()`/`T::new()` as an executable call. The
/// resolver check distinguishes constructors from ordinary zero-argument
/// functions, whose bodies still go through constant folding or call inlining.
fn lower_process_default(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    target: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let ast::Expr::Call {
        callee,
        type_args,
        args,
        bang: false,
        span,
    } = expression
    else {
        return None;
    };
    if !type_args.is_empty() || !args.is_empty() {
        return None;
    }
    let definition = match callee.as_ref() {
        ast::Expr::Path(path) if path.segments.last()?.text == "new" => context
            .resolved
            .resolved(path.segments.get(path.segments.len().checked_sub(2)?)?.span),
        ast::Expr::Path(path) => context.resolved.resolved(path.span),
        ast::Expr::Index { base, .. } => match base.as_ref() {
            ast::Expr::Path(path) => context.resolved.resolved(path.span),
            _ => None,
        },
        _ => None,
    }?;
    if !matches!(
        context.resolved.def(definition)?.kind,
        crate::resolve::DefKind::Builtin
            | crate::resolve::DefKind::Struct
            | crate::resolve::DefKind::Enum
            | crate::resolve::DefKind::TypeAlias
    ) {
        return None;
    }
    let target = target?.clone();
    if matches!(target, crate::types::Ty::Error) {
        return None;
    }
    let kind = ProcessValueKind::Default;
    let width = source_value_width(&kind, Some(&target), process, context);
    Some(push_value(*span, Some(target), width, kind, context))
}

/// Inline a pure, value-returning Siox function into the Process value arena.
/// Parameters and `let` bindings remain compile-time SSA aliases; control
/// flow becomes value-level selection, so the backend never needs an AST or a
/// resolver to execute the call.
fn inline_process_call(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let ast::Expr::Call { callee, args, .. } = expression else {
        return None;
    };
    let function = context.functions.get(callee)?;
    let body = function.body.as_ref()?;
    if function.ret.is_none() || function.params.iter().any(|parameter| parameter.is_self) {
        return None;
    }
    let parameters = function
        .params
        .iter()
        .filter(|parameter| !parameter.is_self)
        .collect::<Vec<_>>();
    if parameters.len() != args.len() {
        return None;
    }

    let first_value = context.process_ir.values.len();
    let arguments = args
        .iter()
        .map(|argument| value_ref(argument, process, context))
        .collect::<Vec<_>>();
    let mut bindings = std::collections::HashMap::new();
    for (parameter, argument) in parameters.into_iter().zip(arguments) {
        let Some(name) = parameter.name.as_ref() else {
            context.process_ir.values.truncate(first_value);
            return None;
        };
        let Some(definition) = context.resolved.declared(name.span) else {
            context.process_ir.values.truncate(first_value);
            return None;
        };
        bindings.insert(definition, argument);
    }
    if !context.inline_functions.insert(function.span) {
        context.process_ir.values.truncate(first_value);
        return None;
    }

    context.value_bindings.push(bindings);
    context.inline_return_types.push(return_type.cloned());
    let result = inline_value_statements(&body.stmts, process, context);
    context.inline_return_types.pop();
    context.value_bindings.pop();
    context.inline_functions.remove(&function.span);
    if result.is_none() {
        context.process_ir.values.truncate(first_value);
    }
    result
}

/// Evaluate a pure function statement sequence symbolically. `return`, local
/// aliases, and branching cover the expression-shaped Siox functions shared
/// by std and hardware lowering; other statements deliberately leave the call
/// explicit and fail closed in the direct backend.
fn inline_value_statements(
    statements: &[Stmt],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let (statement, rest) = statements.split_first()?;
    match statement {
        Stmt::Return {
            value: Some(value), ..
        } => {
            let return_type = context.inline_return_types.last().cloned().flatten();
            Some(value_ref_with_type(
                value,
                process,
                context,
                return_type.as_ref(),
            ))
        }
        Stmt::Let(declaration) => {
            let value = value_ref(declaration.value.as_ref()?, process, context);
            let definition = context.resolved.declared(declaration.name.span)?;
            context.value_bindings.last_mut()?.insert(definition, value);
            inline_value_statements(rest, process, context)
        }
        Stmt::If(statement) => inline_value_if(statement, rest, process, context),
        Stmt::Match(statement) => inline_value_match(statement, rest, process, context),
        _ => None,
    }
}

/// Inline one branch in a fresh lexical binding scope, appending the source
/// continuation so a branch without an early return falls through normally.
fn inline_value_branch(
    branch: &[Stmt],
    continuation: &[Stmt],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let mut statements = Vec::with_capacity(branch.len() + continuation.len());
    statements.extend_from_slice(branch);
    statements.extend_from_slice(continuation);
    context
        .value_bindings
        .push(std::collections::HashMap::new());
    let result = inline_value_statements(&statements, process, context);
    context.value_bindings.pop();
    result
}

/// Turn a function-body `if` into a dependency-ordered Process selection.
fn inline_value_if(
    statement: &ast::IfStmt,
    continuation: &[Stmt],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let condition = value_ref(&statement.cond, process, context);
    let then_value = inline_value_branch(&statement.then.stmts, continuation, process, context)?;
    let else_value = match statement.else_.as_deref() {
        Some(ElseBranch::Block(block)) => {
            inline_value_branch(&block.stmts, continuation, process, context)?
        }
        Some(ElseBranch::If(inner)) => {
            let branch = [Stmt::If(inner.clone())];
            inline_value_branch(&branch, continuation, process, context)?
        }
        None => inline_value_branch(&[], continuation, process, context)?,
    };
    inline_select_value(statement.span, condition, then_value, else_value, context)
}

/// Turn a function-body `match` into first-match-priority selections. Pattern
/// decoding happens while enum identities and typed character literals are
/// still available; the resulting Process graph contains only executable
/// comparisons and selects.
fn inline_value_match(
    statement: &ast::MatchStmt,
    continuation: &[Stmt],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let scrutinee = value_ref(&statement.scrutinee, process, context);
    let mut result = inline_value_branch(&[], continuation, process, context);
    for arm in statement.arms.iter().rev() {
        let value = inline_value_branch(&arm.body.stmts, continuation, process, context)?;
        match inline_pattern_condition(&arm.pattern, scrutinee, context)? {
            None => result = Some(value),
            Some(condition) => {
                result = Some(match result {
                    Some(fallback) => {
                        inline_select_value(arm.span, condition, value, fallback, context)?
                    }
                    None => value,
                });
            }
        }
    }
    result
}

/// `None` inside the outer option denotes a wildcard; an absent outer option
/// means the pattern cannot yet be represented directly.
fn inline_pattern_condition(
    pattern: &ast::Pattern,
    scrutinee: ProcessValueId,
    context: &mut LoweringContext<'_>,
) -> Option<Option<ProcessValueId>> {
    let (span, candidate) = match pattern {
        ast::Pattern::Wildcard => return Some(None),
        ast::Pattern::Path(path) => {
            let definition = context.resolved.resolved(path.span)?;
            let number = definition_number(definition, context)?;
            (path.span, number)
        }
        ast::Pattern::CharLit { ch, span } => {
            let ty = context
                .process_ir
                .values
                .get(scrutinee.0 as usize)?
                .ty
                .as_ref();
            let number = character_number(*ch, ty, context)?;
            (*span, number)
        }
        ast::Pattern::Or { alts, span } => {
            let mut condition = None;
            for alternative in alts {
                let Some(alternative) = inline_pattern_condition(alternative, scrutinee, context)?
                else {
                    return Some(None);
                };
                condition = Some(match condition {
                    Some(previous) => push_inline_binary(
                        *span,
                        ProcessBinaryOp::Or,
                        previous,
                        alternative,
                        Some(1),
                        context,
                    ),
                    None => alternative,
                });
            }
            return Some(condition);
        }
        ast::Pattern::BitPattern { .. } | ast::Pattern::Range { .. } => return None,
    };
    let scrutinee_node = context.process_ir.values.get(scrutinee.0 as usize)?;
    let candidate = push_value(
        span,
        scrutinee_node.ty.clone(),
        scrutinee_node.bit_width,
        ProcessValueKind::Number(candidate),
        context,
    );
    Some(Some(push_inline_binary(
        span,
        ProcessBinaryOp::Eq,
        scrutinee,
        candidate,
        Some(1),
        context,
    )))
}

/// Append a scalar binary node used by symbolic function control flow.
fn push_inline_binary(
    span: crate::diag::Span,
    operation: ProcessBinaryOp,
    left: ProcessValueId,
    right: ProcessValueId,
    width: Option<u32>,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    push_value(
        span,
        None,
        width,
        ProcessValueKind::Binary {
            operation,
            left,
            right,
        },
        context,
    )
}

/// Append one selection while retaining the common result shape.
fn inline_select_value(
    span: crate::diag::Span,
    condition: ProcessValueId,
    then_value: ProcessValueId,
    else_value: ProcessValueId,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let then_node = context.process_ir.values.get(then_value.0 as usize)?;
    let else_node = context.process_ir.values.get(else_value.0 as usize)?;
    let ty = then_node.ty.clone().or_else(|| else_node.ty.clone());
    let width = match (then_node.bit_width, else_node.bit_width) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    };
    Some(push_value(
        span,
        ty,
        width,
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        },
        context,
    ))
}

/// Lower an expression recursively into the process operand arena. Children
/// are inserted before their parent, so ids form a directly executable DAG.
fn value_ref(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> crate::ir::ProcessValueId {
    value_ref_with_type(expression, process, context, None)
}

/// Lower one value with an optional contextual type for its root. Constant
/// aliases use the type of the path being read: a declaration such as
/// `const HIGH: Bit = '1'` must remain the `Bit` discriminant rather than the
/// Unicode code point of a standalone `Char` expression.
fn value_ref_with_type(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    contextual_type: Option<&crate::types::Ty>,
) -> crate::ir::ProcessValueId {
    let span = ast::expr_span(expression);
    let ty = contextual_type
        .cloned()
        .or_else(|| context.typed.expr_type(span).cloned());

    if let ast::Expr::Path(path) = expression {
        if let Some(value) = inline_bound_value(path, context) {
            return value;
        }
    }

    // Constants are aliases for their initializer values, not runtime
    // storage. Inline them before constructing the surrounding node so the
    // Process IR contains only executable value forms. The stack is merely
    // best-effort recovery for a rejected constant cycle.
    if let ast::Expr::Path(path) = expression {
        if let Some(definition) = context.resolved.resolved(path.span) {
            if let Some(initializer) = context.constants.get(&definition).copied() {
                if context.constant_stack.insert(definition) {
                    let value = value_ref_with_type(initializer, process, context, ty.as_ref());
                    context.constant_stack.remove(&definition);
                    return value;
                }
            }
        }
    }

    if matches!(expression, ast::Expr::Call { .. }) {
        if let Some(value) =
            crate::ir::eval_const_fns(expression, context.constant_integers, context.functions, 0)
        {
            let kind = ProcessValueKind::Number(ProcessNumber::Integer(vec![value as u64]));
            let width = source_value_width(&kind, ty.as_ref(), process, context);
            return push_value(span, ty, width, kind, context);
        }
        if let Some(value) = lower_process_raw_resize(expression, process, context, ty.as_ref()) {
            return value;
        }
        if let Some(value) = lower_process_default(expression, process, context, ty.as_ref()) {
            return value;
        }
        if let Some(value) = inline_process_call(expression, process, context, ty.as_ref()) {
            return value;
        }
    }

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
                match definition_number(definition, context) {
                    Some(number) => ProcessValueKind::Number(number),
                    None => ProcessValueKind::Definition(definition),
                }
            } else {
                ProcessValueKind::Intrinsic(path_name(path))
            }
        }
        ast::Expr::Int { text, .. } => ProcessValueKind::Number(parse_number(text, ty.as_ref())),
        ast::Expr::SuffixLit { text, suffix, .. } => {
            match normalized_suffix(text, &suffix.text, context) {
                Some(number) => ProcessValueKind::Number(number),
                None => ProcessValueKind::Suffixed {
                    number: parse_number(text, ty.as_ref()),
                    suffix: suffix.text.clone(),
                },
            }
        }
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
        ast::Expr::CharLit { ch, .. } => match character_number(*ch, ty.as_ref(), context) {
            Some(number) => ProcessValueKind::Number(number),
            None => ProcessValueKind::Char(*ch),
        },
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
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            // Character literals are context-typed enum values. Type checking
            // records the counterpart but deliberately keeps the literal's
            // standalone `Char` identity, so retain the counterpart here
            // before its declaration identity disappears.
            let left_context = matches!(lhs.as_ref(), ast::Expr::CharLit { .. })
                .then(|| context.typed.expr_type(ast::expr_span(rhs)).cloned())
                .flatten();
            let right_context = matches!(rhs.as_ref(), ast::Expr::CharLit { .. })
                .then(|| context.typed.expr_type(ast::expr_span(lhs)).cloned())
                .flatten();
            let left_type = context.typed.expr_type(ast::expr_span(lhs));
            let right_type = context.typed.expr_type(ast::expr_span(rhs));
            ProcessValueKind::Binary {
                operation: lower_binary_operator(op, left_type, right_type),
                left: value_ref_with_type(lhs, process, context, left_context.as_ref()),
                right: value_ref_with_type(rhs, process, context, right_context.as_ref()),
            }
        }
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

    let width = source_value_width(&kind, ty.as_ref(), process, context);
    push_value(span, ty, width, kind, context)
}

/// Insert one already-lowered value node.
fn push_value(
    span: crate::diag::Span,
    ty: Option<crate::types::Ty>,
    width: Option<u32>,
    kind: ProcessValueKind,
    context: &mut LoweringContext<'_>,
) -> crate::ir::ProcessValueId {
    let id = crate::ir::ProcessValueId(context.process_ir.values.len() as u32);
    context.process_ir.values.push(ProcessValue {
        span,
        ty,
        bit_width: width,
        kind,
    });
    id
}

/// Packed width known at the temporary typed-AST adapter boundary. Composite
/// runtime values retain their recursive layout elsewhere; this records only
/// the scalar width a direct LLVM operation may rely on.
fn source_value_width(
    kind: &ProcessValueKind,
    ty: Option<&crate::types::Ty>,
    process: &ProcessCfg,
    context: &LoweringContext<'_>,
) -> Option<u32> {
    let typed_width = |ty: &crate::types::Ty| {
        ty.bit_width()
            .or_else(|| {
                let crate::types::Ty::Named(definition) = ty else {
                    return None;
                };
                let qualified = context.resolved.qualified_name(*definition)?;
                let symbols = context.design.enum_syms.get(&qualified).or_else(|| {
                    qualified
                        .rsplit("::")
                        .next()
                        .and_then(|name| context.design.enum_syms.get(name))
                })?;
                let highest = symbols.keys().copied().max().unwrap_or(0);
                Some((u64::BITS - highest.leading_zeros()).max(1))
            })
            .or_else(|| {
                let mut widths = context
                    .process_ir
                    .storages
                    .iter()
                    .filter(|storage| storage.ty.as_ref() == Some(ty))
                    .filter_map(|storage| storage.layout.as_ref())
                    .chain(
                        process
                            .locals
                            .iter()
                            .filter(|local| local.ty.as_ref() == Some(ty))
                            .filter_map(|local| local.layout.as_ref()),
                    )
                    .filter_map(|layout| layout.bit_width()?.try_into().ok());
                let first = widths.next()?;
                widths.all(|width| width == first).then_some(first)
            })
    };
    if let ProcessValueKind::Number(ProcessNumber::Integer(words)) = kind {
        let natural = integer_words_width(words)?;
        return Some(
            ty.and_then(&typed_width)
                .map_or(natural, |contextual| natural.max(contextual)),
        );
    }
    if let Some(width) = ty.and_then(&typed_width).filter(|width| *width != 0) {
        return Some(width);
    }
    let width = |id: &ProcessValueId| context.process_ir.values.get(id.0 as usize)?.bit_width;
    let width = match kind {
        ProcessValueKind::Number(ProcessNumber::Integer(words)) => integer_words_width(words),
        ProcessValueKind::Number(ProcessNumber::Real(_)) | ProcessValueKind::ForeignCall { .. } => {
            Some(64)
        }
        ProcessValueKind::Suffixed { number, .. } => match number {
            ProcessNumber::Integer(_) | ProcessNumber::Real(_) => Some(64),
        },
        ProcessValueKind::BitString { width, .. } => Some(*width),
        ProcessValueKind::Char(_) => Some(32),
        ProcessValueKind::String(value) => {
            u32::try_from(value.chars().count()).ok()?.checked_mul(32)
        }
        ProcessValueKind::Local { local, .. } => {
            let local = process.locals.get(local.0 as usize)?;
            local
                .layout
                .as_ref()
                .and_then(|layout| layout.bit_width()?.try_into().ok())
                .or_else(|| local.ty.as_ref().and_then(typed_width))
        }
        ProcessValueKind::Storage(storage) => context
            .process_ir
            .storages
            .get(storage.0 as usize)?
            .layout
            .as_ref()?
            .bit_width()?
            .try_into()
            .ok(),
        ProcessValueKind::Signal {
            state: ProcessSignalState::Event,
            ..
        } => Some(1),
        ProcessValueKind::Signal { signals, .. } => {
            signals.iter().try_fold(0u32, |total, signal| {
                total.checked_add(context.design.signal_width(*signal)?)
            })
        }
        ProcessValueKind::BitSlice { high, low, .. } => high.checked_sub(*low)?.checked_add(1),
        ProcessValueKind::CheckedIndex { index, .. } => width(index),
        ProcessValueKind::TableLookup { table, .. } => context
            .design
            .lookup_tables
            .get(table.0)
            .map(|table| table.element_width),
        ProcessValueKind::Unary { operation, operand } => match operation {
            ProcessUnaryOp::RealToInteger => Some(64),
            ProcessUnaryOp::Neg | ProcessUnaryOp::Not => width(operand),
        },
        ProcessValueKind::RawResize { operand } => width(operand),
        ProcessValueKind::Binary {
            operation,
            left,
            right,
        } => match operation {
            ProcessBinaryOp::Eq
            | ProcessBinaryOp::Ne
            | ProcessBinaryOp::Lt
            | ProcessBinaryOp::Le
            | ProcessBinaryOp::Gt
            | ProcessBinaryOp::Ge
            | ProcessBinaryOp::SignedLt
            | ProcessBinaryOp::SignedLe
            | ProcessBinaryOp::SignedGt
            | ProcessBinaryOp::SignedGe
            | ProcessBinaryOp::FloatEq
            | ProcessBinaryOp::FloatNe
            | ProcessBinaryOp::FloatLt
            | ProcessBinaryOp::FloatLe
            | ProcessBinaryOp::FloatGt
            | ProcessBinaryOp::FloatGe => Some(1),
            ProcessBinaryOp::FloatAdd
            | ProcessBinaryOp::FloatSub
            | ProcessBinaryOp::FloatMul
            | ProcessBinaryOp::FloatDiv => Some(64),
            ProcessBinaryOp::Shl => shifted_width(width(left)?, *right, context),
            _ => Some(width(left)?.max(width(right)?)),
        },
        ProcessValueKind::Select {
            then_value,
            else_value,
            ..
        } => Some(width(then_value)?.max(width(else_value)?)),
        ProcessValueKind::MetaCompare { .. } => Some(1),
        ProcessValueKind::Concat(values) | ProcessValueKind::Array(values) => values
            .iter()
            .try_fold(0u32, |total, value| total.checked_add(width(value)?)),
        ProcessValueKind::Definition(_)
        | ProcessValueKind::Intrinsic(_)
        | ProcessValueKind::Default
        | ProcessValueKind::Field { .. }
        | ProcessValueKind::Attribute { .. }
        | ProcessValueKind::Index { .. }
        | ProcessValueKind::Range { .. }
        | ProcessValueKind::Match { .. }
        | ProcessValueKind::Call { .. }
        | ProcessValueKind::Construct { .. }
        | ProcessValueKind::Invalid => None,
    };
    width.filter(|width| *width != 0)
}

/// Add a constant shift to a value's natural width; dynamic shifts retain the
/// left operand's width, matching normalized digital expression inference.
fn shifted_width(left: u32, right: ProcessValueId, context: &LoweringContext<'_>) -> Option<u32> {
    shifted_arena_width(left, right, &context.process_ir.values)
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
fn lower_binary_operator(
    operator: &ast::BinOp,
    left: Option<&crate::types::Ty>,
    right: Option<&crate::types::Ty>,
) -> ProcessBinaryOp {
    let real = [left, right]
        .into_iter()
        .flatten()
        .any(|ty| matches!(ty, crate::types::Ty::Real));
    // A contextual integer literal adopts the numeric vector family beside
    // it. `unsigned[4] * 2` is therefore unsigned even though the standalone
    // literal's fallback type is the signed kernel integer; a `signed`
    // family beside that same literal selects signed operations.
    let types = [left, right].into_iter().flatten().collect::<Vec<_>>();
    let signed = types
        .iter()
        .find_map(|ty| match ty {
            crate::types::Ty::Array {
                family: Some(family),
                ..
            } => Some(family.rsplit("::").next() == Some("signed")),
            _ => None,
        })
        .unwrap_or_else(|| types.into_iter().any(process_type_is_signed));
    match operator {
        ast::BinOp::Add if real => ProcessBinaryOp::FloatAdd,
        ast::BinOp::Sub if real => ProcessBinaryOp::FloatSub,
        ast::BinOp::Mul if real => ProcessBinaryOp::FloatMul,
        ast::BinOp::Div if real => ProcessBinaryOp::FloatDiv,
        ast::BinOp::Eq if real => ProcessBinaryOp::FloatEq,
        ast::BinOp::Ne if real => ProcessBinaryOp::FloatNe,
        ast::BinOp::Lt if real => ProcessBinaryOp::FloatLt,
        ast::BinOp::Le if real => ProcessBinaryOp::FloatLe,
        ast::BinOp::Gt if real => ProcessBinaryOp::FloatGt,
        ast::BinOp::Ge if real => ProcessBinaryOp::FloatGe,
        ast::BinOp::Add if signed => ProcessBinaryOp::SignedAdd,
        ast::BinOp::Sub if signed => ProcessBinaryOp::SignedSub,
        ast::BinOp::Mul if signed => ProcessBinaryOp::SignedMul,
        ast::BinOp::Div if signed => ProcessBinaryOp::SignedDiv,
        ast::BinOp::Shr if signed => ProcessBinaryOp::ArithmeticShr,
        ast::BinOp::Lt if signed => ProcessBinaryOp::SignedLt,
        ast::BinOp::Le if signed => ProcessBinaryOp::SignedLe,
        ast::BinOp::Gt if signed => ProcessBinaryOp::SignedGt,
        ast::BinOp::Ge if signed => ProcessBinaryOp::SignedGe,
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

/// Whether a checked source type carries signed numeric semantics. The
/// compiler is allowed to identify type families; their values and operator
/// tables remain owned by std.
fn process_type_is_signed(ty: &crate::types::Ty) -> bool {
    matches!(ty, crate::types::Ty::Integer)
        || matches!(
            ty,
            crate::types::Ty::Array {
                family: Some(family),
                ..
            } if family.rsplit("::").next() == Some("signed")
        )
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
               process clock { flag = not flag after 1ns; }\n\
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
                 await 2ns;\n\
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
             impl Operator<\"not\", Bool, Bool> for Bool { fn apply(self) -> Bool { return self; } } \
             pub trait Suffix<symbol: string, input> { fn suffix(data: input) {} }",
            "module std::prelude; pub using std::logic::{Bool}; pub using std::attrs::{test}; \
             pub using std::ops::{Boolean, Operator};",
            "module std::sim; using std::ops::Suffix; pub struct time(integer); \
             impl Suffix<\"ns\", integer> for time { \
               fn suffix(value: integer) -> time { return time(value * 37); } \
             }",
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

        lower(
            &modules,
            &resolved,
            &typed,
            &hierarchy,
            Some(&plan),
            &mut design,
        );
        assert!(design
            .process_ir
            .validate(design.signals.len() as u32)
            .is_empty());
        assert_eq!(design.process_ir.tests.len(), 1);
        assert_eq!(design.process_ir.processes.len(), 3);
        assert!(!design.process_ir.values.is_empty());
        let descriptor = &design.process_ir.tests[0];
        assert_eq!(descriptor.qualified_name, "tests::Smoke");
        assert_eq!(
            descriptor.processes,
            [ProcessId(0), ProcessId(1), ProcessId(2)]
        );
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
        assert!(clock
            .blocks
            .iter()
            .any(
                |block| block.instructions.iter().any(|instruction| matches!(
                    instruction,
                    ProcessInstruction::Schedule { delay, .. }
                        if matches!(
                            &design.process_ir.values[delay.0 as usize].kind,
                            ProcessValueKind::Number(ProcessNumber::Integer(words))
                                if words == &[37]
                        )
                ))
            ));
        let process = &design.process_ir.processes[1];
        assert_eq!(process.label.as_deref(), Some("Smoke::stimulus"));
        assert_eq!(process.locals.len(), 3);
        assert!(process
            .blocks
            .iter()
            .any(|block| matches!(block.terminator, ProcessTerminator::Branch { .. })));
        assert!(process.blocks.iter().any(|block| matches!(
            &block.terminator,
            ProcessTerminator::Suspend {
                operation: ProcessSuspendOp::Await,
                arguments,
                ..
            }
                if matches!(
                    &design.process_ir.values[arguments[0].0 as usize].kind,
                    ProcessValueKind::Number(ProcessNumber::Integer(words))
                        if words == &[74]
                )
        )));
        assert!(
            process.blocks.iter().any(|block| matches!(
                &block.terminator,
                ProcessTerminator::Suspend {
                    operation: ProcessSuspendOp::Settle,
                    arguments,
                    ..
                } if arguments.is_empty()
            )),
            "a foreground drive into the DUT must yield until reactive settling"
        );
        let hardware = &design.process_ir.processes[2];
        let input = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(".dut.input"))
            .map(|index| SignalId(index as u32))
            .expect("DUT input signal");
        let output = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(".dut.output"))
            .map(|index| SignalId(index as u32))
            .expect("DUT output signal");
        assert_eq!(hardware.root, descriptor.root);
        assert_ne!(hardware.owner, descriptor.root);
        assert_eq!(
            hardware.activation,
            ProcessActivation::Reactive {
                sensitivity: vec![ProcessSensitivity::Signal(input)]
            }
        );
        assert!(hardware
            .blocks
            .iter()
            .any(
                |block| block.instructions.iter().any(|instruction| matches!(
                    instruction,
                    ProcessInstruction::Assign {
                        semantics: ProcessAssignment::StagedSignal,
                        driver_context: Some(_),
                        target,
                        value,
                        ..
                    } if matches!(
                        &design.process_ir.values[target.0 as usize].kind,
                        ProcessValueKind::Signal { signals, .. } if signals == &[output]
                    ) && matches!(
                        &design.process_ir.values[value.0 as usize].kind,
                        ProcessValueKind::Signal { signals, .. } if signals == &[input]
                    )
                ))
            ));
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
        assert!(design.process_ir.processes[0]
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                ProcessInstruction::Schedule {
                    driver_context: None,
                    ..
                }
            )));
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
            "test @tests::Smoke root {} processes [%p0, %p1, %p2]",
            descriptor.root.0
        )));
        // The storage arena is part of the product, so `--emit ir` style dumps
        // have to show it or a migration bug there is invisible.
        assert!(
            dump.contains(&format!("storage %g1 root {} i", descriptor.root.0)),
            "the dump should name the entity-level `i` storage:\n{dump}"
        );
        assert!(dump.contains(&format!(
            "process %p1 root {} owner {} [Smoke::stimulus]",
            descriptor.root.0, descriptor.root.0
        )));
        assert!(design.process_ir.values.iter().any(|value| matches!(
            value.kind,
            ProcessValueKind::Binary {
                operation: ProcessBinaryOp::Eq,
                ..
            }
        )));
        assert!(
            design.process_ir.values.iter().all(|value| {
                !matches!(
                    value.kind,
                    ProcessValueKind::Local { .. } | ProcessValueKind::Storage(_)
                ) || value.bit_width.is_some()
            }),
            "scalar state references need direct-backend widths: {:#?}",
            design.process_ir.values
        );
        for (index, value) in design.process_ir.values.iter().enumerate() {
            assert!(
                crate::ir::process::process_value_dependencies(&value.kind)
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

        lower(
            &modules,
            &resolved,
            &typed,
            &hierarchy,
            Some(&plan),
            &mut design,
        );
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
    /// Hardware CFG construction is not conditional on native-test discovery:
    /// ordinary IR/object compilations carry the same canonical process graph.
    fn hardware_processes_lower_without_a_test_plan() {
        let sources = [
            "module gates; entity Gate { input: Bool in, output: Bool out } \
             impl Gate { output = input; }",
            "module std::logic; pub enum Bool { false, true }",
            "module std::ops; using std::logic::{Bool}; pub trait Boolean { fn as_bool(self) -> Bool; } \
             impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } }",
            "module std::prelude; pub using std::logic::{Bool}; pub using std::ops::{Boolean};",
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
        let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
        let mut design = crate::ir::lower(&modules, &resolved, &hierarchy, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());

        lower(&modules, &resolved, &typed, &hierarchy, None, &mut design);
        assert!(design.process_ir.tests.is_empty());
        assert_eq!(design.process_ir.processes.len(), 1);
        let process = &design.process_ir.processes[0];
        assert_eq!(process.root, process.owner);
        assert!(matches!(
            process.activation,
            ProcessActivation::Reactive { ref sensitivity }
                if matches!(sensitivity.as_slice(), [ProcessSensitivity::Signal(_)])
        ));
        assert!(
            design
                .process_ir
                .values
                .iter()
                .all(|value| value.bit_width.is_some()),
            "normalized hardware values must carry backend-ready widths: {:#?}",
            design.process_ir.values
        );
        assert!(design
            .process_ir
            .validate(design.signals.len() as u32)
            .is_empty());
    }

    #[test]
    /// An event is one Boolean regardless of the observed signal's packed
    /// width; treating it as the signal width would inflate direct operations.
    fn event_process_values_are_one_bit() {
        let span = crate::diag::Span::new(FileId(0), 0..0);
        let process_ir = ProcessIr {
            values: vec![ProcessValue {
                span,
                ty: None,
                bit_width: None,
                kind: ProcessValueKind::Signal {
                    signals: vec![SignalId(0)],
                    state: ProcessSignalState::Event,
                },
            }],
            ..ProcessIr::default()
        };
        assert_eq!(
            normalized_value_width(&process_ir, ProcessValueId(0), &Design::default()),
            Some(1)
        );
    }

    #[test]
    /// Constant arithmetic in a shift count must contribute to the shifted
    /// value's natural width. Signed-vector std code builds its fill mask as
    /// `1 << (width - 1)`, so losing this width changes runtime behavior.
    fn normalized_shift_width_folds_integer_expression() {
        let span = crate::diag::Span::new(FileId(0), 0..0);
        let mut process_ir = ProcessIr::default();
        let expression = crate::ir::Expr::Binary {
            op: crate::ir::BinOp::Shl,
            lhs: Box::new(crate::ir::Expr::Const(1)),
            rhs: Box::new(crate::ir::Expr::Binary {
                op: crate::ir::BinOp::Sub,
                lhs: Box::new(crate::ir::Expr::Const(8)),
                rhs: Box::new(crate::ir::Expr::Const(1)),
            }),
        };

        let shifted = push_normalized_value(&mut process_ir, &expression, span, &Design::default());

        assert_eq!(process_ir.values[shifted.0 as usize].bit_width, Some(8));
    }

    #[test]
    /// Resolver-selected constants become their executable initializer graph;
    /// a backend must never need the frontend declaration behind a `DefId`.
    fn module_constants_do_not_survive_as_definitions() {
        let sources = [
            "module tests; const EXPECTED: integer = 3; \
             fn choose(a: integer, b: integer) -> integer { \
               let left: integer = a; \
               if left > b { return left; } return b; \
             } \
             #[std::attrs::test] entity Smoke {} \
             impl Smoke { let observed: integer = 0; \
               process run { observed = choose(observed, EXPECTED); } }",
            "module std::logic; pub enum Bool { false, true }",
            "module std::attrs; using std::logic::{Bool}; pub attr test: Bool for entity;",
            "module std::ops; using std::logic::{Bool}; \
             pub trait Boolean { fn as_bool(self) -> Bool; } \
             impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } }",
            "module std::prelude; pub using std::logic::{Bool}; \
             pub using std::attrs::{test}; pub using std::ops::{Boolean};",
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

        lower(
            &modules,
            &resolved,
            &typed,
            &hierarchy,
            Some(&plan),
            &mut design,
        );

        assert!(design.process_ir.values.iter().any(|value| matches!(
            value.kind,
            ProcessValueKind::Number(ProcessNumber::Integer(ref words))
                if words.as_slice() == [3]
        )));
        assert!(design
            .process_ir
            .values
            .iter()
            .all(|value| !matches!(value.kind, ProcessValueKind::Definition(_))));
        assert!(design
            .process_ir
            .values
            .iter()
            .any(|value| matches!(value.kind, ProcessValueKind::Select { .. })));
        assert!(design
            .process_ir
            .values
            .iter()
            .all(|value| !matches!(value.kind, ProcessValueKind::Call { .. })));
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
                    root: crate::elab::InstanceId(1),
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
                .any(|issue| issue.contains("assigned to another root")),
            "{issues:?}"
        );
        assert!(
            issues.iter().any(|issue| issue.contains("invalid value")),
            "{issues:?}"
        );
    }
}
