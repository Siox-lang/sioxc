//! LLVM code generation for siox (compiled-backend plan, stage B2).
//!
//! Consumes the process-extracted [`crate::ir::Design`] and builds an LLVM
//! module: three word-width state arrays (`cur`/`old`/`event`), the
//! `sx_set`/`sx_read`/`sx_reset` accessors, and a `sx_settle` that evaluates
//! the combinational processes in dependency order. Sequential (event-block)
//! codegen and the full delta-cycle fixpoint are the next increment.
//!
//! LLVM is the permanent backend — building siox needs an LLVM toolchain (see
//! `Cargo.toml` for the pinned version). Values are 64-bit words masked to each
//! signal's width.

mod aot;
mod emit;
mod jit;

pub use aot::emit_object;
pub use emit::emit_module_ir;
pub use jit::{with_jit, Jit};
