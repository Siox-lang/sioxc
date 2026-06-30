//! Name resolution and module system for siox Phase 1 (spec Stage 3).
//!
//! Resolves every identifier to a declaration: module namespace tree, `using`
//! imports, type aliases, public/private visibility, `::` path resolution,
//! associated items (`State::Idle`), trait names, impl targets, entity
//! instance type names, and attribute names.
//!
//! Acceptance (spec Stage 3):
//! - unknown names reported ([`siox_diag::codes::UNKNOWN_NAME`])
//! - ambiguous imports reported
//! - private items inaccessible from outside their module
//! - attribute usage fails if the attribute was not declared/imported
//! - associated paths like `State::Idle` resolve correctly

use siox_diag::DiagnosticSink;
use siox_syntax::Module;

/// Stable id for a resolved declaration. Later stages key off this instead of
/// raw names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DefId(pub u32);

/// The result of resolving a set of modules: a namespace tree plus a map from
/// every name-use site to the [`DefId`] it refers to.
#[derive(Default)]
pub struct Resolved {
    // TODO(stage-3): namespace tree, def table, use-site -> DefId map,
    // visibility table.
}

/// Resolve a crate's worth of parsed modules.
pub fn resolve(_modules: &[Module], _sink: &mut DiagnosticSink) -> Resolved {
    // TODO(stage-3): build module tree, process `using`, resolve paths.
    todo!("Stage 3: name resolution")
}
