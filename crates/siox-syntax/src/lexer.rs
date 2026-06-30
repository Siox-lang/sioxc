//! Hand-written lexer: source text -> [`Token`] stream.
//!
//! Spec Stage 2 work items: tokenization, source spans, error recovery.
//!
//! Lexical decisions pinned here (Stage 2):
//! - Comments: `// line` and `/* nested block */`, emitted as [`TokenKind::Comment`]
//!   trivia so later round-tripping can keep them; the parser skips trivia.
//! - Logic literals: `'0' '1' 'Z' 'X'` — a single character between single
//!   quotes. The lexer accepts any one-character form and leaves value
//!   validation to the type checker.
//! - Numbers: decimal, `0x` hex, `0b` binary integers, and decimal floats
//!   (`1000.0`). Numeric suffixes (`100n`, `1000.0f`) are intentionally *not*
//!   consumed here — they lex as a following identifier and are the parser's
//!   problem (see token.rs note). A `.` only starts a fraction when a digit
//!   follows, so range syntax like `31..0` is never mistaken for a float.
//! - Prefixed strings like `b"01??"` (bit pattern) and `x"05AB"` (hex value)
//!   are *not* special tokens: they lex as an identifier glued to a string
//!   (`Ident` + `StrLit`). The prefix is interpreted later as a string overload
//!   (a library mechanism), so the lexer stays language-neutral here.
//! - Analogue keywords (`domain`, `across`, `through`) are deliberately not
//!   recognised, so they lex as plain identifiers for a later Phase-2 rejection.

use crate::token::{Token, TokenKind};
use siox_diag::{Diagnostic, DiagnosticSink, FileId, Span};

