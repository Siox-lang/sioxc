//! Shared native-test discovery and hierarchy planning.
//!
//! This module is the boundary between the ordinary resolved/type-checked
//! frontend and either native test backend. Attribute discovery happens once;
//! the generated-C compatibility harness and the future software-IR/LLVM
//! lowering consume the same [`TestPlan`].

use std::collections::{HashMap, HashSet};

use crate::diag::{codes, Diagnostic, DiagnosticSink, Span};
use crate::elab::{Hierarchy, InstanceId};
use crate::resolve::{DefId, Resolved};
use crate::syntax::ast::Item;
use crate::syntax::ast::{self, Expr, ImplItem, Stmt, Type, UnOp};
use crate::syntax::Module;
use crate::types::Typed;

/// One test entity after discovery and hierarchy elaboration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestCase {
    /// Stable identity of the entity declaration.
    pub entity: DefId,
    /// Root of this test's independently elaborated hierarchy.
    pub root: InstanceId,
    /// Module-qualified name used by native filtering and reports.
    pub qualified_name: String,
    /// Entity declaration span for selection diagnostics and tooling.
    pub span: Span,
}

/// The single native-test input shared by every backend.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestPlan {
    pub tests: Vec<TestCase>,
}

impl TestPlan {
    pub fn is_empty(&self) -> bool {
        self.tests.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tests.len()
    }
}

#[derive(Clone, Debug)]
struct DiscoveredTest {
    entity: DefId,
    qualified_name: String,
    span: Span,
}

/// Discover the canonical std tests, elaborate exactly their hierarchies, and
/// bind those roots into the backend-neutral plan.
pub fn elaborate(
    modules: &[Module],
    resolved: &Resolved,
    typed: &Typed,
    sink: &mut DiagnosticSink,
) -> (Hierarchy, TestPlan) {
    let discovered = discover(modules, resolved);
    validate_process_scheduling(modules, resolved, &discovered, sink);
    let selected: HashSet<DefId> = discovered.iter().map(|test| test.entity).collect();
    let hierarchy = crate::elab::elaborate_entities(modules, resolved, typed, sink, &selected);

    let by_entity: HashMap<DefId, &DiscoveredTest> =
        discovered.iter().map(|test| (test.entity, test)).collect();
    let tests = hierarchy
        .roots
        .iter()
        .filter_map(|&root| {
            let entity = hierarchy.instance(root).entity_id;
            let test = by_entity.get(&entity)?;
            Some(TestCase {
                entity,
                root,
                qualified_name: test.qualified_name.clone(),
                span: test.span,
            })
        })
        .collect();

    (hierarchy, TestPlan { tests })
}

/// The compatibility runtime supports any number of canonical self-toggle
/// clock processes and one foreground stimulus process. General coroutine
/// scheduling belongs to the software-IR backend; reject it until that backend
/// can preserve concurrency instead of serializing source processes.
fn validate_process_scheduling(
    modules: &[Module],
    resolved: &Resolved,
    tests: &[DiscoveredTest],
    sink: &mut DiagnosticSink,
) {
    for test in tests {
        let items = implementation_items(modules, resolved, test.entity);
        let mut foreground: Vec<(Span, String)> = Vec::new();
        let mut legacy_site = None;

        for item in items {
            match item {
                ImplItem::Process(process) if !is_clock_process(&process.body.stmts) => {
                    let label = process
                        .name
                        .as_ref()
                        .map(|name| format!("process `{}`", name.text))
                        .unwrap_or_else(|| "anonymous process".to_string());
                    foreground.push((process.span, label));
                }
                ImplItem::Stmt(statement) if !is_clock_statement(statement) => {
                    legacy_site.get_or_insert(ast::stmt_span(statement));
                }
                _ => {}
            }
        }
        if let Some(span) = legacy_site {
            foreground.push((span, "legacy impl-scope stimulus".to_string()));
        }

        let Some((first_span, first_label)) = foreground.first().cloned() else {
            continue;
        };
        for (span, label) in foreground.into_iter().skip(1) {
            sink.emit(
                Diagnostic::error(format!(
                    "native test `{}` has more than one foreground process",
                    test.qualified_name
                ))
                .with_code(codes::TEST_PROCESS_SCHEDULING)
                .at(span)
                .label(first_span, format!("first foreground {first_label} is here"))
                .help(format!(
                    "merge {label} into the first stimulus process; self-toggle clock processes may remain separate"
                )),
            );
        }
    }
}

pub(crate) fn is_clock_process(statements: &[Stmt]) -> bool {
    matches!(statements, [statement] if is_clock_statement(statement))
}

