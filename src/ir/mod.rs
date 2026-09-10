//! Unified execution and digital simulation IR for siox Phase 1 (spec Stage 6).
//!
//! [`Design::process_ir`] owns independently scheduled process CFGs, locals,
//! suspension, activation, and test descriptors. During the compatibility
//! migration, ordinary hardware behavior is also normalized into explicit
//! event dependencies, combinational drivers, and sequential next-state
//! updates; those `Driver`/`EventBlock` forms will become derived process
//! optimizations. `::event` and `::old` are explicit IR operations.
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
use crate::syntax::ast::{self, BinOp as AstBinOp, UnOp as AstUnOp};
use crate::syntax::Module;

pub(crate) mod design;
pub(crate) mod expr;
mod functions;
pub(crate) mod layout;
mod lower;
mod lower_helpers;
mod passes;
pub(crate) mod process;
pub(crate) mod query;

pub use design::*;
pub use expr::*;
pub use functions::FunctionIndex;
pub use layout::*;
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

type OperatorImpls<'a> = HashMap<(String, String), Vec<(&'a ast::FnDecl, Option<String>)>>;
type NumericRangeInfo = (u32, bool, Option<(i64, i64)>);

#[cfg(test)]
mod tests;
