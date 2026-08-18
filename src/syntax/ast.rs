//! Abstract syntax tree for siox Phase 1.
//!
//! Every node carries a [`Span`] for diagnostics. This module is the contract
//! between the parser (Stage 2) and every later stage. Node shapes below are a
//! starting skeleton aligned to the spec's "AST should represent" list; expect
//! to refine fields as the parser and type checker are written.

use crate::diag::Span;

/// A parsed source file: `module <path>;` followed by items.
#[derive(Clone, Debug)]
pub struct Module {
    pub path: Path,
    pub items: Vec<Item>,
    pub span: Span,
}

/// A `::`-separated path such as `std::logic::Bit` (spec 3 / Stage 3).
#[derive(Clone, Debug)]
pub struct Path {
    pub segments: Vec<Ident>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Ident {
    pub text: String,
    pub span: Span,
}

/// Top-level (module-scope) declarations.
#[derive(Clone, Debug)]
pub enum Item {
    Using(Using),
    Const(ConstDecl),
    /// A module-level function (spec 3.25-adjacent): pure `return`/`if`-chain
    /// bodies, inlined at lowering like operator impls; const-evaluable when
    /// its arguments are (so `clog2(DEPTH)` works in width positions).
    Fn(FnDecl),
    /// `extern "C" { fn sqrt(x: real) -> real; ... }` — foreign C functions
    /// callable from siox: `real` maps to `double`, integer-shaped types to
    /// 64-bit words. Native binaries resolve the named symbols while linking.
    ExternBlock {
        abi: String,
        fns: Vec<FnDecl>,
        span: Span,
    },
    Struct(StructDecl),
    View(ViewDecl),
    Enum(EnumDecl),
    Entity(EntityDecl),
    Impl(ImplDecl),
    Trait(TraitDecl),
    AttrDecl(AttrDecl),
}

/// `using std::logic::{Bit, ...};` or `using Word = unsigned[32];` (spec 3.4).
#[derive(Clone, Debug)]
pub struct Using {
    pub is_pub: bool,
    pub kind: UsingKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum UsingKind {
    /// `using a::b::{c, d};`
    Import { base: Path, names: Vec<Ident> },
    /// `using Word = unsigned[32];`
    Alias { name: Ident, ty: Type },
}

/// `const NAME: Ty = expr;` — module scope or inside impl (spec 3.3).
#[derive(Clone, Debug)]
pub struct ConstDecl {
    pub is_pub: bool,
    pub name: Ident,
    pub ty: Type,
    pub value: Expr,
    pub span: Span,
}

/// Generic/elaboration parameter list `<W: integer, T>` (spec 3.2).
#[derive(Clone, Debug, Default)]
pub struct Params {
    pub params: Vec<Param>,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: Ident,
    /// `None` for a bare type parameter `<T>`; `Some` for `<W: integer>`.
    pub bound: Option<Type>,
    pub span: Span,
}

/// `struct Packet<T> { valid: Bit, data: T }` (spec 3.7). No directions.
#[derive(Clone, Debug)]
pub struct StructDecl {
    pub is_pub: bool,
    pub name: Ident,
    pub params: Params,
    /// Nominal derivation base (`struct B : A`): `B` reuses `A`'s
    /// representation as a distinct type, optionally adding `fields`. `None`
    /// for a plain aggregate struct.
    pub base: Option<Type>,
    pub fields: Vec<Field>,
    pub span: Span,
}

/// `view Source<T> for Stream<T> { valid out, ready in, }`.
///
/// A view is a named, storage-free directional projection of a struct. It is
/// a nominal type for method/trait lookup and reuses its target's fields and
/// representation.
#[derive(Clone, Debug)]
pub struct ViewDecl {
    pub is_pub: bool,
    pub name: Ident,
    pub params: Params,
    /// Backing struct named by `for Struct`.
    pub target: Type,
    pub fields: Vec<ViewField>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ViewField {
    pub dir: Direction,
    pub name: Ident,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub is_pub: bool,
    pub name: Ident,
    pub ty: Type,
    pub span: Span,
}

/// `enum State: unsigned[2] { Idle = 0, ... }` (spec 3.8). No payloads in Phase 1.
#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub is_pub: bool,
    pub name: Ident,
    /// The `: Type` after the name. When it resolves to an enum this is a
    /// nominal derivation BASE (`enum Logic : ULogic` inherits its variants);
    /// when numeric it is the discriminant representation (`enum S : unsigned[2]`).
    pub repr: Option<Type>,
    pub variants: Vec<EnumVariant>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct EnumVariant {
    pub name: Ident,
    pub value: Option<Expr>,
    pub span: Span,
}

/// `entity Counter<W: integer> { clk: Bit in, count: unsigned[W] out, }`.
///
/// Entity bodies are interface-only (spec 3.1): ports and bus/interface
/// fields, never state or behavior.
#[derive(Clone, Debug)]
pub struct EntityDecl {
    pub attrs: Vec<Attr>,
    pub is_pub: bool,
    pub is_extern: bool,
    pub name: Ident,
    pub params: Params,
    pub ports: Vec<Port>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Port {
    /// `None` means direction comes from an applied view (spec 3.19), e.g.
    /// `bus: Sink Stream<...>`.
    pub dir: Option<Direction>,
    pub name: Ident,
    pub ty: Type,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
    Inout,
}

/// `impl<W: integer> Counter<W> { ... }` or `impl Trait for Type { ... }`.
#[derive(Clone, Debug)]
pub struct ImplDecl {
    /// Metadata on an implementation. Custom operators use `precedence` here.
    pub attrs: Vec<Attr>,
    pub params: Params,
    /// `Some(trait_path)` for `impl Trait for Target`.
    pub trait_: Option<Path>,
    /// Rust-style trait type arguments: the `<integer>` in
    /// `impl Add<integer> for Complex` (the rhs operand type). Empty when the
    /// trait is unparameterized (`impl Add for T` reads as `Add<Self>`).
    pub trait_args: Vec<GenericArg>,
    pub target: Type,
    pub items: Vec<ImplItem>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ImplItem {
    Const(ConstDecl),
    /// Persistent state / signal: `let value: unsigned[W] = 0;`
    Let(LetDecl),
    /// Method / function: `fn send(self, value: T) { ... }`
    Fn(FnDecl),
    /// Bus-mode leaf direction: `in clk;` / `out valid;` (spec 3.19).
    ModeField {
        dir: Direction,
        name: Ident,
        span: Span,
    },
    /// Bare behavioral statement (combinational or event-controlled block).
    Stmt(Stmt),
}

/// `trait ClockLike { fn rising(self); ... }` (spec 3.20). Compile-time only.
#[derive(Clone, Debug)]
pub struct TraitDecl {
    pub is_pub: bool,
    pub name: Ident,
    pub params: Params,
    pub items: Vec<FnDecl>,
    pub span: Span,
}

/// `pub attr top: Bool for entity;` (spec 3.5).
#[derive(Clone, Debug)]
pub struct AttrDecl {
    pub is_pub: bool,
    pub name: Ident,
    pub ty: Type,
    pub targets: Vec<Ident>, // entity, let, port, instance, node, signal, ...
    pub span: Span,
}

/// An applied attribute `#[top]` / `#[name = "x"]` (spec 3.5/3.6).
#[derive(Clone, Debug)]
pub struct Attr {
    pub name: Path,
    /// `None` is boolean shorthand `#[top]` == `#[top = true]`.
    pub value: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FnDecl {
    /// Module functions and inherent methods are private unless explicitly
    /// exported. Trait requirements inherit the trait's visibility.
    pub is_pub: bool,
    pub name: Ident,
    /// Type parameters with optional trait bounds: `fn max<T: Ord>(...)`.
    /// Bounds are checked at each call site (fns inline, so a call is a
    /// monomorphization). Empty for a non-generic fn.
    pub generics: Params,
    pub params: Vec<FnParam>,
    pub ret: Option<Type>,
    /// `None` for a trait requirement signature without a body.
    pub body: Option<Block>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FnParam {
    /// `self` receiver vs. a named parameter.
    pub is_self: bool,
    pub name: Option<Ident>,
    pub ty: Option<Type>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct LetDecl {
    /// Metadata attributes on the declaration (`#[external_clock] let p =
    /// Pll { .. };`) — per-instance values for type-targeted attrs (spec 3.5).
    pub attrs: Vec<Attr>,
    pub name: Ident,
    pub ty: Option<Type>,
    pub value: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let(LetDecl),
    /// `target = expr;` — meaning resolved by context (spec 3.12).
    /// `x = v;`, optionally delayed VHDL-style: `clk = !clk after 5ns;`
    /// (`after` is testbench-only in Phase 1; the self-toggle idiom is the
    /// canonical clock generator).
    Assign {
        target: Expr,
        value: Expr,
        after: Option<Expr>,
        span: Span,
    },
    If(IfStmt),
    Match(MatchStmt),
    /// `for i in 0..10 { ... }` over a static range (spec Stage 1 / 8).
    For {
        var: Ident,
        range: Expr,
        body: Block,
        span: Span,
    },
    /// `assert!(cond, "msg");`, `wait 10.ns;`, `tick(clk);` (Stage 8).
    Expr(Expr),
    Return {
        value: Option<Expr>,
        span: Span,
    },
}

#[derive(Clone, Debug)]
pub struct IfStmt {
    pub cond: Expr,
    pub then: Block,
    /// Optional `else` / `else if` chain.
    pub else_: Option<Box<ElseBranch>>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ElseBranch {
    Block(Block),
    If(IfStmt),
}

#[derive(Clone, Debug)]
pub struct MatchStmt {
    pub scrutinee: Expr,
    pub arms: Vec<MatchArm>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Block,
    pub span: Span,
}

impl MatchArm {
    /// The arm's value in a match *expression*: its body is a single expression
    /// (`A => a + b`) or a bare `return`. `None` for a statement arm.
    pub fn value_expr(&self) -> Option<&Expr> {
        match self.body.stmts.as_slice() {
            [Stmt::Expr(e)] => Some(e),
            [Stmt::Return { value: Some(e), .. }] => Some(e),
            _ => None,
        }
    }
}

/// Patterns: enum paths, bit patterns `"01--"` / `x"A?"`, and `_` (spec 3.22).
#[derive(Clone, Debug)]
pub enum Pattern {
    Wildcard,
    Path(Path),
    BitPattern {
        text: String,
        span: Span,
    },
    /// `A | B | C` — matches if any alternative matches (spec 3.22).
    Or {
        alts: Vec<Pattern>,
        span: Span,
    },
    /// An integer literal (`5`) or inclusive range (`0..9`) pattern for a
    /// numeric scrutinee; a bare literal is `lo == hi`.
    Range {
        lo: i64,
        hi: i64,
        span: Span,
    },
    /// A character literal (`'0'`, `'Z'`) naming a variant of a char-valued
    /// enum — `Logic` above all. Like the expression form it has no intrinsic
    /// value: the variant it selects comes from the scrutinee's type.
    CharLit {
        ch: char,
        span: Span,
    },
}

#[derive(Clone, Debug)]
pub enum Expr {
    Int {
        text: String,
        span: Span,
    },
    /// `1ns`, `10MHz`, `5i` — a numeric literal with an adjacent unit/type
    /// suffix. `text` is the numeric part exactly as written.
    SuffixLit {
        text: String,
        suffix: Ident,
        span: Span,
    },
    /// `x"123ABC"` / `o"17"` — a radix bit-string literal; `base` is the
    /// prefix letter (validated against std's `impl Prefix`), `digits` the
    /// text between the quotes. (A plain string is `StrLit`, not this.)
    BitStrLit {
        base: char,
        digits: String,
        span: Span,
    },
    /// A single character between single quotes (`'g'`, `'0'`). A character
    /// literal has no intrinsic value — its type (and so its numeric value)
    /// comes from context: the enum it is assigned to or compared against.
    CharLit {
        ch: char,
        span: Span,
    },
    StrLit {
        text: String,
        span: Span,
    },
    Path(Path),
    /// `x.field` (spec `.` member access).
    Field {
        base: Box<Expr>,
        field: Ident,
        span: Span,
    },
    /// A VHDL-style attribute tick: `sig'event`, `sig'old`, `data'length`,
    /// `arr'high` (spec 3.9/3.10/3.23). `'` is exclusively for attributes; `::`
    /// is namespace/type selection and `.` is field/method access.
    SysAttr {
        base: Box<Expr>,
        attr: Ident,
        span: Span,
    },
    /// `data[7..0]` slice or `data[0]` index (spec 3.23).
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
        span: Span,
    },
    /// `0..10`, `31..0`.
    Range {
        lo: Box<Expr>,
        hi: Box<Expr>,
        span: Span,
    },
    /// An inclusive range with an omitted bound: `..4`, `1..`, or `..`.
    /// The surrounding indexing operation supplies omitted `left`/`right`
    /// bounds; other contexts diagnose the missing bounds.
    PartialRange {
        lo: Option<Box<Expr>>,
        hi: Option<Box<Expr>>,
        span: Span,
    },
    Unary {
        op: UnOp,
        rhs: Box<Expr>,
        span: Span,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
    /// Rust-style `if c { a } else { b }` as a value (else required; branches
    /// are single expressions). `else if` chains nest in `els`.
    IfExpr {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
        span: Span,
    },
    /// `match s { A => e1, _ => e2 }` in value position — each arm's body is a
    /// single expression (spec 3.22).
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
        span: Span,
    },
    /// `f(a, b)` / `read<string>(path)` / `assert!(...)`.
    Call {
        callee: Box<Expr>,
        /// Explicit type construction arguments. Phase 1 uses this for
        /// constructor-like intrinsics such as `read<T>`; ordinary generic
        /// functions continue to infer their type parameters from values.
        type_args: Vec<Type>,
        args: Vec<Expr>,
        bang: bool,
        span: Span,
    },
    /// Instance/struct construction `Counter<W = 8> { .clk = clk, .count = c }`
    /// (spec 3.2/3.12). `ty` is `None` for a name-less struct literal
    /// `{ .valid = '1', .data = 5 }`, whose type comes from the assignment
    /// target's declaration.
    Construct {
        ty: Option<Type>,
        args: Vec<ConnectArg>,
        /// Struct spread-update base: `{ ..base, .x = v }` takes every field
        /// from `base` and overrides the ones in `args`. `None` for a plain
        /// literal.
        spread: Option<Box<Expr>>,
        span: Span,
    },
    /// Bit concatenation `{a, b, c}` — the first element is the most significant.
    Concat {
        parts: Vec<Expr>,
        span: Span,
    },
    /// `[a, b, c]` — an array literal (spec 3.23), one value per element.
    Array {
        elems: Vec<Expr>,
        span: Span,
    },
}

/// A field connection inside an instance/struct literal (spec 3.12). Two
/// shapes:
/// - **explicit** `.clk = sig` — `field: Some`, `value: Some`.
/// - **positional** `sig` — `field: None`, `value: Some`; bound to the port /
///   struct field at this argument's ordinal position.
///
/// (`value: None` is only an error-recovery artifact — a `.field` written
/// without a value; the bare `.field` name-shorthand is not a form.)
#[derive(Clone, Debug)]
pub struct ConnectArg {
    pub field: Option<Ident>,
    pub value: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    // The only core textual binary operators.
    And,
    Or,
    /// A library/user-defined textual infix operator. Its binding power comes
    /// from the implementation's `#[precedence = N]` metadata.
    Custom {
        symbol: String,
        precedence: u8,
    },
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    /// True when the result carries the operands' family, so an expression
    /// built from this operator answers "is it signed", "is it this enum",
    /// "how wide is it" the same way its operands do.
    ///
    /// `and`/`or` belong here: they are not fixed to `Bool` but overloaded
    /// per type (`Operator<"and", Logic, Logic> for Logic`), so they return
    /// what they were given — `x and y` on `Logic` is a `Logic`.
    ///
    /// Comparisons yield `Bool` or `Ordering` whatever their operands were,
    /// and a custom operator's result comes from its impl's declared output.
    pub fn keeps_operand_family(&self) -> bool {
        matches!(
            self,
            BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Shl
                | BinOp::Shr
                | BinOp::And
                | BinOp::Or
        )
    }
}

/// Type syntax: names, parameterized types, widths and ranges.
#[derive(Clone, Debug)]
pub enum Type {
    /// `Bit`, `Logic`, `State`, or a path like `std::logic::Bit`.
    Path(Path),
    /// `unsigned[W]`, `signed[8]` — a parameterized builtin width type.
    /// Also covers array/slice types `Logic[31..0]` (spec 3.23); the bracket
    /// content is an expression (a width or a range). `None` is the
    /// unconstrained form `Char[]` — the range is set at use (spec 3.23).
    Indexed {
        base: Box<Type>,
        index: Option<Box<Expr>>,
        span: Span,
    },
    /// `Counter<W = 8>`, `Stream<unsigned[32]>` — generic application.
    Generic {
        base: Box<Type>,
        args: Vec<GenericArg>,
        span: Span,
    },
    /// A named view applied to a backing struct: `Source Stream<T>`.
    View {
        view: Path,
        target: Box<Type>,
        span: Span,
    },
}

/// One argument inside `<...>`. Spec 3.2 forbids mixing named and positional.
#[derive(Clone, Debug)]
pub enum GenericArg {
    Positional(Expr),
    /// An unambiguously type-shaped nested application (`Box<T>` inside
    /// `Outer<Box<T>>`). Bare names and indexed forms remain expressions until
    /// their parameter kind disambiguates them in later stages.
    PositionalType(Type),
    Named {
        name: Ident,
        value: Expr,
    },
    NamedType {
        name: Ident,
        ty: Type,
    },
}

/// Native scheduler scale for the std-defined physical suffixes:
/// femtoseconds for time units and hertz for frequency units. Expression
/// typing and value construction come from `std::sim`'s `Suffix` impls; this
/// table converts durations at the generated scheduler boundary.
/// Rust-style operator-trait names (spec 3.25): `a + b` dispatches to an
/// `impl Add for <type of a>` with a method selected by the rhs type. Names
/// follow Rust's `std::ops` where that matches the language. Siox uses one
/// type-directed `And` contract for both scalar boolean and per-bit `and`;
/// `==`/`!=` stay built-in (or derive from `Ord`).
/// The source span of any expression node.
/// The source span of a statement.
///
/// Used to attribute generated code back to the line that produced it, so a
/// debugger and a runtime failure both name the source rather than the
/// intermediate the compiler emitted.
pub fn stmt_span(s: &Stmt) -> Span {
    match s {
        Stmt::Let(l) => l.span,
        Stmt::Assign { span, .. } => *span,
        Stmt::If(i) => i.span,
        Stmt::Match(m) => m.span,
        Stmt::For { span, .. } => *span,
        Stmt::Expr(e) => expr_span(e),
        Stmt::Return { span, .. } => *span,
    }
}

pub fn expr_span(e: &Expr) -> Span {
    match e {
        Expr::Int { span, .. }
        | Expr::SuffixLit { span, .. }
        | Expr::BitStrLit { span, .. }
        | Expr::CharLit { span, .. }
        | Expr::StrLit { span, .. }
        | Expr::Field { span, .. }
        | Expr::SysAttr { span, .. }
        | Expr::IfExpr { span, .. }
        | Expr::Match { span, .. }
        | Expr::Index { span, .. }
        | Expr::Range { span, .. }
        | Expr::PartialRange { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Binary { span, .. }
        | Expr::Call { span, .. }
        | Expr::Construct { span, .. }
        | Expr::Concat { span, .. }
        | Expr::Array { span, .. } => *span,
        Expr::Path(p) => p.span,
    }
}

/// The standard operator symbols that carry built-in precedence — an
/// `impl Operator<sym, _, _>` for one of these needs no `#[precedence]`. Any
/// other symbol (a user operator like `xor`) must declare its precedence.
pub fn is_builtin_operator(sym: &str) -> bool {
    matches!(
        sym,
        "+" | "-" | "*" | "/" | "<<" | ">>" | "and" | "or" | "not" | "<=>"
    )
}

/// Symbols the grammar reserves for the language itself — assignment, paths,
/// ranges, separators, brackets, attributes — so an `Operator<sym, …>` impl
/// cannot claim them (spec 3.25). The six comparisons are reserved too: they
/// are derived from the three-way `<=>`, so overload that instead. An empty
/// symbol is rejected here as well.
pub fn is_reserved_operator(sym: &str) -> bool {
    matches!(
        sym,
        "" | "="
            | "::"
            | ":"
            | ";"
            | ","
            | "."
            | ".."
            | "=>"
            | "->"
            | "#"
            | "!"
            | "&"
            | "|"
            | "@"
            | "<"
            | ">"
            | "=="
            | "!="
            | "<="
            | ">="
            | "+="
            | "-="
            | "*="
            | "/="
            | "&="
            | "|="
            | "("
            | ")"
            | "{"
            | "}"
            | "["
            | "]"
    )
}

/// Whether `sym` is one of the six comparison operators derived from `<=>`.
pub fn is_comparison_operator(sym: &str) -> bool {
    matches!(sym, "<" | ">" | "==" | "!=" | "<=" | ">=")
}

pub fn suffix_scale(s: &str) -> Option<u128> {
    Some(match s {
        "fs" => 1,
        "ps" => 1_000,
        "ns" => 1_000_000,
        "us" => 1_000_000_000,
        "ms" => 1_000_000_000_000,
        "s" => 1_000_000_000_000_000,
        "Hz" => 1,
        "kHz" => 1_000,
        "MHz" => 1_000_000,
        "GHz" => 1_000_000_000,
        _ => return None,
    })
}
