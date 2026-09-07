//! Token kinds for the siox lexer.
//!
//! Spec Stage 1 freezes the surface syntax. The keyword and punctuation sets
//! below are the Phase 1 lexical vocabulary; analogue keywords (`domain`,
//! `across`, `through`) are intentionally absent and must be lexed as plain
//! identifiers so the type checker can reject them with a Phase-2 diagnostic
//! (spec Stage 10: "Use of Phase 2-only analogue syntax").

use crate::diag::Span;

/// One lexed token: what it is, and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    /// Which lexical form this token is.
    pub kind: TokenKind,
    /// The token's extent in the source file, for diagnostics.
    pub span: Span,
}

/// Every lexical form the siox lexer produces.
///
/// Analogue keywords (`domain`, `across`, `through`) are deliberately absent:
/// they lex as plain identifiers so the type checker can reject them with a
/// Phase-2 diagnostic rather than the lexer failing first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenKind {
    // Literals & names
    /// An identifier or a keyword-shaped word that is not a keyword.
    Ident,
    /// An integer literal such as `42`, `0xFF`, or `0b1010`. A numeric suffix
    /// like the `n` in `100n` lexes separately as a trailing identifier.
    Int,
    /// A float literal such as `1000.0`. Suffixes lex separately, as for [`TokenKind::Int`].
    Float,
    /// A single character in single quotes: `'0'`, `'Z'`, `'a'`, `'!'`.
    CharacterLit,
    /// A double-quoted string. A radix-prefixed string such as `x"05AB"` lexes
    /// as an identifier followed by this.
    StrLit,

    // Keywords (Phase 1)
    /// The `module` keyword.
    Module,
    /// The `using` keyword.
    Using,
    /// The `pub` visibility keyword.
    Pub,
    /// The `entity` keyword.
    Entity,
    /// The `impl` keyword.
    Impl,
    /// The `struct` keyword.
    Struct,
    /// The `view` keyword.
    View,
    /// The `enum` keyword.
    Enum,
    /// The `trait` keyword.
    Trait,
    /// The `attr` keyword, which declares a user attribute.
    Attr,
    /// The `const` keyword.
    Const,
    /// The `let` keyword: signal, state, or local binding (`let x: T = e;`).
    Let,
    /// The `fn` keyword: function or method declaration.
    Fn,
    /// The `process` keyword.
    Process,
    /// The `in` port direction, also the separator in `for i in range`.
    In,
    /// The `out` port direction.
    Out,
    /// The `inout` port direction.
    Inout,
    /// The `if` keyword.
    If,
    /// The `else` keyword.
    Else,
    /// The `match` keyword.
    Match,
    /// The `for` keyword.
    For,
    /// The `return` keyword.
    Return,
    /// The `extern` keyword.
    Extern,
    /// The `self` receiver, also the base of `self'event`. `true`/`false` are
    /// not keywords — they stay identifiers, being enum variants.
    SelfKw,

    // Punctuation
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `<` — comparison, and the opening of a generic argument list.
    Lt,
    /// `>` — comparison, and the closing of a generic argument list.
    Gt,
    /// `::` — the type and module sigil.
    ColonColon,
    /// `:` — type ascription and trait bounds.
    Colon,
    /// `;`
    Semi,
    /// `,`
    Comma,
    /// `.` — the value sigil: field access and method calls.
    Dot,
    /// `..` — range construction.
    DotDot,
    /// `=` — the single assignment operator; siox has no `<=`/`:=` split.
    Eq,
    /// `==`
    EqEq,
    /// `=>` — separates a match arm's pattern from its body.
    FatArrow,
    /// `->` — return type. Analogue path use of this token is Phase 2.
    Arrow,
    /// `&`
    Amp,
    /// `|` — also the pattern alternative separator.
    Pipe,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `+=`
    PlusEq,
    /// `-=`
    MinusEq,
    /// `*=`
    StarEq,
    /// `/=`
    SlashEq,
    /// `&=`
    AmpEq,
    /// `|=`
    PipeEq,
    /// `<<`
    Shl,
    /// `>>`
    Shr,
    /// `!` — the macro-shaped call marker, as in `assert!`.
    Bang,
    /// `!=`
    BangEq,
    /// `<=`
    LtEq,
    /// `>=`
    GtEq,
    /// A user-defined punctuation operator such as `%%` or `^^`.
    CustomOp,
    /// `#` — introduces an attribute application `#[...]`.
    Pound,
    /// `'` — the VHDL-style attribute accessor in `sig'event`. A `'c'`-shaped
    /// run is lexed as a [`TokenKind::CharacterLit`] instead; see the lexer.
    Tick,

    // Trivia / control
    /// A comment, retained so the formatter can reproduce it.
    Comment,
    /// End of input.
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
            TokenKind::Process => "`process`",
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
