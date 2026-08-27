//! Shared native-test discovery and hierarchy planning.
//!
//! This module is the boundary between the ordinary resolved/type-checked
//! frontend and either native test backend. Attribute discovery happens once;
//! the generated-C compatibility harness and the future software-IR/LLVM
//! lowering consume the same [`TestPlan`].

use std::collections::{HashMap, HashSet};

use crate::diag::{DiagnosticSink, Span};
use crate::elab::{Hierarchy, InstanceId};
use crate::resolve::{DefId, Resolved};
use crate::syntax::ast::Item;
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
}
