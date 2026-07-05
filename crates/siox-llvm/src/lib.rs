//! LLVM code generation for siox (compiled-backend plan, stage B2).
//!
//! Consumes the process-extracted [`siox_ir::Design`] and builds an LLVM
//! module: three word-width state arrays (`cur`/`old`/`event`), the
//! `sx_set`/`sx_read`/`sx_reset` accessors, and a `sx_settle` that evaluates
//! the combinational processes in dependency order. Sequential (event-block)
//! codegen and the full delta-cycle fixpoint are the next increment.
//!
//! Everything LLVM-facing is behind the `llvm` cargo feature so the default
//! workspace build — and the interpreter that acts as the differential
//! oracle — never needs an LLVM toolchain. Values are 64-bit words masked to
//! each signal's width, matching the interpreter so results are bit-identical.

//! Without the `llvm` feature this crate is empty, so the default workspace
//! build (and the interpreter oracle) compiles with no LLVM toolchain. Build
//! the emitter with `--features llvm`.

#[cfg(feature = "llvm")]
mod emit;

#[cfg(feature = "llvm")]
pub use emit::emit_module_ir;
