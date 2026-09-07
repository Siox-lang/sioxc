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
    /// The declared module path from the leading `module a::b;`.
    pub path: Path,
    /// Top-level declarations, in source order.
    pub items: Vec<Item>,
    /// The whole file, from `module` to the last item.
    pub span: Span,
}

/// A `::`-separated path such as `std::logic::Bit` (spec 3 / Stage 3).
#[derive(Clone, Debug)]
pub struct Path {
    /// Segments between `::`, at least one. `std::logic::Bit` has three.
    pub segments: Vec<Ident>,
    /// The whole path, first segment through last.
    pub span: Span,
}

/// A single identifier token, kept with its span so later stages can point at
/// the name the user wrote rather than at a synthesized one.
#[derive(Clone, Debug)]
pub struct Ident {
    /// The identifier exactly as spelled in the source.
    pub text: String,
    /// The identifier's own extent, excluding surrounding punctuation.
    pub span: Span,
}

/// Top-level (module-scope) declarations.
#[derive(Clone, Debug)]
pub enum Item {
    /// `using std::logic::Bit;` — an import or a type alias.
    Using(Using),
    /// A module-scope `const`.
    Const(ConstDecl),
    /// A module-level function (spec 3.25-adjacent): pure `return`/`if`-chain
    /// bodies, inlined at lowering like operator impls; const-evaluable when
    /// its arguments are (so `clog2(DEPTH)` works in width positions).
    Fn(FnDecl),
    /// `extern "C" { fn sqrt(x: real) -> real; ... }` — foreign C functions
    /// callable from siox: `real` maps to `double`, integer-shaped types to
    /// 64-bit words. Native binaries resolve the named symbols while linking.
    ExternBlock {
        /// The ABI string after `extern`; only `"C"` is accepted in Phase 1.
        abi: String,
        /// Bodiless signatures declared in the block.
        fns: Vec<FnDecl>,
        /// `extern` keyword through the closing brace.
        span: Span,
    },
    /// An aggregate or nominally derived `struct`.
    Struct(StructDecl),
    /// A directional `view` projection over a struct.
    View(ViewDecl),
    /// An `enum`, optionally with a representation or derivation base.
    Enum(EnumDecl),
    /// An `entity` interface declaration.
    Entity(EntityDecl),
    /// An inherent or trait `impl` block.
    Impl(ImplDecl),
    /// A `trait` declaration.
    Trait(TraitDecl),
    /// A user-defined attribute declaration (`attr top: Bool for entity;`).
    AttrDecl(AttrDecl),
}

/// `using std::logic::{Bit, ...};` or `using Word = unsigned[32];` (spec 3.4).
#[derive(Clone, Debug)]
pub struct Using {
    /// `pub using ...` re-exports the name from this module.
    pub is_pub: bool,
    /// Whether this brings names in or defines an alias.
    pub kind: UsingKind,
    /// `using` keyword through the terminating `;`.
    pub span: Span,
}

/// Which of the two `using` forms was written.
#[derive(Clone, Debug)]
pub enum UsingKind {
    /// `using a::b::{c, d};`
    Import {
        /// The path shared by every imported name (`a::b`).
        base: Path,
        /// Names taken from `base`. A single-name import has one entry.
        names: Vec<Ident>,
    },
    /// `using Word = unsigned[32];`
    Alias {
        /// The new name introduced in this module.
        name: Ident,
        /// The type it stands for. An alias is transparent, not nominal.
        ty: Type,
    },
}

/// `const NAME: Ty = expr;` — module scope or inside impl (spec 3.3).
#[derive(Clone, Debug)]
pub struct ConstDecl {
    /// Whether the constant is exported from its module or impl.
    pub is_pub: bool,
    /// The constant's name.
    pub name: Ident,
    /// The declared type; constants are never inferred.
    pub ty: Type,
    /// The initializer, which must be const-evaluable.
    pub value: Expr,
    /// `const` keyword through the terminating `;`.
    pub span: Span,
}

