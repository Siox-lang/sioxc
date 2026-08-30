//! `siox` (silicon oxide) — a digital hardware description language and
//! simulator. This library target contains the whole compiler pipeline through
//! IR lowering and the native LLVM backend. Frontend consumers can use the
//! pipeline modules without invoking the backend.
//!
//! **The pipeline is a strict top-to-bottom stack** (each stage uses only the
//! stages above it, plus [`diag`] which everything uses). The crate boundaries
//! that used to enforce this are gone — the layering is now a **convention**,
//! kept by module discipline; do not introduce upward or sideways `use`s.
//!
//! | module    | stage | role |
//! | --------- | ----- | ---- |
//! | [`diag`]    | 0 | `Span`, `SourceMap`, `Diagnostic`, the error/warning code catalogue |
//! | [`syntax`]  | 1–2 | lexer, tokens, AST, parser, pretty-printer |
//! | [`resolve`] | 3 | name resolution, `using` imports, visibility, `DefId`s |
//! | [`types`]   | 4 | type & kind checking; Phase-2 syntax rejection |
//! | [`elab`]    | 5 | elaboration: parameter substitution, instance hierarchy |
//! | [`ir`]      | 6 | lowering to the digital simulation IR |
//!
//! [`compiler`] is the presentation-neutral embedding boundary that composes
//! those stages for editors, build tools, and `sioxc`. The native LLVM AOT
//! backend is available as `siox::llvm` when the `llvm` feature is enabled.

extern crate self as siox;

pub mod compiler;
pub mod diag;
pub mod elab;
pub mod ir;
#[cfg(feature = "llvm")]
pub mod llvm;
pub mod resolve;
pub mod syntax;
pub mod test_ir;
pub mod testbench;
pub mod types;
