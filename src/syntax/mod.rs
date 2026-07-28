//! Lexer, parser, AST and pretty-printer for siox Phase 1.
//!
//! Spec: `docs/language.md` Stage 1 (syntax freeze) and Stage 2
//! (lexer/parser). The AST must be able to represent every item listed under
//! "AST should represent" in Stage 2.

pub mod ast;
pub mod format;
pub mod lexer;
pub mod parser;
pub mod pretty;
pub mod token;

pub use ast::Module;

/// Parse a single source file into a [`Module`] AST.
///
/// Diagnostics (lex/parse errors, recovery notes) are pushed into `sink`.
/// Returns a best-effort AST even on error so later stages can keep going.
pub fn parse_module(
    file: crate::diag::FileId,
    src: &str,
    sink: &mut crate::diag::DiagnosticSink,
) -> Module {
    let tokens = lexer::Lexer::new(file, src).tokenize(sink);
    let operators = parser::discover_custom_operators(src, &tokens);
    parser::Parser::new(src, tokens, sink)
        .with_custom_operators(&operators)
        .parse_module()
}

/// Parse with a precomputed custom textual-operator table.
pub fn parse_module_with_operators(
    file: crate::diag::FileId,
    src: &str,
    operators: &std::collections::HashMap<String, u8>,
    sink: &mut crate::diag::DiagnosticSink,
) -> Module {
    let tokens = lexer::Lexer::new(file, src).tokenize(sink);
    parser::Parser::new(src, tokens, sink)
        .with_custom_operators(operators)
        .parse_module()
}

/// Decode a bit-pattern literal (`"1-1-"` / `x"A?"`, spec 3.22) into a
/// `(mask, value)` pair: an input matches when `input & mask == value`.
/// A bare string (empty prefix) is per-bit, using `-` (the `std_ulogic`
/// don't-care) as the wildcard; a radix prefix (`x`/`o`) uses `?` to mask its
/// whole group (nibble/triad). `_` separators are ignored. `None` when the
/// text isn't a well-formed pattern. Words are low-word first.
pub fn bit_pattern_mask(text: &str) -> Option<(Vec<u64>, Vec<u64>)> {
    let (base, digits) = match text.split_once('"') {
        Some((b, rest)) => (b, rest.trim_end_matches('"')),
        None => return None,
    };
    let per: u32 = match base {
        "" => 1, // bare string, per-bit
        "o" => 3,
        "x" => 4,
        _ => return None,
    };
    let radix = 1u32 << per; // 2, 8, or 16
    let mut mask = Vec::new();
    let mut value = Vec::new();
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let (m, v) = match c {
            '?' | '-' => (0, 0), // don't-care: `-` in bare strings, `?` in radix groups
            _ => {
                let d = c.to_digit(radix)? as u64;
                (((1u64 << per) - 1), d)
            }
        };
        push_group(&mut mask, per, m);
        push_group(&mut value, per, v);
    }
    Some((mask, value))
}

fn push_group(words: &mut Vec<u64>, bits: u32, value: u64) {
    let mut carry = value;
    for word in words.iter_mut() {
        let next = *word >> (64 - bits);
        *word = (*word << bits) | carry;
        carry = next;
    }
    if carry != 0 || words.is_empty() {
        words.push(carry);
    }
}