/// Generic/elaboration parameter list `<W: integer, T>` (spec 3.2).
#[derive(Clone, Debug, Default)]
pub struct Params {
    /// The parameters in declaration order. Empty when no `<...>` was written,
    /// which is why this defaults rather than being optional.
    pub params: Vec<Param>,
}

/// One generic or elaboration parameter inside a [`Params`] list.
#[derive(Clone, Debug)]
pub struct Param {
    /// The parameter's name, used both at the use site and in `<W = 8>`.
    pub name: Ident,
    /// `None` for a bare type parameter `<T>`; `Some` for `<W: integer>`.
    pub bound: Option<Type>,
    /// The parameter's own extent inside the `<...>` list.
    pub span: Span,
}

/// `struct Packet<T> { valid: Bit, data: T }` (spec 3.7). No directions.
#[derive(Clone, Debug)]
pub struct StructDecl {
    /// Whether the struct is visible outside its module.
    pub is_pub: bool,
    /// The type's name, which is also its nominal identity.
    pub name: Ident,
    /// Generic parameters; empty for a non-parameterized struct.
    pub params: Params,
    /// Nominal derivation base (`struct B : A`): `B` reuses `A`'s
    /// representation as a distinct type, optionally adding `fields`. `None`
    /// for a plain aggregate struct.
    pub base: Option<Type>,
    /// Declared fields, in layout order. Empty for a derived leaf type whose
    /// representation comes entirely from `base`.
    pub fields: Vec<Field>,
    /// `struct` keyword through the closing brace.
    pub span: Span,
}

/// `view Source<T> for Stream<T> { valid out, ready in, }`.
///
/// A view is a named, storage-free directional projection of a struct. It is
/// a nominal type for method/trait lookup and reuses its target's fields and
/// representation.
#[derive(Clone, Debug)]
pub struct ViewDecl {
    /// Whether the view is visible outside its module.
    pub is_pub: bool,
    /// The view's name, used as a type qualifier at a port (`Source Stream`).
    pub name: Ident,
    /// Generic parameters, which usually mirror the target struct's.
    pub params: Params,
    /// Backing struct named by `for Struct`.
    pub target: Type,
    /// A direction for each field the view names. Fields the view omits are
    /// not reachable through it.
    pub fields: Vec<ViewField>,
    /// `view` keyword through the closing brace.
    pub span: Span,
}

/// One field's direction inside a [`ViewDecl`], e.g. the `valid out` in
/// `view Source for Stream { valid out, ready in, }`.
#[derive(Clone, Debug)]
pub struct ViewField {
    /// Which way data flows through this field for a port using the view.
    pub dir: Direction,
    /// The backing struct field this redirects; it must exist on the target.
    pub name: Ident,
    /// The `name dir` pair's extent.
    pub span: Span,
}

/// One declared field of a [`StructDecl`].
#[derive(Clone, Debug)]
pub struct Field {
    /// Whether the field is readable outside its defining module.
    pub is_pub: bool,
    /// The field's name, used for `.field` access and `.field =` connection.
    pub name: Ident,
    /// The field's declared type; struct fields are never inferred.
    pub ty: Type,
    /// The `name: Type` pair's extent.
    pub span: Span,
}

/// `enum State: unsigned[2] { Idle = 0, ... }` (spec 3.8). No payloads in Phase 1.
#[derive(Clone, Debug)]
pub struct EnumDecl {
    /// Whether the enum is visible outside its module.
    pub is_pub: bool,
    /// The enum's name, which is also its nominal identity.
    pub name: Ident,
    /// The `: Type` after the name. When it resolves to an enum this is a
    /// nominal derivation BASE (`enum Logic : ULogic` inherits its variants);
    /// when numeric it is the discriminant representation (`enum S : unsigned[2]`).
    pub repr: Option<Type>,
    /// The declared variants in source order. Empty when every variant is
    /// inherited from a derivation base.
    pub variants: Vec<EnumVariant>,
    /// `enum` keyword through the closing brace.
    pub span: Span,
}

