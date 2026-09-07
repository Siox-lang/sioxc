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
//! | [`ir`]      | 6 | canonical process/control, value, layout, and digital simulation IR |
//! | [`test_ir`] | adapter | temporary normalized-hardware/test-AST lowering into `ir::Design::process_ir` |
//!
//! [`compiler`] is the presentation-neutral embedding boundary that composes
//! those stages for editors, build tools, and `sioxc`. The native LLVM AOT
//! backend is available as `siox::llvm` when the `llvm` feature is enabled.
//!
//! ```mermaid
//! flowchart TD
//!     src["source .siox"] --> syntax
//!     syntax["syntax<br/>lex, parse"] --> resolve
//!     resolve["resolve<br/>names, visibility"] --> types
//!     types["types<br/>type & kind check"] --> elab
//!     elab["elab<br/>instances, parameters"] --> ir
//!     ir["ir<br/>digital simulation IR"] --> tb
//!     ir --> emit
//!     tb["testbench + test_ir<br/>process descriptors"] --> build
//!     emit["llvm::emit<br/>design to LLVM IR"] --> aot
//!     aot["llvm::aot"] --> obj["native object"]
//!     build["driver::build<br/>generated C + libfst"] --> exe["test executable<br/>VCD / FST"]
//!     diag["diag: spans, diagnostics"] -.-> syntax
//!     diag -.-> resolve
//!     diag -.-> types
//!     diag -.-> elab
//!     diag -.-> ir
//!     compiler["compiler: embedding boundary,<br/>owns orchestration"] === src
//! ```
//!
//! # Building the documentation
//!
//! ```text
//! cargo doc --no-deps --open
//! ```
//!
//! The ```` ```mermaid ```` blocks throughout these docs are rendered by
//! `docs/rustdoc-header.html`, which `.cargo/config.toml` passes to rustdoc as
//! `--html-in-header`. Diagrams fall back to their source text when the
//! mermaid CDN is unreachable, so the pages stay readable offline. Use
//! `cargo doc --document-private-items` to include the internal helpers that
//! make up most of each stage.

// Every public item carries documentation. CI runs clippy with `-D warnings`,
// so this is a hard gate there while staying a warning locally.
#![warn(missing_docs)]

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
