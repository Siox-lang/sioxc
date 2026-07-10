//! Abstract syntax tree for siox Phase 1.
//!
//! Every node carries a [`Span`] for diagnostics. This module is the contract
//! between the parser (Stage 2) and every later stage. Node shapes below are a
//! starting skeleton aligned to the spec's "AST should represent" list; expect
//! to refine fields as the parser and type checker are written.

use siox_diag::Span;

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
    Struct(StructDecl),
    Enum(EnumDecl),
    Entity(EntityDecl),
    Impl(ImplDecl),
    Trait(TraitDecl),
    AttrDecl(AttrDecl),
}

/// `using std::logic::{Bit, ...};` or `using Word = uint[32];` (spec 3.4).
#[derive(Clone, Debug)]
pub struct Using {
    pub kind: UsingKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum UsingKind {
    /// `using a::b::{c, d};`
    Import { base: Path, names: Vec<Ident> },
    /// `using Word = uint[32];`
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
    pub fields: Vec<Field>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: Ident,
    pub ty: Type,
    pub span: Span,
}

/// `enum State: uint[2] { Idle = 0, ... }` (spec 3.8). No payloads in Phase 1.
#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub is_pub: bool,
    pub name: Ident,
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

/// `entity Counter<W: integer> { in clk: Clock; out count: uint[W]; }`.
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
    /// `None` means direction comes from a bus mode / recursive default
    /// (spec 3.19), e.g. `bus: in Stream<...>::Sink`.
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

/// `impl Counter<W: integer> { ... }`, `impl Trait for Type { ... }`, or a
/// directional bus mode `impl out Stream<T>::Source { ... }` (spec 3.19).
#[derive(Clone, Debug)]
pub struct ImplDecl {
    pub params: Params,
    /// `Some(trait_path)` for `impl Trait for Target`.
    pub trait_: Option<Path>,
    /// Rust-style trait type arguments: the `<integer>` in
    /// `impl Add<integer> for Complex` (the rhs operand type). Empty when the
    /// trait is unparameterized (`impl Add for T` reads as `Add<Self>`).
    pub trait_args: Vec<GenericArg>,
    /// Optional leading direction for bus-mode impls (`impl out ...`).
    pub mode_dir: Option<Direction>,
    pub target: Type,
    pub items: Vec<ImplItem>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ImplItem {
    Const(ConstDecl),
    /// Persistent state / signal: `let value: uint[W] = 0;`
    Let(LetDecl),
    /// Method / function: `fn send(self, value: T) { ... }`
    Fn(FnDecl),
    /// Bus-mode leaf direction: `in clk;` / `out valid;` (spec 3.19).
    ModeField { dir: Direction, name: Ident, span: Span },
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
    pub name: Ident,
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
    Assign { target: Expr, value: Expr, after: Option<Expr>, span: Span },
    If(IfStmt),
    Match(MatchStmt),
    /// `for i in 0..10 { ... }` over a static range (spec Stage 1 / 8).
    For { var: Ident, range: Expr, body: Block, span: Span },
    /// `assert!(cond, "msg");`, `wait 10.ns;`, `tick(clk);` (Stage 8).
    Expr(Expr),
    Return { value: Option<Expr>, span: Span },
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

/// Patterns: enum paths, bit patterns `b"01??"`, and `_` (spec 3.22).
#[derive(Clone, Debug)]
pub enum Pattern {
    Wildcard,
    Path(Path),
    BitPattern { text: String, span: Span },
}

#[derive(Clone, Debug)]
pub enum Expr {
    Int { text: String, span: Span },
    /// `1ns`, `10MHz`, `5i` — a numeric literal with an adjacent unit/type
    /// suffix. `text` is the numeric part exactly as written.
    SuffixLit { text: String, suffix: Ident, span: Span },
    /// `x"123ABC"` / `b"0101"` — bit-string literal; `base` is the prefix
    /// letter, `digits` the text between the quotes.
    BitStrLit { base: char, digits: String, span: Span },
    LogicLit { ch: char, span: Span },
    StrLit { text: String, span: Span },
    Bool { value: bool, span: Span },
    Path(Path),
    /// `x.field` (spec `.` member access).
    Field { base: Box<Expr>, field: Ident, span: Span },
    /// `x::event`, `x::old`, `clk::rising`, `data::width` (spec 3.9/3.10/3.23).
    SysAttr { base: Box<Expr>, attr: Ident, span: Span },
    /// `data[7..0]` slice or `data[0]` index (spec 3.23).
    Index { base: Box<Expr>, index: Box<Expr>, span: Span },
    /// `0..10`, `31..0`.
    Range { lo: Box<Expr>, hi: Box<Expr>, span: Span },
    Unary { op: UnOp, rhs: Box<Expr>, span: Span },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr>, span: Span },
    /// Rust-style `if c { a } else { b }` as a value (else required; branches
    /// are single expressions). `else if` chains nest in `els`.
    IfExpr { cond: Box<Expr>, then: Box<Expr>, els: Box<Expr>, span: Span },
    /// `f(a, b)` / `tick(clk)` / `assert!(...)`.
    Call { callee: Box<Expr>, args: Vec<Expr>, bang: bool, span: Span },
    /// Instance/struct construction `Counter<W = 8> { .clk, .count = c }`
    /// (spec 3.2/3.12). `ty` is `None` for a name-less struct literal
    /// `{ .valid = '1', .data = 5 }`, whose type comes from the assignment
    /// target's declaration.
    Construct {
        ty: Option<Type>,
        args: Vec<ConnectArg>,
        span: Span,
    },
    /// Bit concatenation `{a, b, c}` — the first element is the most significant.
    Concat { parts: Vec<Expr>, span: Span },
}

/// A field connection inside an instance/struct literal. `value: None` is the
/// shorthand `.clk` meaning `.clk = clk` (spec 3.12).
#[derive(Clone, Debug)]
pub struct ConnectArg {
    pub field: Ident,
    pub value: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    // Textual logical/bitwise operators (`a and b`). `nand`/`nor`/`xnor` are the
    // negated forms; `xor` is between `and` and `or` in precedence.
    And,
    Nand,
    Xor,
    Xnor,
    Or,
    Nor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Type syntax: names, parameterized types, widths and ranges.
#[derive(Clone, Debug)]
pub enum Type {
    /// `Bit`, `Logic`, `Clock`, `State`, or a path like `std::logic::Bit`.
    Path(Path),
    /// `uint[W]`, `int[8]` — a parameterized builtin width type.
    /// Also covers array/slice types `Logic[31..0]` (spec 3.23); the bracket
    /// content is an expression (a width or a range). `None` is the
    /// unconstrained form `Char[]` — the range is set at use (spec 3.23).
    Indexed { base: Box<Type>, index: Option<Box<Expr>>, span: Span },
    /// `Counter<W = 8>`, `Stream<uint[32]>` — generic application.
    Generic { base: Box<Type>, args: Vec<GenericArg>, span: Span },
    /// Directional bus-mode view: `out Stream<T>::Source`, `in Packet`
    /// (spec 3.19). `mode` is the trailing `::Source`/`::Sink` if present.
    Mode { dir: Direction, inner: Box<Type>, mode: Option<Ident>, span: Span },
}

/// One argument inside `<...>`. Spec 3.2 forbids mixing named and positional.
#[derive(Clone, Debug)]
pub enum GenericArg {
    Positional(Expr),
    Named { name: Ident, value: Expr },
}

/// Scale factor for a numeric literal suffix: femtoseconds for time units,
/// hertz for frequency units. `1ns` scales to 1_000_000 (fs), `10MHz` to
/// 10_000_000 (Hz).
// ponytail: fixed table — becomes std-defined suffix declarations (Time/Freq/
// Complex types) when literal-suffix overloading lands.
/// Rust-style operator-trait names (spec 3.25): `a + b` dispatches to an
/// `impl Add for <type of a>` with a method selected by the rhs type. Names
/// follow Rust's `std::ops` where Rust has the operator; siox's extra logic
/// words get matching names. `==`/`!=` stay built-in (or derive from `Ord`).
pub fn op_trait_name(op: &str) -> Option<&'static str> {
    Some(match op {
        "+" => "Add",
        "-" => "Sub",
        "*" => "Mul",
        "/" => "Div",
        "<<" => "Shl",
        ">>" => "Shr",
        "and" => "BitAnd",
        "or" => "BitOr",
        "xor" => "BitXor",
        "nand" => "Nand",
        "nor" => "Nor",
        "xnor" => "Xnor",
        "not" => "Not",
        "<=>" => "Ord",
        _ => return None,
    })
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