/// One variant of an [`EnumDecl`]. Phase 1 variants carry no payload.
#[derive(Clone, Debug)]
pub struct EnumVariant {
    /// The variant's name, selected as `Enum::Name`.
    pub name: Ident,
    /// An explicit discriminant (`Idle = 0`). `None` continues the implicit
    /// sequence from the previous variant.
    pub value: Option<Expr>,
    /// The variant's own extent, including any `= value`.
    pub span: Span,
}

/// `entity Counter<W: integer> { clk: Bit in, count: unsigned[W] out, }`.
///
/// Entity bodies are interface-only (spec 3.1): ports and bus/interface
/// fields, never state or behavior.
#[derive(Clone, Debug)]
pub struct EntityDecl {
    /// Applied attributes such as `#[test]` or `#[top]`. These are metadata
    /// for tooling and test discovery, not compiler directives.
    pub attrs: Vec<Attr>,
    /// Whether the entity is instantiable from outside its module.
    pub is_pub: bool,
    /// `extern entity` — declared here but implemented outside siox, so no
    /// `impl` body is required and nothing is elaborated for it.
    pub is_extern: bool,
    /// The entity's name, used to instantiate it and to name it to vendor
    /// tooling.
    pub name: Ident,
    /// Elaboration parameters (`<W: integer>`), substituted per instance.
    pub params: Params,
    /// The interface: every port, in declaration order.
    pub ports: Vec<Port>,
    /// `entity` keyword through the closing brace.
    pub span: Span,
}

/// One port in an [`EntityDecl`]'s interface.
#[derive(Clone, Debug)]
pub struct Port {
    /// `None` means direction comes from an applied view (spec 3.19), e.g.
    /// `bus: Sink Stream<...>`.
    pub dir: Option<Direction>,
    /// The port's name, used when connecting an instance.
    pub name: Ident,
    /// The port's type, including any width or view qualifier.
    pub ty: Type,
    /// The whole port declaration's extent.
    pub span: Span,
}

/// Which way data flows through a port or view field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Readable inside the entity, driven by the instantiator. Writing one is
    /// an error (`WRITE_TO_INPUT_PORT`).
    In,
    /// Driven inside the entity, readable by the instantiator.
    Out,
    /// Driven from either side; resolved through the type's `Resolve` impl,
    /// which is how a tristate pin gets `'Z'` semantics.
    Inout,
}

/// `impl<W: integer> Counter<W> { ... }` or `impl Trait for Type { ... }`.
#[derive(Clone, Debug)]
pub struct ImplDecl {
    /// Metadata on an implementation. Custom operators use `precedence` here.
    pub attrs: Vec<Attr>,
    /// The impl's own generic binder — the `<W: integer>` in
    /// `impl<W: integer> Counter<W>`. This spelling is required; shorthands
    /// are rejected.
    pub params: Params,
    /// `Some(trait_path)` for `impl Trait for Target`.
    pub trait_: Option<Path>,
    /// Rust-style trait type arguments: the `<integer>` in
    /// `impl Add<integer> for Complex` (the rhs operand type). Empty when the
    /// trait is unparameterized (`impl Add for T` reads as `Add<Self>`).
    pub trait_args: Vec<GenericArg>,
    /// The type receiving the implementation — the `Counter<W>` in
    /// `impl<W: integer> Counter<W>`.
    pub target: Type,
    /// The block's members, in source order.
    pub items: Vec<ImplItem>,
    /// `impl` keyword through the closing brace.
    pub span: Span,
}

/// One member of an [`ImplDecl`] body.
#[derive(Clone, Debug)]
pub enum ImplItem {
    /// An associated constant.
    Const(ConstDecl),
    /// Persistent state / signal: `let value: unsigned[W] = 0;`
    Let(LetDecl),
    /// Method / function: `fn send(self, value: T) { ... }`
    Fn(FnDecl),
    /// Bus-mode leaf direction: `in clk;` / `out valid;` (spec 3.19).
    ModeField {
        /// The direction this leaf takes in the bus mode being implemented.
        dir: Direction,
        /// The struct field the direction applies to.
        name: Ident,
        /// The `dir name;` statement's extent.
        span: Span,
    },
    /// A concurrent hardware process whose body executes sequentially.
    Process(ProcessDecl),
    /// Bare behavioral statement (combinational or event-controlled block).
    Stmt(Stmt),
}

