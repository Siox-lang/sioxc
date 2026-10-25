# Split custom operators into binary and unary contracts

Status: **proposal, not implemented**. Written 2026-10-22. Replace the current
binary-only `CustomOperator` with `BinaryOperator` and `UnaryOperator`, using
the same trait-name casing as other core contracts. The implemented syntax
remains documented in
[language.md](https://github.com/Siox-lang/siox-paper/blob/main/docs/language.md#325-operator-traits-rust-style).

This proposal is about custom operators. Named standard traits such as `Add`,
`Sub`, `And`, `Or`, `Not` and `Neg`, and comparison traits `Eq`/`Ord`, remain
unchanged. It does not revive the removed catch-all `Operator` contract.

## Contracts

Declare both contracts in `core::ops`, re-export them through `std::ops` and
the existing operator prelude, and give them separate lang-item identities:

```siox
pub trait BinaryOperator<op: string, Rhs, Out> {
    fn apply(rhs: Rhs) -> Out;
}

pub trait UnaryOperator<op: string, Out> {
    fn apply() -> Out;
}
```

These examples use the proposed
[implicit receiver syntax](implicit-receivers-and-static.md): ordinary `fn`
has an implicit `self`, so `apply()` still has one operand. Neither method is
static. If the operator split lands before that syntax migration, the bodies
temporarily spell the existing receiver parameters as `apply(self, rhs)` and
`apply(self)`. Arity must be explicit in the contract, not guessed from a body.

| Contract | Source expression | Receiver | Other value arguments |
| --- | --- | --- | --- |
| `BinaryOperator<"op", Rhs, Out> for Lhs` | `a op b` | `a: Lhs` | `b: Rhs` |
| `UnaryOperator<"op", Out> for Input` | `op a` | `a: Input` | None |

Proposed lang roles are `binary_operator` and `unary_operator`; the old
`custom_operator` role retires with migration. Resolve canonical declarations,
not unrelated user traits that happen to have the same leaf name. Trait impls
inherit trait visibility and therefore do not add `pub` to `apply`.

### Binary example

```siox
impl BinaryOperator<"clip", unsigned, unsigned[3]> for unsigned {
    attr precedence = 35;

    fn apply(rhs: unsigned) -> unsigned[3] {
        return unsigned[3](self + rhs);
    }
}
// a clip b
```

The receiver is the left operand. The right operand and result may have
different types, and `Out` retains its complete width, range and alias
identity. A chained expression consumes the declared result, not the input's
format. Contextual literals and existing explicit-conversion rules still apply.

### Unary example

```siox
impl UnaryOperator<"flip", Logic> for Logic {
    attr precedence = 100;

    fn apply() -> Logic {
        return not self;
    }
}
// flip value
```

Custom unary operators are **prefix** operators in the first implementation.
No postfix operator syntax is introduced. This is new expression-parser
support, not something today's binary contract already provides.

Unary output is not required to equal the receiver type. For example, a
reduction operator may consume an array and return `Bool`; the compiler must
not automatically lift every unary operator element by element. Existing
blanket array implementations and newtype forwarding can express library-owned
behavior where appropriate.

## Precedence and parsing

Both contracts require `attr precedence = N;`. Precedence stays a source
attribute in the implementation, not a new trait argument or associativity
attribute. Larger values bind more tightly, using the existing precedence
scale and validation. Missing, nonconstant or conflicting declarations produce
diagnostics with the declaration spans.

Parser discovery and semantic registries must distinguish **symbol and arity**:

- `BinaryOperator<"fold", ...>` and `UnaryOperator<"fold", ...>` may coexist.
  Expression position chooses infix versus prefix; operand types then select
  a matching implementation of that contract.
- All visible implementations of a given symbol/arity agree on precedence,
  including different operand types. The two arities may have different
  precedences. Grouping must never depend on type inference or file order.
- Preserve the existing deterministic grouping for equal-precedence binary
  chains. Prefix chains nest on their operand: `flip flip x` is
  `flip (flip x)`. Parentheses make other grouping explicit. No user-facing
  associativity setting is added.
- A custom unary operand is parsed according to its declared precedence, not
  unconditionally as the tightest expression. With unary precedence 100,
  `flip a * b` groups as `(flip a) * b`; with unary precedence 85,
  `wrap a * b` groups as `wrap (a * b)` because multiplication binds at 90.
  The corresponding `wrap a + b` groups as `(wrap a) + b` (addition is 80).
- Calls, member access, indexing, attributes, macro expansion and parenthesized
  operands retain their structural meaning. Built-in `not` and unary `-`
  retain their existing named contracts and grouping.

Reuse discovery over the exact transitive `use` graph, including re-exports;
do not scan unrelated files. Extend its metadata to carry arity and prefix
binding power. Preserve longest valid punctuation matching and source spans.

Prefix **word** operators create a new name-lookup question: `op(x)` may look
like a function call, and `op` may also be a value name. Recommended direction:
visible unary operator words have an explicit prefix role; report ambiguous
unqualified operator/value/function spellings and suggest qualification or a
rename. Qualified ordinary functions remain ordinary calls. Settle and test
this collision policy before landing, rather than guessing from operand types.

## Validation and dispatch

- A binary implementation has exactly one receiver and one explicit `Rhs`
  parameter. A unary implementation has exactly one receiver and no explicit
  value parameters. Both return the declared `Out`. Reject a static `apply`,
  missing method, extra operand or incompatible return signature early.
- Dispatch uses canonical trait identity, symbol, arity and operand types,
  not just the method name `apply`. Shared method spellings must not cause a
  direct call to silently select an unrelated operator; use existing
  trait-qualified disambiguation where needed.
- Preserve grammar-reserved symbols (`=`, `::`, `.`, `..`, brackets, comparisons
  and the other existing reserved forms). Standard symbols use their named
  traits rather than either custom contract. For example, unary `-` uses
  `Neg`, not `UnaryOperator<"-", Out>`.
- A binary-only implementation cannot satisfy a unary use, or vice versa.
  Diagnostics identify the missing contract and the operand span, without
  cascading into unrelated width or scope failures.
- Reuse ordinary function/trait evaluation and the single canonical Process IR
  pipeline for hardware and native simulation. Preserve operand evaluation
  order, once-only evaluation, value layouts, suspension/effect restrictions
  and debug locations. No new per-symbol LLVM operation or runtime evaluator.
- The compiler names contracts and selects implementations; core/std and user
  source own operator behavior, logic truth tables and conversion rules.

## Migration and implementation

1. Add the two core declarations/lang roles, std re-exports and prelude entries.
   Keep named standard operator contracts and their precedence unchanged.
2. Change declaration discovery, syntax AST and pretty-printing to represent
   custom prefix unary expressions as well as infix binary ones. Do not encode
   unary use as a binary call with a dummy operand.
3. Extend resolver/type/lowering registries with explicit arity. Update generic
   bounds, overload selection, constrained output typing, source method lookup,
   operator expansion and diagnostics together. Reuse shared helpers rather
   than copy the binary machinery into a separate lowering pipeline.
4. Migrate every `CustomOperator<op, Rhs, Out>` implementation and bound to
   `BinaryOperator<op, Rhs, Out>`. This includes core Bool operations, std logic
   tables, signed/unsigned implementations and blanket array contracts. Rename
   the contract without rewriting their established semantics.
5. Migrate the actual sibling corpus, embedded Rust fixtures, macro examples,
   formatter/editor-facing metadata and documentation in the same landing.
   Update language.md and the house-rule vocabulary only when implemented.
   Keep this proposal and the receiver migration coordinated without making
   either silently reinterpret an old declaration.
6. Remove the old active contract and role, with a targeted migration diagnostic
   for `CustomOperator`: suggest `BinaryOperator`, not a guessed unary rewrite.
   Do not keep two live aliases that duplicate registration or discovery.

## Acceptance checks

- Binary compatibility after renaming: existing std truth tables, custom words
  and punctuation, heterogeneous operands, generic bounds and lifted arrays.
- Custom unary words/punctuation, chained prefixes, mixed unary/binary use of
  the same symbol, parentheses, and precedence above/below binary operations.
- Prefix use with calls, fields, indexing, attributes and macros; diagnostics
  for operator/name collisions, reserved symbols and a missing arity.
- Declaration discovery through imports/re-exports; conflicting precedences
  within one arity, allowed differences between arities, and source-order
  independence.
- Unary and binary results with explicit widths, positive/negative range
  labels, aliases and different result types; subsequent operator selection.
- Both receiver spellings during coordinated development, but only the chosen
  migrated syntax at release. Reject static/instance and argument-count errors
  in the trait contract rather than during native lowering.
- Both full Rust suites and actual default/bitpack corpora, plus direct native
  ordinary/debug executions with result assertions and VCD parity. Include
  multiword operands, exact X/Z behavior and evaluation-once controls.

## Scope and remaining decision

This replaces the custom binary contract and adds a custom prefix-unary
contract. It does not replace `Add`/`Not`/other named traits, add postfix
operators, alter comparisons, add implicit casts or change entity call
semantics. Prefix word/name collision policy remains an explicit decision
before implementation. Nothing in this document changes today's compiler.
