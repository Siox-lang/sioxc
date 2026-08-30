//! Backend-neutral software control IR for native test entities.
//!
//! Digital behavior stays in [`crate::ir::Design`]. This module owns the
//! procedural side of a native test: test descriptors, local storage,
//! assignments, runtime/scheduler calls, and control-flow placeholders. The
//! generated-C compatibility backend consumes the descriptors now; subsequent
//! slices will replace deferred control nodes with full basic-block lowering
//! and emit these functions through LLVM.

use std::collections::HashSet;

use crate::diag::Span;
use crate::elab::{Hierarchy, InstanceId};
use crate::ir::{Design, SourceLayout};
use crate::resolve::{DefId, DefKind, Resolved};
use crate::syntax::ast::{self, ImplItem, Stmt};
use crate::syntax::Module;
use crate::testbench::TestPlan;
use crate::types::{Ty, Typed};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

/// Software-side product paired with the ordinary digital [`Design`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Program {
    pub tests: Vec<TestFunction>,
}

/// One zero-argument test function and its runtime descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestFunction {
    pub entity: DefId,
    pub root: InstanceId,
    pub qualified_name: String,
    pub span: Span,
    pub entry: BlockId,
    pub locals: Vec<Local>,
    pub blocks: Vec<BasicBlock>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Local {
    pub id: LocalId,
    pub name: String,
    pub span: Span,
    pub ty: Option<Ty>,
    pub layout: Option<SourceLayout>,
    pub persistent: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BasicBlock {
    pub id: BlockId,
    pub instructions: Vec<Instruction>,
    pub terminator: Terminator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instruction {
    Declare {
        local: LocalId,
        initializer: Option<ValueRef>,
        span: Span,
    },
    Assign {
        target: ValueRef,
        value: ValueRef,
        delay: Option<ValueRef>,
        span: Span,
    },
    Runtime {
        operation: RuntimeOp,
        arguments: Vec<ValueRef>,
        span: Span,
    },
    /// A typed, source-anchored control node awaiting CFG expansion. Keeping
    /// this explicit prevents an incomplete emitter from silently dropping it.
    DeferredControl { kind: DeferredControl, span: Span },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeOp {
    Assert,
    Warn,
    Print,
    Await,
    Stop,
    Finish,
    Call(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeferredControl {
    If,
    Match,
    For,
    Return,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Terminator {
    Return,
    Goto(BlockId),
    Branch {
        condition: ValueRef,
        then_block: BlockId,
        else_block: BlockId,
    },
}

/// A source expression with the type checker result retained beside it.
/// Expression semantics are deliberately not encoded as text; `text` is only
/// for IR rendering while later lowering replaces it with SSA values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueRef {
    pub span: Span,
    pub ty: Option<Ty>,
    pub text: String,
}

/// Build the initial software control product from the same resolved plan and
/// concrete layouts used by the compatibility backend.
pub fn lower(
    modules: &[Module],
    resolved: &Resolved,
    typed: &Typed,
    hierarchy: &Hierarchy,
    plan: &TestPlan,
    design: &Design,
) -> Program {
    let tests = plan
        .tests
        .iter()
        .map(|test| {
            let root_path = hierarchy.root_path(test.root);
            let mut function = TestFunction {
                entity: test.entity,
                root: test.root,
                qualified_name: test.qualified_name.clone(),
                span: test.span,
                entry: BlockId(0),
                locals: Vec::new(),
                blocks: vec![BasicBlock {
                    id: BlockId(0),
                    instructions: Vec::new(),
                    terminator: Terminator::Return,
                }],
            };

            for item in crate::testbench::implementation_items(modules, resolved, test.entity) {
                match item {
                    ImplItem::Let(declaration) if !is_entity_instance(declaration, resolved) => {
                        let local = push_local(
                            &mut function,
                            declaration,
                            true,
                            design
                                .source_layouts
                                .get(&format!("{root_path}.{}", declaration.name.text))
                                .cloned(),
                            typed,
                        );
                        function.blocks[0].instructions.push(Instruction::Declare {
                            local,
                            initializer: declaration
                                .value
                                .as_ref()
                                .map(|value| value_ref(value, typed)),
                            span: declaration.span,
                        });
                    }
                    ImplItem::Process(process) => {
                        for statement in &process.body.stmts {
                            lower_statement(statement, typed, &mut function);
                        }
                    }
                    ImplItem::Stmt(statement) => lower_statement(statement, typed, &mut function),
                    ImplItem::Const(_)
                    | ImplItem::Fn(_)
                    | ImplItem::ModeField { .. }
                    | ImplItem::Let(_) => {}
                }
            }
            function
        })
        .collect();
    Program { tests }
}

fn push_local(
    function: &mut TestFunction,
    declaration: &ast::LetDecl,
    persistent: bool,
    layout: Option<SourceLayout>,
    typed: &Typed,
) -> LocalId {
    let id = LocalId(function.locals.len() as u32);
    function.locals.push(Local {
        id,
        name: declaration.name.text.clone(),
        span: declaration.span,
        ty: declaration
            .value
            .as_ref()
            .and_then(|value| typed.expr_type(ast::expr_span(value)))
            .cloned(),
        layout,
        persistent,
    });
    id
}

fn lower_statement(statement: &Stmt, typed: &Typed, function: &mut TestFunction) {
    let instructions = &mut function.blocks[0].instructions;
    match statement {
        Stmt::Let(declaration) => {
            let local = push_local(function, declaration, false, None, typed);
            function.blocks[0].instructions.push(Instruction::Declare {
                local,
                initializer: declaration
                    .value
                    .as_ref()
                    .map(|value| value_ref(value, typed)),
                span: declaration.span,
            });
        }
        Stmt::Assign {
            target,
            value,
            after,
            span,
        } => instructions.push(Instruction::Assign {
            target: value_ref(target, typed),
            value: value_ref(value, typed),
            delay: after.as_ref().map(|delay| value_ref(delay, typed)),
            span: *span,
        }),
        Stmt::Expr(ast::Expr::Call {
            callee, args, span, ..
        }) => {
            let name = callee_name(callee);
            let operation = match name.as_str() {
                "assert" => RuntimeOp::Assert,
                "warn" => RuntimeOp::Warn,
                "print" => RuntimeOp::Print,
                "await" => RuntimeOp::Await,
                "stop" => RuntimeOp::Stop,
                "finish" => RuntimeOp::Finish,
                _ => RuntimeOp::Call(name),
            };
            instructions.push(Instruction::Runtime {
                operation,
                arguments: args
                    .iter()
                    .map(|argument| value_ref(argument, typed))
                    .collect(),
                span: *span,
            });
        }
        Stmt::Expr(expression) => instructions.push(Instruction::Runtime {
            operation: RuntimeOp::Call("<expression>".to_string()),
            arguments: vec![value_ref(expression, typed)],
            span: ast::expr_span(expression),
        }),
        Stmt::If(statement) => instructions.push(Instruction::DeferredControl {
            kind: DeferredControl::If,
            span: statement.span,
        }),
        Stmt::Match(statement) => instructions.push(Instruction::DeferredControl {
            kind: DeferredControl::Match,
            span: statement.span,
        }),
        Stmt::For { span, .. } => instructions.push(Instruction::DeferredControl {
            kind: DeferredControl::For,
            span: *span,
        }),
        Stmt::Return { span, .. } => instructions.push(Instruction::DeferredControl {
            kind: DeferredControl::Return,
            span: *span,
        }),
    }
}

fn value_ref(expression: &ast::Expr, typed: &Typed) -> ValueRef {
    let span = ast::expr_span(expression);
    ValueRef {
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

fn is_entity_instance(declaration: &ast::LetDecl, resolved: &Resolved) -> bool {
    let ty = match &declaration.value {
        Some(ast::Expr::Construct { ty: Some(ty), .. }) => Some(ty),
        _ => declaration.ty.as_ref(),
    };
    ty.and_then(|ty| type_def_id(ty, resolved))
        .is_some_and(|id| resolved.kind_of(id) == Some(DefKind::Entity))
}

fn type_def_id(ty: &ast::Type, resolved: &Resolved) -> Option<DefId> {
    match ty {
        ast::Type::Path(path) => resolved.resolved(path.span),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => {
            type_def_id(base, resolved)
        }
        ast::Type::View { view, .. } => resolved.resolved(view.span),
    }
}

impl Program {
    /// Structural invariants required before either backend consumes this IR.
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();
        let mut names = HashSet::new();
        let mut roots = HashSet::new();
        for test in &self.tests {
            if !names.insert(test.qualified_name.clone()) {
                issues.push(format!(
                    "duplicate test descriptor `{}`",
                    test.qualified_name
                ));
            }
            if !roots.insert(test.root) {
                issues.push(format!("test root {:?} is used more than once", test.root));
            }
            if test.blocks.get(test.entry.0 as usize).map(|block| block.id) != Some(test.entry) {
                issues.push(format!(
                    "test `{}` has an invalid entry block",
                    test.qualified_name
                ));
            }
            for (index, local) in test.locals.iter().enumerate() {
                if local.id != LocalId(index as u32) {
                    issues.push(format!(
                        "test `{}` has a non-dense local id",
                        test.qualified_name
                    ));
                }
            }
            for (index, block) in test.blocks.iter().enumerate() {
                if block.id != BlockId(index as u32) {
                    issues.push(format!(
                        "test `{}` has a non-dense block id",
                        test.qualified_name
                    ));
                }
                for instruction in &block.instructions {
                    if let Instruction::Declare { local, .. } = instruction {
                        if test.locals.get(local.0 as usize).map(|value| value.id) != Some(*local) {
                            issues.push(format!(
                                "test `{}` references an invalid local {:?}",
                                test.qualified_name, local
                            ));
                        }
                    }
                }
                for target in terminator_targets(&block.terminator) {
                    if test.blocks.get(target.0 as usize).map(|value| value.id) != Some(target) {
                        issues.push(format!(
                            "test `{}` branches to an invalid block {:?}",
                            test.qualified_name, target
                        ));
                    }
                }
            }
        }
        issues
    }

    pub fn to_ir_string(&self) -> String {
        let mut output = String::new();
        for test in &self.tests {
            output.push_str(&format!(
                "test @{} root {} {{\n",
                test.qualified_name, test.root.0
            ));
            for local in &test.locals {
                output.push_str(&format!(
                    "  local %{} {}{}\n",
                    local.id.0,
                    local.name,
                    if local.persistent { " persistent" } else { "" }
                ));
            }
            for block in &test.blocks {
                output.push_str(&format!("  bb{}:\n", block.id.0));
                for instruction in &block.instructions {
                    output.push_str(&format!("    {instruction:?}\n"));
                }
                output.push_str(&format!("    {:?}\n", block.terminator));
            }
            output.push_str("}\n");
        }
        output
    }
}

fn terminator_targets(terminator: &Terminator) -> Vec<BlockId> {
    match terminator {
        Terminator::Return => Vec::new(),
        Terminator::Goto(target) => vec![*target],
        Terminator::Branch {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{DiagnosticSink, FileId};

    #[test]
    fn lowers_descriptors_locals_assignments_and_runtime_calls() {
        let sources = [
            "module tests;\n\
             #[std::attrs::test] entity Smoke {}\n\
             impl Smoke {\n\
               let flag: Bool = true;\n\
               flag = false;\n\
               await true;\n\
               assert!(flag == false, \"done\");\n\
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
        let design = crate::ir::lower(&modules, &resolved, &hierarchy, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());

        let program = lower(&modules, &resolved, &typed, &hierarchy, &plan, &design);
        assert!(program.validate().is_empty(), "{:#?}", program.validate());
        assert_eq!(program.tests.len(), 1);
        let test = &program.tests[0];
        assert_eq!(test.qualified_name, "tests::Smoke");
        assert_eq!(test.locals.len(), 1);
        assert!(test.locals[0].persistent);
        assert!(test.locals[0].layout.is_some());
        assert!(test.blocks[0]
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, Instruction::Assign { .. })));
        assert!(test.blocks[0].instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Runtime {
                    operation: RuntimeOp::Await,
                    ..
                }
            )
        }));
        assert!(test.blocks[0].instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Runtime {
                    operation: RuntimeOp::Assert,
                    ..
                }
            )
        }));
        assert!(program.to_ir_string().contains("test @tests::Smoke root 0"));
    }

    #[test]
    fn validator_rejects_an_invalid_entry_block() {
        let program = Program {
            tests: vec![TestFunction {
                entity: DefId(0),
                root: InstanceId(0),
                qualified_name: "tests::Broken".to_string(),
                span: Span::new(FileId(0), 0..1),
                entry: BlockId(1),
                locals: Vec::new(),
                blocks: vec![BasicBlock {
                    id: BlockId(0),
                    instructions: Vec::new(),
                    terminator: Terminator::Return,
                }],
            }],
        };
        assert!(program
            .validate()
            .iter()
            .any(|issue| issue.contains("invalid entry block")));
    }
}