/// A `process { ... }` block: concurrent with other processes, sequential
/// inside. Declarations and structural instances stay outside it.
#[derive(Clone, Debug)]
pub struct ProcessDecl {
    /// Optional diagnostic/tooling label: `process receive { ... }`.
    pub name: Option<Ident>,
    /// Statements the process runs. They execute sequentially within the
    /// process even though processes are concurrent with each other.
    pub body: Block,
    /// `process` keyword through the closing brace.
    pub span: Span,
}

/// `trait ClockLike { fn rising(self); ... }` (spec 3.20). Compile-time only.
#[derive(Clone, Debug)]
pub struct TraitDecl {
    /// Whether the trait can be implemented outside its module.
    pub is_pub: bool,
    /// The trait's name, used in bounds and in `impl Trait for T`.
    pub name: Ident,
    /// Trait parameters, such as the operand types of `Operator<sym, In, Out>`.
    pub params: Params,
    /// Required methods. A body makes the requirement defaulted.
    pub items: Vec<FnDecl>,
    /// `trait` keyword through the closing brace.
    pub span: Span,
}

/// `pub attr top: Bool for entity;` (spec 3.5).
#[derive(Clone, Debug)]
pub struct AttrDecl {
    /// Whether the attribute can be applied outside its module.
    pub is_pub: bool,
    /// The attribute's name, written as `#[name]` at a use site.
    pub name: Ident,
    /// The type of the attribute's value; `Bool` allows the `#[name]`
    /// shorthand.
    pub ty: Type,
    /// Declaration kinds the attribute may be applied to — `entity`, `let`,
    /// `port`, `instance`, `node`, `signal`, and so on. Applying it elsewhere
    /// is a diagnostic.
    pub targets: Vec<Ident>,
    /// `attr` keyword through the terminating `;`.
    pub span: Span,
}

/// An applied attribute `#[top]` / `#[name = "x"]` (spec 3.5/3.6).
#[derive(Clone, Debug)]
pub struct Attr {
    /// The attribute being applied, resolved against `attr` declarations.
    pub name: Path,
    /// `None` is boolean shorthand `#[top]` == `#[top = true]`.
    pub value: Option<Expr>,
    /// The whole `#[...]` form's extent.
    pub span: Span,
}

/// A function signature and optional body: a module function, an inherent or
/// trait method, a trait requirement, or an `extern "C"` declaration.
#[derive(Clone, Debug)]
pub struct FnDecl {
    /// Module functions and inherent methods are private unless explicitly
    /// exported. Trait requirements inherit the trait's visibility.
    pub is_pub: bool,
    /// The function's name, used at the call site.
    pub name: Ident,
    /// Type parameters with optional trait bounds: `fn max<T: Ord>(...)`.
    /// Bounds are checked at each call site (fns inline, so a call is a
    /// monomorphization). Empty for a non-generic fn.
    pub generics: Params,
    /// Value parameters in order; a leading `self` receiver is the first entry.
    pub params: Vec<FnParam>,
    /// The declared return type. `None` means the function returns nothing.
    pub ret: Option<Type>,
    /// `None` for a trait requirement signature without a body.
    pub body: Option<Block>,
    /// `fn` keyword through the closing brace, or through `;` when bodiless.
    pub span: Span,
}

/// One parameter of an [`FnDecl`], either the `self` receiver or a named
/// value parameter.
#[derive(Clone, Debug)]
pub struct FnParam {
    /// `self` receiver vs. a named parameter.
    pub is_self: bool,
    /// The parameter's name. `None` for the `self` receiver.
    pub name: Option<Ident>,
    /// The declared type. `None` for `self`, whose type is the impl target.
    pub ty: Option<Type>,
    /// The parameter's own extent in the argument list.
    pub span: Span,
}

