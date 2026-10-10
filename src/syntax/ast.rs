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
    /// Every lint directive in the file (`#[allow(...)]` on an item or
    /// statement, `#![allow(...)]` for the module), with the extent it
    /// governs. The sink applies them to warnings as they are emitted.
    pub lints: Vec<crate::diag::lints::LintDirective>,
    /// The whole file, from `module` to the last item.
    pub span: Span,
    /// The argument tokens of every `name!(…)` call in the file, keyed by the
    /// call's span. The parser also reads a call's arguments as expressions,
    /// which is all a built-in macro needs; a user macro re-reads the tokens
    /// by its parameters' fragment kinds (`syntax::macros`).
    pub macro_args: MacroArgTable,
}

/// Every `name!(…)` call's argument tokens, by the call's span. A macro body
/// can yield several calls with one span (a `for macro` repetition writes the
/// same tokens again), so each span holds its calls in source order; the
/// expansion pass consumes them in the same order.
pub type MacroArgTable = std::collections::HashMap<Span, Vec<MacroArgs>>;

/// Append `from`'s calls to `into`, keeping each span's order.
pub fn merge_macro_args(into: &mut MacroArgTable, from: MacroArgTable) {
    for (span, calls) in from {
        into.entry(span).or_default().extend(calls);
    }
}

/// A token as a macro captures and re-emits it: its kind, the span it was
/// written at, and its text, which hygiene may have renamed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacroToken {
    /// The lexical form.
    pub kind: crate::syntax::token::TokenKind,
    /// Where it was written, in the caller or in the macro's body.
    pub span: Span,
    /// The source text, or the renamed identifier.
    pub text: String,
}

/// The delimiters of a macro invocation's arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroDelim {
    /// `name!(…)`
    Paren,
    /// `name![…]`
    Bracket,
    /// `name!{…}`, which needs no `;` as a statement or item.
    Brace,
}

/// A macro invocation's arguments, inside its delimiters.
#[derive(Clone, Debug)]
pub struct MacroArgs {
    /// Which delimiters enclosed them.
    pub delim: MacroDelim,
    /// The tokens between the delimiters.
    pub tokens: Vec<MacroToken>,
    /// Whether the tokens read as comma-separated expressions; when not, the
    /// call's `args` are empty and only a user macro can accept them.
    pub parsed: bool,
}

/// `pub macro twice($x: expr) { $x + $x }` (siox-paper/docs/language.md §3.30).
#[derive(Clone, Debug)]
pub struct MacroDecl {
    /// Whether it is exported.
    pub is_pub: bool,
    /// The macro's name, invoked as `name!`.
    pub name: Ident,
    /// The parameters, in order.
    pub params: Vec<MacroParam>,
    /// The body's tokens, without the enclosing braces.
    pub body: Vec<MacroToken>,
    /// `macro` (or `pub`) through the closing brace.
    pub span: Span,
}

/// One `$name: kind` macro parameter.
#[derive(Clone, Debug)]
pub struct MacroParam {
    /// The name after `$`.
    pub name: Ident,
    /// What syntax the argument must be.
    pub kind: FragmentKind,
    /// `$xs: expr...`: the last parameter, taking zero or more arguments.
    pub variadic: bool,
}

/// The syntax a macro parameter accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentKind {
    /// One expression, substituted as one operand.
    Expr,
    /// One identifier.
    Ident,
    /// One type.
    Type,
    /// One path.
    Path,
    /// One statement, without its `;`.
    Stmt,
    /// One item.
    Item,
    /// Any balanced tokens.
    Tokens,
}

