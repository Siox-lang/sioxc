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

/// The radix prefixes the compiler knows how to evaluate, and how many bits
/// one of their digits is worth.
///
/// std owns which prefixes *exist* (`impl Prefix<"x", string> for unsigned`),
/// but evaluating one is a compiler intrinsic until const string ops exist, so
/// the alphabet and the digit width live here. This is the single source of
/// truth for both: the radix is `1 << bits`, so a digit is well-formed exactly
/// when `char::to_digit` accepts it at that radix.
///
/// It used to be spelled out at six sites — two in the emitter, two in `ir`,
/// the pattern parser, and `bit_pattern_mask` below — in four different
/// shapes, and the pattern parser's copy had already drifted: it matched
/// `"x" | "o"` where expression position accepts any letter and lets type
/// checking reject it, so a prefix std declared and the compiler did not
/// support reported a clean diagnostic in one position and a raw parse error
/// in the other.
pub const RADIX_PREFIXES: &[(char, u32)] = &[('x', 4), ('o', 3)];

/// Bits per digit for `base`; 1 (binary, one character per bit) for a plain
/// string and for any prefix that is not a radix.
pub fn bits_per_digit(base: char) -> u32 {
    RADIX_PREFIXES
        .iter()
        .find(|(prefix, _)| *prefix == base)
        .map_or(1, |(_, bits)| *bits)
}

/// The numeric radix `base` reads its digits in: 16, 8, or 2.
pub fn radix_of(base: char) -> u32 {
    1 << bits_per_digit(base)
}

/// Whether `base` is a radix prefix the compiler can evaluate — false for the
/// plain (unprefixed) string, which is per-bit rather than per-digit.
pub fn is_radix_prefix(base: char) -> bool {
    RADIX_PREFIXES.iter().any(|(prefix, _)| *prefix == base)
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
    // A bare string is per-bit; anything else must be a radix prefix the
    // compiler can evaluate. One letter, so `chars` is the whole test.
    let mut letters = base.chars();
    let per: u32 = match (letters.next(), letters.next()) {
        (None, _) => 1, // bare string, per-bit
        (Some(c), None) if is_radix_prefix(c) => bits_per_digit(c),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The prefix table is the single source of truth for the alphabet and the
    /// digit width, and the two derived accessors have to stay consistent with
    /// it — every consumer reads one or the other. Six sites used to spell the
    /// mapping out independently.
    #[test]
    fn every_radix_prefix_decodes_through_the_table() {
        for &(prefix, bits) in RADIX_PREFIXES {
            assert!(is_radix_prefix(prefix), "`{prefix}` is in the table");
            assert_eq!(bits_per_digit(prefix), bits);
            assert_eq!(radix_of(prefix), 1 << bits, "radix is 2^bits");

            // A digit of this radix decodes; the first digit past it does not.
            let last = char::from_digit(radix_of(prefix) - 1, radix_of(prefix)).unwrap();
            assert!(
                bit_pattern_mask(&format!("{prefix}\"{last}\"")).is_some(),
                "`{prefix}\"{last}\"` is a well-formed pattern"
            );
            // One character past the radix — `8` for octal, `g` for hex.
            let past = "0123456789abcdefg"
                .chars()
                .nth(radix_of(prefix) as usize)
                .unwrap();
            assert_eq!(
                bit_pattern_mask(&format!("{prefix}\"{past}\"")),
                None,
                "`{prefix}\"{past}\"` is one digit past the radix"
            );

            // `?` masks one whole group, so the mask is `bits` zero bits wide.
            let (mask, value) = bit_pattern_mask(&format!("{prefix}\"?\"")).unwrap();
            assert_eq!((mask, value), (vec![0], vec![0]), "`?` is a don't-care");
        }
        // A bare string is per-bit, and is not itself a radix prefix.
        assert_eq!(bits_per_digit('b'), 1, "no prefix means one bit per char");
        assert!(!is_radix_prefix('b'));
        assert_eq!(
            bit_pattern_mask("\"01--\""),
            Some((vec![0b1100], vec![0b0100]))
        );
        // A letter std may declare but the compiler cannot evaluate.
        assert_eq!(bit_pattern_mask("d\"42\""), None);
    }
}