pub(crate) fn is_clock_statement(statement: &Stmt) -> bool {
    let Stmt::Assign {
        target,
        value: Expr::Unary {
            op: UnOp::Not, rhs, ..
        },
        after: Some(_),
        ..
    } = statement
    else {
        return false;
    };
    crate::syntax::pretty::expr_string(target) == crate::syntax::pretty::expr_string(rhs)
}

fn discover(modules: &[Module], resolved: &Resolved) -> Vec<DiscoveredTest> {
    modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| {
            let Item::Entity(entity) = item else {
                return None;
            };
            if !entity
                .attrs
                .iter()
                .any(|attribute| crate::resolve::is_enabled_std_test_attribute(resolved, attribute))
            {
                return None;
            }
            let id = resolved.declared(entity.name.span)?;
            Some(DiscoveredTest {
                entity: id,
                qualified_name: resolved
                    .qualified_name(id)
                    .unwrap_or_else(|| entity.name.text.clone()),
                span: entity.span,
            })
        })
        .collect()
}

/// Inherent implementation items belonging to one resolved entity.
///
/// Both the compatibility backend and software-IR lowering enter a test body
/// through this identity-based lookup. Keeping it beside [`TestPlan`] avoids a
/// second owner/name matching rule at each backend boundary.
pub fn implementation_items<'a>(
    modules: &'a [Module],
    resolved: &Resolved,
    entity: DefId,
) -> Vec<&'a ImplItem> {
    modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            Item::Impl(implementation)
                if implementation.trait_.is_none()
                    && type_def_id(&implementation.target, resolved) == Some(entity) =>
            {
                Some(implementation.items.iter())
            }
            _ => None,
        })
        .flatten()
        .collect()
}

fn type_def_id(ty: &Type, resolved: &Resolved) -> Option<DefId> {
    match ty {
        Type::Path(path) => resolved.resolved(path.span),
        Type::Generic { base, .. } | Type::Indexed { base, .. } => type_def_id(base, resolved),
        Type::View { view, .. } => resolved.resolved(view.span),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{DiagnosticSink, FileId};

    fn modules(source: &str) -> (Vec<Module>, DiagnosticSink) {
        let std_logic = "module std::logic; pub enum Bool { false, true }";
        let std_attrs =
            "module std::attrs; using std::logic::{Bool}; pub attr test: Bool for entity;";
        let prelude =
            "module std::prelude; pub using std::logic::{Bool}; pub using std::attrs::{test};";
        let mut sink = DiagnosticSink::new();
        let modules = [source, std_logic, std_attrs, prelude]
            .iter()
            .enumerate()
            .map(|(index, source)| {
                crate::syntax::parse_module(FileId(index as u32), source, &mut sink)
            })
            .collect();
        (modules, sink)
    }

    #[test]
    fn discovery_uses_the_canonical_std_attribute_and_its_value() {
        let source = "module tests;\n\
            pub attr test: Bool for entity;\n\
            #[std::attrs::test] entity Enabled {}\n\
            #[std::attrs::test = false] entity Disabled {}\n\
            #[tests::test] entity Custom {}\n";
        let (modules, mut sink) = modules(source);
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());

        let (hierarchy, plan) = elaborate(&modules, &resolved, &typed, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());
        assert_eq!(plan.len(), 1);
        assert_eq!(plan.tests[0].qualified_name, "tests::Enabled");
        assert_eq!(hierarchy.roots, vec![plan.tests[0].root]);
    }

    #[test]
    fn native_tests_reject_multiple_foreground_processes_but_allow_clocks() {
        let diagnostics = |source: &str| {
            let (modules, mut sink) = modules(source);
            let resolved = crate::resolve::resolve(&modules, &mut sink);
            let tests = discover(&modules, &resolved);
            validate_process_scheduling(&modules, &resolved, &tests, &mut sink);
            sink.diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.code == Some(codes::TEST_PROCESS_SCHEDULING))
                .count()
        };

        assert_eq!(
            diagnostics(
                "module tests;\n#[std::attrs::test] entity T {}\n\
                 impl T { process first {} process second {} }\n"
            ),
            1,
            "the compatibility backend must not serialize concurrent stimulus"
        );
        assert_eq!(
            diagnostics(
                "module tests;\n#[std::attrs::test] entity T {}\n\
                 impl T {\n\
                   let clk: Bool = false;\n\
                   process clock { clk = not clk after 1; }\n\
                   process stimulus {}\n\
                 }\n"
            ),
            0,
            "a canonical background clock and one foreground process are supported"
        );
        assert_eq!(
            diagnostics(
                "module tests;\n#[std::attrs::test] entity T {}\n\
                 impl T { process stimulus {} print!(\"legacy\"); }\n"
            ),
            1,
            "legacy impl-scope stimulus is one foreground sequence too"
        );
    }
}