/// A `let` declaration: persistent signal state in an impl body, or a local
/// binding inside a process or function.
#[derive(Clone, Debug)]
pub struct LetDecl {
    /// Metadata attributes on the declaration (`#[external_clock] let p =
    /// Pll { .. };`) — per-instance values for type-targeted attrs (spec 3.5).
    pub attrs: Vec<Attr>,
    /// The name being bound.
    pub name: Ident,
    /// The declared type. `None` infers it from `value`, which must then be
    /// present.
    pub ty: Option<Type>,
    /// The initializer. `None` leaves the signal at its type's structural
    /// value — always initialized, but undriven.
    pub value: Option<Expr>,
    /// `let` keyword through the terminating `;`.
    pub span: Span,
}

/// A brace-delimited statement sequence.
#[derive(Clone, Debug)]
pub struct Block {
    /// The statements, in execution order.
    pub stmts: Vec<Stmt>,
    /// Opening brace through closing brace.
    pub span: Span,
}

/// One statement in a [`Block`].
#[derive(Clone, Debug)]
pub enum Stmt {
    /// A local or signal declaration.
    Let(LetDecl),
    /// `target = expr;` — meaning resolved by context (spec 3.12).
    /// `x = v;`, optionally delayed VHDL-style: `clk = !clk after 5ns;`
    /// (`after` is testbench-only in Phase 1; the self-toggle idiom is the
    /// canonical clock generator).
    Assign {
        /// The assigned place: a name, field, index, slice, or concatenation.
        target: Expr,
        /// The value driven onto `target`.
        value: Expr,
        /// `after 5ns` — schedule the write instead of applying it now.
        /// Testbench-only in Phase 1.
        after: Option<Expr>,
        /// Target through the terminating `;`.
        span: Span,
    },
    /// An `if` / `else if` / `else` chain in statement position.
    If(IfStmt),
    /// A `match` in statement position.
    Match(MatchStmt),
    /// `for i in 0..10 { ... }` over a static range (spec Stage 1 / 8).
    For {
        /// The loop variable, bound fresh in each iteration's `body`.
        var: Ident,
        /// The range iterated over. It must be static: loops are unrolled at
        /// elaboration, not executed at run time.
        range: Expr,
        /// The statements repeated per iteration.
        body: Block,
        /// `for` keyword through the closing brace.
        span: Span,
    },
    /// `assert!(cond, "msg");`, `wait 10.ns;`, `tick(clk);` (Stage 8).
    Expr(Expr),
    /// `return;` or `return expr;`.
    Return {
        /// The returned value, or `None` for a bare `return`.
        value: Option<Expr>,
        /// `return` keyword through the terminating `;`.
        span: Span,
    },
}

/// An `if` statement and the head of any `else` chain hanging off it.
#[derive(Clone, Debug)]
pub struct IfStmt {
    /// The tested condition, read through the `Condition` trait.
    pub cond: Expr,
    /// Statements run when `cond` holds.
    pub then: Block,
    /// Optional `else` / `else if` chain.
    pub else_: Option<Box<ElseBranch>>,
    /// `if` keyword through the end of the last branch.
    pub span: Span,
}

/// What follows an `else`: a final block, or another `if` continuing the chain.
#[derive(Clone, Debug)]
pub enum ElseBranch {
    /// A terminal `else { ... }`.
    Block(Block),
    /// An `else if ...`, which may itself carry a further `else`.
    If(IfStmt),
}

/// A `match` in statement position.
#[derive(Clone, Debug)]
pub struct MatchStmt {
    /// The value being matched.
    pub scrutinee: Expr,
    /// The arms, tested in source order; the first match wins.
    pub arms: Vec<MatchArm>,
    /// `match` keyword through the closing brace.
    pub span: Span,
}

