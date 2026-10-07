//! Unified execution and digital simulation IR for siox Phase 1 (spec Stage 6).
//!
//! [`Design::process_ir`] owns independently scheduled process CFGs, locals,
//! suspension, activation, and test descriptors. Source hardware lowers into
//! this same canonical product before scheduler decomposition. Explicit event
//! dependencies, combinational drivers, and sequential next-state updates in
//! the `Driver`/`EventBlock` view are derived compatibility optimizations.
//! `'event` and `'old` are explicit IR operations.
//!
//! Current compatibility forms:
//! ```text
//! Driver(signal, expression, condition)              // combinational
//! OnEvent(event_condition): next(signal) = expression // sequential
//! ```
//! and `Rising(clk)` lowers to
//! `Event(clk) && Old(clk) == '0' && Current(clk) == '1'`.
//!
//! The IR data types are deliberately **language-neutral** — they use their own
//! `BinOp`/`UnOp` and never reference the siox AST — so that other HDL frontends
//! could target the same IR. Only `lower` (the siox frontend lowering) consumes
//! the siox AST.
//!
//! Phase-1 scope: lowers the behaviour of each non-extern entity in the design,
//! with the entity's declared (possibly parametric) widths. Per-instance width
//! specialization and cross-instance flattening/connection are follow-ups.

use std::collections::{HashMap, HashSet};

use crate::diag::DiagnosticSink;
use crate::elab::Hierarchy;
use crate::resolve::{DefId, Resolved};
use crate::syntax::ast::{self, BinOp as AstBinOp};
use crate::syntax::Module;

mod derive;
pub(crate) mod design;
pub(crate) mod expr;
mod functions;
pub(crate) mod layout;
mod lower;
mod lower_helpers;
mod passes;
pub(crate) mod process;
pub(crate) mod query;

pub(crate) use derive::derive_scheduler_forms;
pub use design::*;
pub use expr::*;
pub use functions::FunctionIndex;
pub use layout::*;
pub(crate) use lower::lower_processes;
use lower::AccessStep;
pub use lower::{lower, lower_in};
use lower_helpers::*;
pub use lower_helpers::{
    array_families, derived_widths, enum_discriminants, enum_first_discriminants, eval_const_fns,
    eval_const_stmts, loop_range, subst_expr_paths, subst_stmt_paths,
};
pub use passes::call_fn_key;
use passes::*;
pub use process::*;
#[cfg(test)]
use query::render;
pub use query::{read_set, IndexSite, Process, ProcessKind};

/// `(operator trait, implementing type)` to the `fn` declarations that
/// implement it, each paired with the impl's declared right-operand type
/// (`None` reads as `Self`). Overload selection matches on that type.
#[derive(Default)]
struct OperatorImpls<'a>(HashMap<String, HashMap<String, OperatorCandidates<'a>>>);

/// The implementations of one operator or hook trait for one owner type.
type OperatorCandidates<'a> = Vec<(&'a ast::FnDecl, Option<String>)>;

impl<'a> OperatorImpls<'a> {
    /// The implementations of `name` for `owner`. Keyed trait/operator name
    /// first, then owner, so a lookup borrows both instead of building a
    /// `(String, String)` key.
    fn get(&self, name: &str, owner: &str) -> Option<&OperatorCandidates<'a>> {
        self.0.get(name)?.get(owner)
    }

    fn contains(&self, name: &str, owner: &str) -> bool {
        self.get(name, owner).is_some()
    }

    fn entry(&mut self, name: &str, owner: String) -> &mut OperatorCandidates<'a> {
        self.0
            .entry(name.to_owned())
            .or_default()
            .entry(owner)
            .or_default()
    }

    /// Every `(name, owner, implementations)`.
    fn iter(&self) -> impl Iterator<Item = (&str, &str, &OperatorCandidates<'a>)> {
        self.0.iter().flat_map(|(name, owners)| {
            owners
                .iter()
                .map(move |(owner, functions)| (name.as_str(), owner.as_str(), functions))
        })
    }
}
/// A value-range-constrained numeric type, as
/// `(storage width, is_real, declared bounds)`.
type NumericRangeInfo = (u32, bool, Option<(i64, i64)>);

#[cfg(test)]
mod tests;
