//! Token kinds for the siox lexer.
//!
//! Spec Stage 1 freezes the surface syntax. The keyword and punctuation sets
//! below are the Phase 1 lexical vocabulary; analogue keywords (`domain`,
//! `across`, `through`) are intentionally absent and must be lexed as plain
//! identifiers so the type checker can reject them with a Phase-2 diagnostic
//! (spec Stage 10: "Use of Phase 2-only analogue syntax").

use siox_diag::Span;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenKind {
    // Literals & names
    Ident,
    Int,        // 42, 0xFF, 0b1010 (numeric suffixes like 100n lex as a trailing ident)
    Float,      // 1000.0  (the `f`-style suffix lexes as a trailing ident, like Int)
    LogicLit,   // '0' '1' 'Z' 'X'
    StrLit,     // "work"
    BitPatLit,  // b"01??"  prefixed-string overload, pattern context only (spec 3.22)
    HexStrLit,  // x"05AB"  prefixed-string overload for a hex value literal

    // Keywords (Phase 1)
    Module,
    Using,
    Pub,
    Entity,
    Impl,
    Struct,
    Enum,
    Trait,
    Attr,
    Const,
    Let,
    Fn,        // reserved; methods use `let name(self)` per spec examples
    In,
    Out,
    Inout,
    If,
    Else,
    Match,
    For,
    Return,
    Extern,
    SelfKw,    // self (method receiver + `self::event`, spec 3.9/3.20); `true`/`false` stay idents (enum)

    // Punctuation
    LParen,    // (
    RParen,    // )
    LBrace,    // {
    RBrace,    // }
    LBracket,  // [
    RBracket,  // ]
    Lt,        // <
    Gt,        // >
    ColonColon, // ::
    Colon,     // :
    Semi,      // ;
    Comma,     // ,
    Dot,       // .
    DotDot,    // ..  (ranges, spec 3.23)
    Eq,        // =   (single operator, spec 3.12)
    EqEq,      // ==
    FatArrow,  // =>  (match arms)
    Arrow,     // ->  (return type; NOTE: analogue path use is Phase 2)
    Amp,       // &
    Pipe,      // |
    Plus,
    Minus,
    Star,
    Slash,
    Shl,       // <<
    Shr,       // >>
    Question,  // ? (wildcard inside bit patterns only)
    Bang,      // ! (assert!)
    Pound,     // # (attribute application `#[...]`, spec 3.5/3.6)

    // Trivia / control
    Comment,
    Eof,
    /// Lexer error recovery token.
    Unknown,
}
