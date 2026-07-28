//! Token kinds for the siox lexer.
//!
//! Spec Stage 1 freezes the surface syntax. The keyword and punctuation sets
//! below are the Phase 1 lexical vocabulary; analogue keywords (`domain`,
//! `across`, `through`) are intentionally absent and must be lexed as plain
//! identifiers so the type checker can reject them with a Phase-2 diagnostic
//! (spec Stage 10: "Use of Phase 2-only analogue syntax").

use crate::diag::Span;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenKind {
    // Literals & names
    Ident,
    Int,          // 42, 0xFF, 0b1010 (numeric suffixes like 100n lex as a trailing ident)
    Float,        // 1000.0  (the `f`-style suffix lexes as a trailing ident, like Int)
    CharacterLit, // a single character in single quotes: '0' '1' 'Z' 'X', 'a', '!'
    StrLit,       // "work"  (prefixed strings like x"05AB" lex as Ident + StrLit)

    // Keywords (Phase 1)
    Module,
    Using,
    Pub,
    Entity,
    Impl,
    Struct,
    View,
    Enum,
    Trait,
    Attr,
    Const,
    Let, // signal / state / local binding: `let x: T = e;`
    Fn,  // function / method declaration: `fn name(self) { ... }`
    In,
    Out,
    Inout,
    If,
    Else,
    Match,
    For,
    Return,
    Extern,
    SelfKw, // self (method receiver + `self'event`, spec 3.9/3.20); `true`/`false` stay idents (enum)

    // Punctuation
    LParen,     // (
    RParen,     // )
    LBrace,     // {
    RBrace,     // }
    LBracket,   // [
    RBracket,   // ]
    Lt,         // <
    Gt,         // >
    ColonColon, // ::
    Colon,      // :
    Semi,       // ;
    Comma,      // ,
    Dot,        // .
    DotDot,     // ..  (ranges, spec 3.23)
    Eq,         // =   (single operator, spec 3.12)
    EqEq,       // ==
    FatArrow,   // =>  (match arms)
    Arrow,      // ->  (return type; NOTE: analogue path use is Phase 2)
    Amp,        // &
    Pipe,       // |
    Plus,
    Minus,
    Star,
    Slash,
    PlusEq,   // +=
    MinusEq,  // -=
    StarEq,   // *=
    SlashEq,  // /=
    AmpEq,    // &=
    PipeEq,   // |=
    Shl,      // <<
    Shr,      // >>
    Bang,     // ! (assert!)
    BangEq,   // !=
    LtEq,     // <=
    GtEq,     // >=
    CustomOp, // user-defined punctuation operator, e.g. %% or ^^
    Pound,    // # (attribute application `#[...]`, spec 3.5/3.6)
    Tick,     // ' (VHDL-style attribute accessor `sig'event`, spec 3.9); a
    //   `'c'`-shaped run stays a CharacterLit — see the lexer.

    // Trivia / control
    Comment,
    Eof,
    /// Lexer error recovery token.
    Unknown,
}

impl TokenKind {
    /// How this kind should be named in a diagnostic. Punctuation and keywords
    /// render as the source spelling a user actually types (`` `;` ``), never
    /// the Rust variant name — "expected Semi" means nothing to a reader.
    pub fn describe(&self) -> &'static str {
        match self {
            // Abstract kinds read as prose; everything else is literal syntax.
            TokenKind::Ident => "an identifier",
            TokenKind::Int => "an integer literal",
            TokenKind::Float => "a float literal",
            TokenKind::CharacterLit => "a character literal",
            TokenKind::StrLit => "a string literal",
            TokenKind::CustomOp => "an operator",
            TokenKind::Comment => "a comment",
            TokenKind::Eof => "end of input",
            TokenKind::Unknown => "an unrecognized token",

            TokenKind::Module => "`module`",
            TokenKind::Using => "`using`",
            TokenKind::Pub => "`pub`",
            TokenKind::Entity => "`entity`",
            TokenKind::Impl => "`impl`",
            TokenKind::Struct => "`struct`",
            TokenKind::View => "`view`",
            TokenKind::Enum => "`enum`",
            TokenKind::Trait => "`trait`",
            TokenKind::Attr => "`attr`",
            TokenKind::Const => "`const`",
            TokenKind::Let => "`let`",
            TokenKind::Fn => "`fn`",
            TokenKind::In => "`in`",
            TokenKind::Out => "`out`",
            TokenKind::Inout => "`inout`",
            TokenKind::If => "`if`",
            TokenKind::Else => "`else`",
            TokenKind::Match => "`match`",
            TokenKind::For => "`for`",
            TokenKind::Return => "`return`",
            TokenKind::Extern => "`extern`",
            TokenKind::SelfKw => "`self`",

            TokenKind::LParen => "`(`",
            TokenKind::RParen => "`)`",
            TokenKind::LBrace => "`{`",
            TokenKind::RBrace => "`}`",
            TokenKind::LBracket => "`[`",
            TokenKind::RBracket => "`]`",
            TokenKind::Lt => "`<`",
            TokenKind::Gt => "`>`",
            TokenKind::ColonColon => "`::`",
            TokenKind::Colon => "`:`",
            TokenKind::Semi => "`;`",
            TokenKind::Comma => "`,`",
            TokenKind::Dot => "`.`",
            TokenKind::DotDot => "`..`",
            TokenKind::Eq => "`=`",
            TokenKind::EqEq => "`==`",
            TokenKind::FatArrow => "`=>`",
            TokenKind::Arrow => "`->`",
            TokenKind::Amp => "`&`",
            TokenKind::Pipe => "`|`",
            TokenKind::Plus => "`+`",
            TokenKind::Minus => "`-`",
            TokenKind::Star => "`*`",
            TokenKind::Slash => "`/`",
            TokenKind::PlusEq => "`+=`",
            TokenKind::MinusEq => "`-=`",
            TokenKind::StarEq => "`*=`",
            TokenKind::SlashEq => "`/=`",
            TokenKind::AmpEq => "`&=`",
            TokenKind::PipeEq => "`|=`",
            TokenKind::Shl => "`<<`",
            TokenKind::Shr => "`>>`",
            TokenKind::Bang => "`!`",
            TokenKind::BangEq => "`!=`",
            TokenKind::LtEq => "`<=`",
            TokenKind::GtEq => "`>=`",
            TokenKind::Pound => "`#`",
            TokenKind::Tick => "`'`",
        }
    }
}
