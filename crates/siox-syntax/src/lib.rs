//! Lexer, parser, AST and pretty-printer for siox Phase 1.
//!
//! Spec: `docs/language.md` Stage 1 (syntax freeze) and Stage 2
//! (lexer/parser). The AST must be able to represent every item listed under
//! "AST should represent" in Stage 2.

pub mod token;
pub mod lexer;
pub mod ast;
pub mod parser;
pub mod pretty;

pub use ast::Module;

/// Parse a single source file into a [`Module`] AST.
///
/// Diagnostics (lex/parse errors, recovery notes) are pushed into `sink`.
/// Returns a best-effort AST even on error so later stages can keep going.
pub fn parse_module(
    file: siox_diag::FileId,
    src: &str,
    sink: &mut siox_diag::DiagnosticSink,
) -> Module {
    let tokens = lexer::Lexer::new(file, src).tokenize(sink);
    let operators = parser::discover_custom_operators(src, &tokens);
    parser::Parser::new(src, tokens, sink)
        .with_custom_operators(&operators)
        .parse_module()
}

/// Parse with a precomputed custom textual-operator table.
pub fn parse_module_with_operators(
    file: siox_diag::FileId,
    src: &str,
    operators: &std::collections::HashMap<String, u8>,
    sink: &mut siox_diag::DiagnosticSink,
) -> Module {
    let tokens = lexer::Lexer::new(file, src).tokenize(sink);
    parser::Parser::new(src, tokens, sink)
        .with_custom_operators(operators)
        .parse_module()
}