/// One `pattern => body` arm of a match.
#[derive(Clone, Debug)]
pub struct MatchArm {
    /// The pattern selecting this arm.
    pub pattern: Pattern,
    /// The arm's body. In a match *expression* this is a single expression;
    /// see [`MatchArm::value_expr`].
    pub body: Block,
    /// Pattern through the end of the body.
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
    /// `_` — matches anything and binds nothing.
    Wildcard,
    /// An enum variant path such as `State::Idle`.
    Path(Path),
    /// A bit pattern such as `"01--"`, where `-` and `?` are don't-cares.
    BitPattern {
        /// The pattern text between the quotes, without the quotes.
        text: String,
        /// The literal's extent, including its quotes.
        span: Span,
    },
    /// `A | B | C` — matches if any alternative matches (spec 3.22).
    Or {
        /// The alternatives, tried in order.
        alts: Vec<Pattern>,
        /// First alternative through last.
        span: Span,
    },
    /// An integer literal (`5`) or inclusive range (`0..9`) pattern for a
    /// numeric scrutinee; a bare literal is `lo == hi`.
    Range {
        /// Inclusive lower bound.
        lo: i64,
        /// Inclusive upper bound; equal to `lo` for a bare literal.
        hi: i64,
        /// The pattern's extent.
        span: Span,
    },
    /// A character literal (`'0'`, `'Z'`) naming a variant of a char-valued
    /// enum — `Logic` above all. Like the expression form it has no intrinsic
    /// value: the variant it selects comes from the scrutinee's type.
    CharLit {
        /// The character between the single quotes.
        ch: char,
        /// The literal's extent, including its quotes.
        span: Span,
    },
}

