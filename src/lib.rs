//! `siox` (silicon oxide) — a digital hardware description language and
//! simulator, exposed as one library so both the compiler CLI (`sioxc`) and the
//! language server (`siox-lsp`) share the same pipeline.
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
//! | [`run`]     | 7–8 | simulation kernel / `#[test]` runner (engine-agnostic) |
//! | [`llvm`]    | 7 | LLVM JIT + native AOT backend (behind the `llvm` feature) |
//! | [`wave`]    | 9 | `Trace` recording + VCD export |

pub mod diag;
pub mod syntax;
pub mod resolve;
pub mod types;
pub mod elab;
pub mod ir;
pub mod run;
pub mod wave;

// The LLVM backend is optional: `--no-default-features` yields a frontend-only
// library (no inkwell / LLVM toolchain), which `siox-lsp` builds against.
#[cfg(feature = "llvm")]
pub mod llvm;