impl FragmentKind {
    /// The kind a parameter names, or `None` for an unknown one.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "expr" => Self::Expr,
            "ident" => Self::Ident,
            "type" => Self::Type,
            "path" => Self::Path,
            "stmt" => Self::Stmt,
            "item" => Self::Item,
            "tokens" => Self::Tokens,
            _ => return None,
        })
    }

    /// How the kind is written.
    pub fn name(self) -> &'static str {
        match self {
            Self::Expr => "expr",
            Self::Ident => "ident",
            Self::Type => "type",
            Self::Path => "path",
            Self::Stmt => "stmt",
            Self::Item => "item",
            Self::Tokens => "tokens",
        }
    }
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
    /// `use std::logic::Bit;` — an import or a type alias.
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
    /// A module-level attribute binding: `attr top for Chip = true;`.
    AttrBinding(AttrBinding),
    /// A `macro` declaration. The expansion pass removes it.
    Macro(MacroDecl),
    /// A macro invocation in item position, `register_bank!(regs, 4);`. Its
    /// arguments are in [`Module::macro_args`] under `span`; the expansion
    /// pass replaces it with the items it expands to.
    MacroCall {
        /// The macro's name or path.
        path: Path,
        /// The path through the closing delimiter.
        span: Span,
    },
}

/// `use std::logic::{Bit, ...};` or `type Word = unsigned[32];` (spec 3.4).
#[derive(Clone, Debug)]
pub struct Using {
    /// `pub use ...` re-exports the names; `pub type ...` exports the alias.
    pub is_pub: bool,
    /// Whether this brings names in or defines an alias.
    pub kind: UsingKind,
    /// `use`/`type` keyword through the terminating `;`.
    pub span: Span,
}

/// An import (`use`) or a transparent type alias (`type`). The two shared
/// the removed `use` keyword, and still share this node.
#[derive(Clone, Debug)]
pub enum UsingKind {
    /// `use a::b::{c, d};`, `use a::b::C;`, `use Local = a::b::C;`
    Import {
        /// The path shared by every imported name (`a::b`).
        base: Path,
        /// Names taken from `base`. A single-name import has one entry.
        names: Vec<ImportName>,
    },
    /// `type Word = unsigned[32];`, or generic `type Pair<T> = Packet<T>;`
    Alias {
        /// The new name introduced in this module.
        name: Ident,
        /// Generic parameters, substituted at every use.
        params: Params,
        /// The type it stands for. An alias is transparent, not nominal.
        ty: Type,
    },
}

/// One leaf of an import tree, optionally renamed: `C` in `use a::b::C;`,
/// `Local = C` in `use a::b::{Local = C};` (Rust's `C as Local`), `d::C` in
/// `use a::{d::C};` (`via` holds `d`), `self` in `use a::{self}`, or the
/// glob in `use a::*;`.
#[derive(Clone, Debug)]
pub struct ImportName {
    /// Module segments between the import's base path and this leaf, from
    /// nested groups: `[logic]` for `Bit` in `use std::{logic::{Bit}};`.
    pub via: Vec<Ident>,
    /// The name as declared in the module `base::via`; `self` names that
    /// module itself, and `*` (with `glob`) every public name in it.
    pub name: Ident,
    /// The local name it is bound to, when renamed.
    pub local: Option<Ident>,
    /// `use a::*;`: every public name of the module (or variant of the enum).
    pub glob: bool,
    /// Not written: one name a glob brought in, expanded by
    /// `syntax::imports`. It is never reported as an unused import.
    pub expanded: bool,
}

impl ImportName {
    /// The name this import introduces in the importing module.
    pub fn binding(&self) -> &Ident {
        self.local.as_ref().unwrap_or(&self.name)
    }
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
    /// Attributes bound to the port from its entity's implementation
    /// (`attr keep for clk = true;`). Ports carry no `#[...]` of their own.
    pub attrs: Vec<Attr>,
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
    /// An attribute binding: `attr keep for probe = true;` names a member,
    /// `attr precedence = 40;` binds the enclosing implementation (or, for an
    /// attribute declared `for entity`, the entity it implements).
    AttrBinding(AttrBinding),
}

/// A `process { ... }` block: concurrent with other processes, sequential
/// inside. Declarations and structural instances stay outside it.
#[derive(Clone, Debug)]
pub struct ProcessDecl {
    /// Optional VHDL-style label: `receive: process { ... }`. It names the
    /// process for diagnostics and tools and never changes behavior.
    pub label: Option<Ident>,
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
    /// Trait parameters, such as the operand types of `Add<Rhs, Out>`.
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
    /// `= <default>`: the value every target has until a binding says
    /// otherwise, so a read (`x'name`) always answers.
    pub default: Option<Expr>,
    /// `attr` keyword through the terminating `;`.
    pub span: Span,
}