/// An expression. Every variant carries a [`Span`]; [`expr_span`] reads it
/// without matching each shape by hand.
#[derive(Clone, Debug)]
pub enum Expr {
    /// An integer literal, possibly with a radix prefix (`0x`, `0b`, `0o`).
    Int {
        /// The literal exactly as written, prefix and digit separators
        /// included; it is parsed later so diagnostics can quote the source.
        text: String,
        /// The literal's extent.
        span: Span,
    },
    /// `1ns`, `10MHz`, `5i` — a numeric literal with an adjacent unit/type
    /// suffix. `text` is the numeric part exactly as written.
    SuffixLit {
        /// The numeric part exactly as written, without the suffix.
        text: String,
        /// The suffix identifier, resolved against std's `Suffix` impls.
        suffix: Ident,
        /// Number through suffix.
        span: Span,
    },
    /// `x"123ABC"` / `o"17"` — a radix bit-string literal; `base` is the
    /// prefix letter (validated against std's `impl Prefix`), `digits` the
    /// text between the quotes. (A plain string is `StrLit`, not this.)
    BitStrLit {
        /// The prefix letter: `x` for hex, `o` for octal, and so on.
        base: char,
        /// The digit text between the quotes.
        digits: String,
        /// Prefix letter through closing quote.
        span: Span,
    },
    /// A single character between single quotes (`'g'`, `'0'`). A character
    /// literal has no intrinsic value — its type (and so its numeric value)
    /// comes from context: the enum it is assigned to or compared against.
    CharLit {
        /// The character between the single quotes.
        ch: char,
        /// The literal's extent, including its quotes.
        span: Span,
    },
    /// A double-quoted string. Also how a bit-string of logic values
    /// (`"1X10"`) is written when no radix prefix is needed.
    StrLit {
        /// The text between the quotes, with escapes already resolved.
        text: String,
        /// The literal's extent, including its quotes.
        span: Span,
    },
    /// A name or `::`-qualified path: a local, a constant, an enum variant.
    Path(Path),
    /// `x.field` (spec `.` member access).
    Field {
        /// The value being projected.
        base: Box<Expr>,
        /// The field being selected.
        field: Ident,
        /// Base through field name.
        span: Span,
    },
    /// A VHDL-style attribute tick: `sig'event`, `sig'old`, `data'length`,
    /// `arr'high` (spec 3.9/3.10/3.23). `'` is exclusively for attributes; `::`
    /// is namespace/type selection and `.` is field/method access.
    SysAttr {
        /// The signal or value the attribute is asked about.
        base: Box<Expr>,
        /// The attribute name after the tick, such as `event` or `length`.
        attr: Ident,
        /// Base through attribute name.
        span: Span,
    },
    /// `data[7..0]` slice or `data[0]` index (spec 3.23).
    Index {
        /// The value being indexed.
        base: Box<Expr>,
        /// A single index, or a range for a slice.
        index: Box<Expr>,
        /// Base through the closing bracket.
        span: Span,
    },
    /// `0..10`, `31..0`.
    Range {
        /// The left bound as written; it may exceed `hi` for a descending
        /// range such as `31..0`.
        lo: Box<Expr>,
        /// The right bound as written.
        hi: Box<Expr>,
        /// Left bound through right bound.
        span: Span,
    },
    /// An inclusive range with an omitted bound: `..4`, `1..`, or `..`.
    /// The surrounding indexing operation supplies omitted `left`/`right`
    /// bounds; other contexts diagnose the missing bounds.
    PartialRange {
        /// The left bound, or `None` to take the indexed value's own left.
        lo: Option<Box<Expr>>,
        /// The right bound, or `None` to take the indexed value's own right.
        hi: Option<Box<Expr>>,
        /// The written extent, `..` included.
        span: Span,
    },
    /// A prefix operator application.
    Unary {
        /// Which operator was written.
        op: UnOp,
        /// The operand.
        rhs: Box<Expr>,
        /// Operator through operand.
        span: Span,
    },
    /// An infix operator application, including user-defined operators.
    Binary {
        /// Which operator was written, with its binding power already
        /// resolved for [`BinOp::Custom`].
        op: BinOp,
        /// The left operand.
        lhs: Box<Expr>,
        /// The right operand.
        rhs: Box<Expr>,
        /// Left operand through right operand.
        span: Span,
    },
    /// Rust-style `if c { a } else { b }` as a value (else required; branches
    /// are single expressions). `else if` chains nest in `els`.
    IfExpr {
        /// The tested condition.
        cond: Box<Expr>,
        /// Value when `cond` holds.
        then: Box<Expr>,
        /// Value otherwise. Required: an expression must always have a value.
        els: Box<Expr>,
        /// `if` keyword through the last branch.
        span: Span,
    },
    /// `match s { A => e1, _ => e2 }` in value position — each arm's body is a
    /// single expression (spec 3.22).
    Match {
        /// The value being matched.
        scrutinee: Box<Expr>,
        /// The arms, tested in order; each body is a single expression.
        arms: Vec<MatchArm>,
        /// `match` keyword through the closing brace.
        span: Span,
    },
    /// `f(a, b)` / `read<string>(path)` / `assert!(...)`.
    Call {
        /// What is being called: a path, or a `.method` field access whose
        /// base becomes the receiver.
        callee: Box<Expr>,
        /// Explicit type construction arguments. Phase 1 uses this for
        /// constructor-like intrinsics such as `read<T>`; ordinary generic
        /// functions continue to infer their type parameters from values.
        type_args: Vec<Type>,
        /// The value arguments, in order.
        args: Vec<Expr>,
        /// Whether the call was written with `!`, as in `assert!(...)`.
        /// Macro-shaped intrinsics take their arguments lazily.
        bang: bool,
        /// Callee through the closing parenthesis.
        span: Span,
    },
    /// Instance/struct construction `Counter<W = 8> { .clk = clk, .count = c }`
    /// (spec 3.2/3.12). `ty` is `None` for a name-less struct literal
    /// `{ .valid = '1', .data = 5 }`, whose type comes from the assignment
    /// target's declaration.
    Construct {
        /// The type being constructed, or `None` for a bare `{ ... }` literal
        /// whose type comes from the assignment target.
        ty: Option<Type>,
        /// Field or port connections, explicit or positional.
        args: Vec<ConnectArg>,
        /// Struct spread-update base: `{ ..base, .x = v }` takes every field
        /// from `base` and overrides the ones in `args`. `None` for a plain
        /// literal.
        spread: Option<Box<Expr>>,
        /// Type name through the closing brace.
        span: Span,
    },
    /// Bit concatenation `{a, b, c}` — the first element is the most significant.
    Concat {
        /// The concatenated parts, most significant first.
        parts: Vec<Expr>,
        /// Opening brace through closing brace.
        span: Span,
    },
    /// `[a, b, c]` — an array literal (spec 3.23), one value per element.
    Array {
        /// The elements, in ascending index order.
        elems: Vec<Expr>,
        /// Opening bracket through closing bracket.
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
    /// The named field for `.field = value`; `None` for a positional argument.
    pub field: Option<Ident>,
    /// The connected value. `None` only in error recovery.
    pub value: Option<Expr>,
    /// The argument's own extent.
    pub span: Span,
}

/// A prefix operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    /// Arithmetic negation, `-x`.
    Neg,
    /// Logical or bitwise complement, `not x`. On a vector this lowers to an
    /// exclusive-or against all-ones.
    Not,
}

