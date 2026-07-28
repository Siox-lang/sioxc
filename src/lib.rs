//! `siox` (silicon oxide) — a digital hardware description language and
//! simulator. This library target contains the whole compiler pipeline through
//! IR lowering, the native LLVM backend, shared testbench definitions, and
//! waveform export. Frontend consumers can use the pipeline modules without
//! invoking the backend.
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
//! | [`testbench`] | 7–8 | shared `#[test]` language runtime (engine-agnostic) |
//! | [`wave`]    | 9 | `Trace` recording + VCD export |
//!
//! The native LLVM AOT backend is the [`llvm`] module.

extern crate self as siox;

pub mod diag;
pub mod elab;
pub mod ir;
#[cfg(feature = "llvm")]
pub mod llvm;
pub mod resolve;
pub mod syntax;
pub mod target;
pub mod testbench;
pub mod types;
pub mod wave;