pub struct Lexer<'a> {
    file: FileId,
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(file: FileId, src: &'a str) -> Self {
        Lexer { file, src, bytes: src.as_bytes(), pos: 0 }
    }

    /// Lex the whole input into a token vector terminated by `Eof`.
    ///
    /// Lex/recovery errors are pushed into `sink`; the stream is always
    /// well-formed and `Eof`-terminated so the parser can keep going.
    pub fn tokenize(&mut self, sink: &mut DiagnosticSink) -> Vec<Token> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace();
            let start = self.pos;
            let Some(c) = self.peek() else {
                tokens.push(Token { kind: TokenKind::Eof, span: self.span(start, start) });
                break;
            };

            let kind = match c {
                b'/' if self.peek_at(1) == Some(b'/') => self.line_comment(),
                b'/' if self.peek_at(1) == Some(b'*') => self.block_comment(sink, start),
                c if is_ident_start(c) => self.ident_or_keyword(),
                c if c.is_ascii_digit() => self.number(sink, start),
                b'\'' => self.logic_literal(sink, start),
                b'"' => {
                    self.string_body(sink, start);
                    TokenKind::StrLit
                }
                _ => self.punctuation(sink, start),
            };

            tokens.push(Token { kind, span: self.span(start, self.pos) });
        }
        tokens
    }

    // --- scanners -----------------------------------------------------------

    fn line_comment(&mut self) -> TokenKind {
        // Consume through end of line, leaving the newline for skip_whitespace.
        while let Some(c) = self.peek() {
            if c == b'\n' {
                break;
            }
            self.bump();
        }
        TokenKind::Comment
    }

    fn block_comment(&mut self, sink: &mut DiagnosticSink, start: usize) -> TokenKind {
        self.pos += 2; // `/*`
        let mut depth = 1;
        while depth > 0 {
            match self.peek() {
                None => {
                    sink.emit(
                        Diagnostic::error("unterminated block comment")
                            .at(self.span(start, self.pos)),
                    );
                    break;
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    self.pos += 2;
                    depth += 1;
                }
                Some(b'*') if self.peek_at(1) == Some(b'/') => {
                    self.pos += 2;
                    depth -= 1;
                }
                _ => self.pos += 1,
            }
        }
        TokenKind::Comment
    }

    fn ident_or_keyword(&mut self) -> TokenKind {
        let start = self.pos;
        while self.peek().is_some_and(is_ident_continue) {
            self.bump();
        }
        keyword_kind(&self.src[start..self.pos]).unwrap_or(TokenKind::Ident)
    }

    fn number(&mut self, sink: &mut DiagnosticSink, start: usize) -> TokenKind {
        // Optional `0x` / `0b` radix prefix, otherwise plain decimal. Only
        // decimal numbers can carry a fractional part.
        let mut is_decimal = true;
        let radix_digit: fn(u8) -> bool = if self.peek() == Some(b'0')
            && matches!(self.peek_at(1), Some(b'x' | b'X'))
        {
            self.pos += 2;
            is_decimal = false;
            is_hex_digit
        } else if self.peek() == Some(b'0') && matches!(self.peek_at(1), Some(b'b' | b'B')) {
            self.pos += 2;
            is_decimal = false;
            is_bin_digit
        } else {
            is_dec_digit
        };

        let digits_start = self.pos;
        while self.peek().is_some_and(radix_digit) {
            self.bump();
        }
        if self.pos == digits_start && self.pos > start + 1 {
            // A prefix (`0x` / `0b`) with no digits behind it.
            sink.emit(
                Diagnostic::error("expected digits after numeric prefix")
                    .at(self.span(start, self.pos)),
            );
        }

        // A `.` followed by a digit is a fractional part and makes this a float.
        // Requiring the trailing digit keeps range syntax (`31..0`) as integers.
        if is_decimal && self.peek() == Some(b'.') && self.peek_at(1).is_some_and(is_dec_digit) {
            self.bump(); // `.`
            while self.peek().is_some_and(is_dec_digit) {
                self.bump();
            }
            return TokenKind::Float;
        }
        TokenKind::Int
    }

    fn logic_literal(&mut self, sink: &mut DiagnosticSink, start: usize) -> TokenKind {
        self.bump(); // opening `'`
        let mut chars = 0;
        while let Some(c) = self.peek() {
            if c == b'\'' || c == b'\n' {
                break;
            }
            self.bump();
            chars += 1;
        }
        if self.peek() == Some(b'\'') {
            self.bump(); // closing `'`
            if chars == 1 {
                return TokenKind::LogicLit;
            }
            sink.emit(
                Diagnostic::error("logic literal must contain exactly one character")
                    .at(self.span(start, self.pos)),
            );
        } else {
            sink.emit(
                Diagnostic::error("unterminated logic literal")
                    .at(self.span(start, self.pos)),
            );
        }
        TokenKind::Unknown
    }

    /// Scan a `"..."` body starting at the opening quote. Used for both string
    /// and bit-pattern literals; the caller has already consumed any prefix.
    fn string_body(&mut self, sink: &mut DiagnosticSink, start: usize) {
        self.bump(); // opening `"`
        loop {
            match self.peek() {
                None | Some(b'\n') => {
                    sink.emit(
                        Diagnostic::error("unterminated string literal")
                            .at(self.span(start, self.pos)),
                    );
                    return;
                }
                Some(b'"') => {
                    self.bump();
                    return;
                }
                Some(b'\\') => {
                    self.bump(); // backslash
                    self.bump(); // escaped byte (if any)
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    fn punctuation(&mut self, sink: &mut DiagnosticSink, start: usize) -> TokenKind {
        let two = self.peek_at(1);
        // Two-character operators first.
        let two_char = match (self.peek(), two) {
            (Some(b':'), Some(b':')) => Some(TokenKind::ColonColon),
            (Some(b'.'), Some(b'.')) => Some(TokenKind::DotDot),
            (Some(b'='), Some(b'=')) => Some(TokenKind::EqEq),
            (Some(b'='), Some(b'>')) => Some(TokenKind::FatArrow),
            (Some(b'-'), Some(b'>')) => Some(TokenKind::Arrow),
            (Some(b'<'), Some(b'<')) => Some(TokenKind::Shl),
            (Some(b'>'), Some(b'>')) => Some(TokenKind::Shr),
            _ => None,
        };
        if let Some(kind) = two_char {
            self.pos += 2;
            return kind;
        }

        let one = self.bump().unwrap();
        match one {
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b'{' => TokenKind::LBrace,
            b'}' => TokenKind::RBrace,
            b'[' => TokenKind::LBracket,
            b']' => TokenKind::RBracket,
            b'<' => TokenKind::Lt,
            b'>' => TokenKind::Gt,
            b':' => TokenKind::Colon,
            b';' => TokenKind::Semi,
            b',' => TokenKind::Comma,
            b'.' => TokenKind::Dot,
            b'=' => TokenKind::Eq,
            b'&' => TokenKind::Amp,
            b'|' => TokenKind::Pipe,
            b'+' => TokenKind::Plus,
            b'-' => TokenKind::Minus,
            b'*' => TokenKind::Star,
            b'/' => TokenKind::Slash,
            b'!' => TokenKind::Bang,
            b'#' => TokenKind::Pound,
            _ => {
                sink.emit(
                    Diagnostic::error(format!("unexpected character `{}`", one as char))
                        .at(self.span(start, self.pos)),
                );
                TokenKind::Unknown
            }
        }
    }

    // --- cursor helpers -----------------------------------------------------

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            self.bump();
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<u8> {
        self.bytes.get(self.pos + n).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.file, start as u32..end as u32)
    }
}

fn is_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

fn is_ident_continue(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}

fn is_dec_digit(c: u8) -> bool {
    c.is_ascii_digit()
}

fn is_hex_digit(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

fn is_bin_digit(c: u8) -> bool {
    c == b'0' || c == b'1'
}

fn keyword_kind(s: &str) -> Option<TokenKind> {
    Some(match s {
        "module" => TokenKind::Module,
        "using" => TokenKind::Using,
        "pub" => TokenKind::Pub,
        "entity" => TokenKind::Entity,
        "impl" => TokenKind::Impl,
        "struct" => TokenKind::Struct,
        "enum" => TokenKind::Enum,
        "trait" => TokenKind::Trait,
        "attr" => TokenKind::Attr,
        "const" => TokenKind::Const,
        "let" => TokenKind::Let,
        "fn" => TokenKind::Fn,
        "in" => TokenKind::In,
        "out" => TokenKind::Out,
        "inout" => TokenKind::Inout,
        "if" => TokenKind::If,
        "else" => TokenKind::Else,
        "match" => TokenKind::Match,
        "for" => TokenKind::For,
        "return" => TokenKind::Return,
        "extern" => TokenKind::Extern,
        "self" => TokenKind::SelfKw,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::TokenKind::*;

    fn lex(src: &str) -> (Vec<TokenKind>, usize) {
        let mut sink = DiagnosticSink::new();
        let kinds: Vec<TokenKind> = Lexer::new(FileId(0), src)
            .tokenize(&mut sink)
            .into_iter()
            .map(|t| t.kind)
            .collect();
        (kinds, sink.error_count())
    }

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).0
    }

    #[test]
    fn empty_input_is_just_eof() {
        assert_eq!(kinds(""), vec![Eof]);
        assert_eq!(kinds("   \n\t "), vec![Eof]);
    }

    #[test]
    fn keywords_vs_identifiers() {
        assert_eq!(kinds("entity impl"), vec![Entity, Impl, Eof]);
        // Analogue keywords are not recognised in Phase 1: plain idents.
        assert_eq!(kinds("domain across through"), vec![Ident, Ident, Ident, Eof]);
        // `self` is a keyword; `true`/`false` are enum idents, not keywords.
        assert_eq!(kinds("self::event"), vec![SelfKw, ColonColon, Ident, Eof]);
        assert_eq!(kinds("true false"), vec![Ident, Ident, Eof]);
    }

    #[test]
    fn spans_cover_the_token_text() {
        let mut sink = DiagnosticSink::new();
        let toks = Lexer::new(FileId(0), "entity Counter").tokenize(&mut sink);
        assert_eq!(toks[0].span.start, 0);
        assert_eq!(toks[0].span.end, 6);
        assert_eq!(toks[1].span.start, 7);
        assert_eq!(toks[1].span.end, 14);
    }

    #[test]
    fn numbers_decimal_hex_binary() {
        assert_eq!(kinds("42 0xFF 0b1010"), vec![Int, Int, Int, Eof]);
        // A suffix is a separate identifier token, not part of the number.
        assert_eq!(kinds("100n"), vec![Int, Ident, Eof]);
    }

    #[test]
    fn floats_and_ranges_are_distinguished() {
        assert_eq!(kinds("1000.0"), vec![Float, Eof]);
        // The `f`-style suffix is a separate identifier, mirroring int suffixes.
        assert_eq!(kinds("1000.0f"), vec![Float, Ident, Eof]);
        // A double dot is a range, never a float.
        assert_eq!(kinds("31..0"), vec![Int, DotDot, Int, Eof]);
        // A trailing dot with no digit is not a fraction.
        assert_eq!(kinds("1.foo"), vec![Int, Dot, Ident, Eof]);
    }

    #[test]
    fn logic_and_string_literals() {
        assert_eq!(kinds("'0' '1' 'Z' 'X'"), vec![LogicLit, LogicLit, LogicLit, LogicLit, Eof]);
        assert_eq!(kinds("\"work\""), vec![StrLit, Eof]);
        // Prefixed strings are not special tokens: an ident glued to a string.
        // The prefix becomes a string overload later. `?` lives inside the
        // string body, so it is never a standalone token.
        assert_eq!(kinds("b\"01??\""), vec![Ident, StrLit, Eof]);
        assert_eq!(kinds("x\"05AB\""), vec![Ident, StrLit, Eof]);
    }

    #[test]
    fn multi_char_punctuation() {
        assert_eq!(
            kinds(":: .. == => -> << >>"),
            vec![ColonColon, DotDot, EqEq, FatArrow, Arrow, Shl, Shr, Eof],
        );
        // `::` must win over two `:` tokens.
        assert_eq!(kinds("State::Idle"), vec![Ident, ColonColon, Ident, Eof]);
    }

    #[test]
    fn attribute_application_pound_bracket() {
        assert_eq!(kinds("#[top]"), vec![Pound, LBracket, Ident, RBracket, Eof]);
    }

    #[test]
    fn comments_are_trivia_tokens() {
        assert_eq!(kinds("a // tail\nb"), vec![Ident, Comment, Ident, Eof]);
        assert_eq!(kinds("a /* x /* nested */ y */ b"), vec![Ident, Comment, Ident, Eof]);
    }

    #[test]
    fn assignment_line_lexes_cleanly() {
        let (ks, errors) = lex("let clk: Logic = '0';");
        assert_eq!(errors, 0);
        assert_eq!(ks, vec![Let, Ident, Colon, Ident, Eq, LogicLit, Semi, Eof]);
    }

    #[test]
    fn error_recovery_reports_and_continues() {
        // Unterminated string: one error, stream still ends in Eof.
        let (ks, errors) = lex("\"oops");
        assert_eq!(errors, 1);
        assert_eq!(ks.last(), Some(&Eof));

        // Stray backtick is unknown but does not stop lexing.
        let (ks, errors) = lex("a ` b");
        assert_eq!(errors, 1);
        assert_eq!(ks, vec![Ident, Unknown, Ident, Eof]);
    }
}