/// An infix operator as written in the source.
///
/// Dispatch is Rust-shaped (spec 3.25): `a + b` resolves to an
/// `impl Operator<"+", Rhs, Out> for <type of a>`, with the implementation
/// selected by the right-hand operand's type. Siox uses one type-directed
/// contract for both scalar boolean and per-element `and`, so the same variant
/// covers `Bool and Bool` and `Logic[] and Logic[]`. `==`/`!=` stay built-in,
/// or derive from the three-way `<=>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BinOp {
    /// Addition, `+`.
    Add,
    /// Subtraction, `-`.
    Sub,
    /// Multiplication, `*`.
    Mul,
    /// Division, `/`.
    Div,
    /// Conjunction, `and` — scalar boolean or per-element, by operand type.
    /// One of the two core textual binary operators.
    And,
    /// Disjunction, `or`, with the same type-directed meaning as [`BinOp::And`].
    Or,
    /// A library/user-defined textual infix operator. Its binding power comes
    /// from the implementation's `#[precedence = N]` metadata.
    Custom {
        /// The operator symbol as written, such as `xor` or `nand`.
        symbol: String,
        /// Binding power taken from the implementation's `#[precedence = N]`.
        precedence: u8,
    },
    /// Left shift, `<<`.
    Shl,
    /// Right shift, `>>`.
    Shr,
    /// Equality, `==`.
    Eq,
    /// Inequality, `!=`.
    Ne,
    /// Less than, `<`.
    Lt,
    /// Less than or equal, `<=`.
    Le,
    /// Greater than, `>`.
    Gt,
    /// Greater than or equal, `>=`.
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
        /// The element or base type being sized.
        base: Box<Type>,
        /// A width or a range. `None` is the unconstrained form.
        index: Option<Box<Expr>>,
        /// Base type through the closing bracket.
        span: Span,
    },
    /// `Counter<W = 8>`, `Stream<unsigned[32]>` — generic application.
    Generic {
        /// The parameterized type being applied.
        base: Box<Type>,
        /// The arguments inside `<...>`, positional or named but never mixed.
        args: Vec<GenericArg>,
        /// Base type through the closing angle bracket.
        span: Span,
    },
    /// A named view applied to a backing struct: `Source Stream<T>`.
    View {
        /// The view supplying each field's direction.
        view: Path,
        /// The backing struct the view projects.
        target: Box<Type>,
        /// View name through target type.
        span: Span,
    },
}

/// One argument inside `<...>`. Spec 3.2 forbids mixing named and positional.
#[derive(Clone, Debug)]
pub enum GenericArg {
    /// A positional argument still in expression form; later stages decide
    /// whether it was meant as a value or a type.
    Positional(Expr),
    /// An unambiguously type-shaped nested application (`Box<T>` inside
    /// `Outer<Box<T>>`). Bare names and indexed forms remain expressions until
    /// their parameter kind disambiguates them in later stages.
    PositionalType(Type),
    /// `Counter<W = 8>` — a named argument whose value is an expression.
    Named {
        /// The parameter being supplied.
        name: Ident,
        /// The value bound to it.
        value: Expr,
    },
    /// A named argument that is unambiguously a type.
    NamedType {
        /// The parameter being supplied.
        name: Ident,
        /// The type bound to it.
        ty: Type,
    },
}

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

/// The source span of any expression node, without matching each shape at the
/// call site.
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

/// Native scheduler scale for the std-defined physical suffixes: femtoseconds
/// for time units and hertz for frequency units.
///
/// Expression typing and value construction come from `std::sim`'s `Suffix`
/// impls; this table exists only to convert durations at the generated
/// scheduler boundary. `None` for a suffix that is not a physical unit.
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
