//! Entity specialization and elaboration for siox Phase 1 (spec Stage 5).
//!
//! Turns parameterized entities and instances into a concrete elaborated
//! hierarchy: parameter substitution, instance creation, port connection
//! resolution (including `.clk` shorthand), nested hierarchy, external entity
//! stubs, bus-mode expansion to leaf permissions, direction checking, and
//! constant-expression evaluation for parameters.
//!
//! Acceptance (spec Stage 5): all entity parameters known after elaboration;
//! all required ports connected or defaulted; direction violations reported;
//! bus modes expand to leaf permissions; external entities are black boxes;
//! the hierarchy can be printed as a tree (`siox tree`).

use siox_diag::DiagnosticSink;
use siox_syntax::Module;
use siox_types::Typed;

/// A concrete elaborated design: a tree of instances with resolved parameters
/// and connections.
#[derive(Default)]
pub struct Hierarchy {
    // TODO(stage-5): instance tree, per-instance parameter bindings,
    // connection map, leaf direction table.
}

impl Hierarchy {
    /// Render the instance tree (backs `siox tree`).
    pub fn to_tree_string(&self) -> String {
        // TODO(stage-5): pretty-print the elaborated hierarchy.
        String::new()
    }
}

/// Elaborate starting from the `#[top]` entity (or a named root).
pub fn elaborate(_modules: &[Module], _typed: &Typed, _sink: &mut DiagnosticSink) -> Hierarchy {
    // TODO(stage-5): find top, substitute params, build instance graph.
    todo!("Stage 5: elaboration")
}
