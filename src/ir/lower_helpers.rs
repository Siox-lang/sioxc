//! Shared helpers used by the Siox-to-IR lowering stage.
//!
//! Helpers are grouped by source-level responsibility while this facade keeps
//! their established visibility within `ir`.

use super::*;

mod builders;
mod generate;
mod source;
mod substitute;
mod types;

pub(super) use builders::*;
pub(super) use generate::*;
pub(super) use source::*;
pub(super) use substitute::*;
pub(super) use types::*;

pub use builders::{eval_const_fns, eval_const_stmts};
pub use substitute::{loop_range, subst_expr_paths, subst_stmt_paths};
pub use types::{array_families, derived_widths, enum_discriminants, enum_first_discriminants};