/// `attr keep for probe = true;` (named) or `attr precedence = 40;`
/// (objectless): metadata attached to a declaration from outside it.
#[derive(Clone, Debug)]
pub struct AttrBinding {
    /// The attribute being bound, resolved against `attr` declarations.
    pub name: Path,
    /// The declaration it binds; `None` binds the enclosing item.
    pub object: Option<Ident>,
    /// The bound value: a literal of the attribute's declared type.
    pub value: Expr,
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
    /// Written as `#[...]`: a compiler directive, built in and resolved by
    /// name, never an `attr` declaration. `false` for metadata bound with
    /// `attr name for x = v;`.
    pub directive: bool,
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
    /// What `#[inline]` asked for. It shapes only the simulation the
    /// compiler builds; a function's hardware meaning never depends on it.
    pub inline: Inline,
}

/// Whether a call to a function is expanded at the call site or calls one
/// shared copy of its body, in the simulation the compiler builds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Inline {
    /// No directive: the compiler inlines small functions and calls large ones.
    #[default]
    Auto,
    /// `#[inline]`: a hint to inline.
    Hint,
    /// `#[inline(always)]`: expand at every call site.
    Always,
    /// `#[inline(never)]`: call one shared copy.
    Never,
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
    /// An import scoped to the rest of its block: `use std::math::max;`.
    Use(Using),
    /// `target = expr;` — meaning resolved by context (spec 3.12).
    /// `x = v;`, optionally delayed VHDL-style: `clk = !clk after 5ns;`
    /// (`after` is testbench-only in Phase 1; the self-toggle idiom is the
    /// canonical clock generator).
    Assign {
        /// Optional VHDL-style label: `sum: y = a + b;`. It names the
        /// assignment for diagnostics and never changes behavior.
        label: Option<Ident>,
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
        /// Optional label on a structural loop: `stages: for k in 0..2 { ... }`
        /// names a hierarchy scope with one child per iteration.
        label: Option<Ident>,
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
    /// Optional label on a structural `if`: `tap: if DEBUG { ... } else { ... }`
    /// names one hierarchy scope, filled by whichever branch is taken. Only
    /// the head of an `else if` chain carries one.
    pub label: Option<Ident>,
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
    /// A range whose bounds are not both integer literals: constants
    /// (`0..DEPTH - 1`), typed literals (`10ns..20ns`, `-1.5..1.5`), or an
    /// open end (`..7`, `8..`). Matches through the scrutinee's `Ord`; a
    /// single expression (`(N - 1)`) is `lo == hi`. Either order is the same
    /// set of values.
    Bounds {
        /// Inclusive bound written first, or none for `..hi`.
        lo: Option<Box<Expr>>,
        /// Inclusive bound written second, or none for `lo..`.
        hi: Option<Box<Expr>>,
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
        /// Explicit generic arguments, types or values: `read<string>(path)`,
        /// and a parameterized type's constructor `float<32, 23>(x)`.
        /// Ordinary generic functions infer their parameters from values.
        type_args: Vec<GenericArg>,
        /// The `Self` type of an associated call written with type
        /// arguments, `ufixed<6, 2>::resize(x)`: `ufixed<6, 2>`, while
        /// `callee` names the function (`ufixed::resize`). Inside the
        /// function `Self'high`/`'low` describe this type.
        qualifier: Option<Box<Type>>,
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
/// `impl Add<Rhs, Out> for <type of a>`, with the implementation
/// selected by the right-hand operand's type. Siox uses one type-directed
/// contract for both scalar boolean and per-element `and`, so the same variant
/// covers `Bool and Bool` and `Logic[] and Logic[]`. Comparisons call
/// `Eq`/`Ord` methods where the operand type has them, and are built in
/// otherwise.
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
    /// Remainder, `%`: the dividend's sign, as Rust's and Verilog's `%` and
    /// VHDL's `rem`.
    Rem,
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
    /// per type (`And<Logic, Logic> for Logic`), so they return
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
                | BinOp::Rem
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

impl GenericArg {
    /// The type of a type-shaped argument; `None` for a value.
    pub fn type_ref(&self) -> Option<&Type> {
        match self {
            GenericArg::PositionalType(ty) | GenericArg::NamedType { ty, .. } => Some(ty),
            _ => None,
        }
    }

    /// The argument read as a type: a type-shaped one as is, a bare or
    /// indexed name (`string`, `unsigned[8]`) as the type it spells. `None`
    /// for a value such as `32`.
    pub fn as_type(&self) -> Option<Type> {
        fn expr_type(expression: &Expr) -> Option<Type> {
            match expression {
                Expr::Path(path) => Some(Type::Path(path.clone())),
                Expr::Index { base, index, span } => Some(Type::Indexed {
                    base: Box::new(expr_type(base)?),
                    index: Some(index.clone()),
                    span: *span,
                }),
                _ => None,
            }
        }
        match self {
            GenericArg::PositionalType(ty) | GenericArg::NamedType { ty, .. } => Some(ty.clone()),
            GenericArg::Positional(value) | GenericArg::Named { value, .. } => expr_type(value),
        }
    }
}

/// The source span of a statement.
///
/// Used to attribute lowered native code back to the line that produced it, so a
/// debugger and a runtime failure both name the source rather than the
/// intermediate the compiler emitted.
pub fn stmt_span(s: &Stmt) -> Span {
    match s {
        Stmt::Use(u) => u.span,
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

/// The standard operator symbols, which carry built-in precedence and have
/// named traits. Any other symbol (a user operator like `xor`) is a
/// `CustomOperator` and must declare its precedence.
pub fn is_builtin_operator(sym: &str) -> bool {
    operator_symbol_trait(sym).is_some()
}

/// The standard operators' named traits (`core::ops`, spec 3.25): trait,
/// symbol, and the method the impl provides. `Not` and `Neg` (unary `-`,
/// after `Sub` so `-` names `Sub` in diagnostics) are unary. User operators
/// are `CustomOperator<"sym", Rhs, Out>` with `apply` instead.
pub const OPERATOR_TRAITS: &[(&str, &str, &str)] = &[
    ("Add", "+", "add"),
    ("Sub", "-", "sub"),
    ("Mul", "*", "mul"),
    ("Div", "/", "div"),
    ("Rem", "%", "rem"),
    ("Shl", "<<", "shl"),
    ("Shr", ">>", "shr"),
    ("And", "and", "and"),
    ("Or", "or", "or"),
    ("Not", "not", "not"),
    ("Neg", "-", "neg"),
];

/// The symbol a standard operator trait answers (`Add` -> `+`).
pub fn operator_trait_symbol(trait_name: &str) -> Option<&'static str> {
    OPERATOR_TRAITS
        .iter()
        .find(|(name, _, _)| *name == trait_name)
        .map(|(_, symbol, _)| *symbol)
}

/// The named trait of a standard operator symbol (`+` -> `Add`).
pub fn operator_symbol_trait(symbol: &str) -> Option<&'static str> {
    OPERATOR_TRAITS
        .iter()
        .find(|(_, sym, _)| *sym == symbol)
        .map(|(name, _, _)| *name)
}

/// The trait an impl of `symbol` names, for diagnostics: `Add<Rhs, Out>`,
/// `Not<Out>`, or `CustomOperator<"xor", Rhs, Out>`.
pub fn operator_impl_form(symbol: &str) -> String {
    match operator_symbol_trait(symbol) {
        Some("Not") => "Not<Out>".to_string(),
        Some(named) => format!("{named}<Rhs, Out>"),
        None => format!("CustomOperator<\"{symbol}\", Rhs, Out>"),
    }
}

/// The method a standard operator trait's impl provides (`Add` -> `add`).
pub fn operator_trait_method(trait_name: &str) -> Option<&'static str> {
    OPERATOR_TRAITS
        .iter()
        .find(|(name, _, _)| *name == trait_name)
        .map(|(_, _, method)| *method)
}

/// Symbols the grammar reserves for the language itself — assignment, paths,
/// ranges, separators, brackets, attributes — so a `CustomOperator<sym, …>` impl
/// cannot claim them (spec 3.25). The six comparisons are reserved too: they
/// are `Eq`/`Ord` methods, so implement those instead. An empty
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

/// Whether `sym` is one of the six comparison operators.
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
