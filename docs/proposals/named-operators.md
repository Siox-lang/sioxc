# Named operator traits, and `CustomOperator`

Status: **proposal**, decided; being implemented.

## Problem

Every operator is one symbol-parameterized trait,
`impl Operator<"+", Input, Output> for T` with `fn apply`. That made sense
when it covered everything, but:

- **It no longer covers everything.** Comparisons are `core::cmp`'s `Eq`/`Ord`
  (language §3.25), so `Operator` is "arithmetic and logic, plus user
  operators": two different things behind one name.
- **Two precedence rules hide in one trait.** A standard symbol (`+`) has
  grammar-fixed precedence and must not declare one; any other symbol must
  declare `attr precedence = N;`. Nothing in the type says which rule applies.
- **Bounds say nothing.** `fn f<T: Operator>` means "has some operator". A
  generic body (a matrix over any numeric element) needs `T: Add<T, T>`.

## Decision

A closed set gets names; the open set keeps a symbol parameter — the same line
`Suffix`/`Prefix` draw, since literal affixes are an open set.

**Standard operators are named traits in `core::ops`**, as rustc's, each with
a method named after it and found by lang item:

| operator | trait | method |
| --- | --- | --- |
| `a + b` | `Add<Rhs, Out>` | `fn add(self, rhs: Rhs) -> Out` |
| `a - b` | `Sub<Rhs, Out>` | `fn sub(self, rhs: Rhs) -> Out` |
| `a * b` | `Mul<Rhs, Out>` | `fn mul(self, rhs: Rhs) -> Out` |
| `a / b` | `Div<Rhs, Out>` | `fn div(self, rhs: Rhs) -> Out` |
| `a << b` | `Shl<Rhs, Out>` | `fn shl(self, rhs: Rhs) -> Out` |
| `a >> b` | `Shr<Rhs, Out>` | `fn shr(self, rhs: Rhs) -> Out` |
| `a and b` | `And<Rhs, Out>` | `fn and(self, rhs: Rhs) -> Out` |
| `a or b` | `Or<Rhs, Out>` | `fn or(self, rhs: Rhs) -> Out` |
| `not a` | `Not<Out>` | `fn not(self) -> Out` |
| `== !=` / `< <= > >=` | `Eq<Rhs>` / `Ord<Rhs>` (`core::cmp`) | `eq`, `lt`, `le`, … |

- rustc's `BitAnd`/`BitOr` become `And`/`Or`: siox's `and`/`or` are one
  boolean-per-bit family (§3.25), not a bitwise/logical pair, so the trait
  takes the operator's own name.
- `Rhs` and `Out` are explicit, as `Operator`'s were (siox has no associated
  types). Overloads still select by the right operand's type.
- The method can be called directly: `a.add(b)` is `a + b`.

**User operators are `CustomOperator<"sym", Rhs, Out>`** with `fn apply`, and
always declare their precedence:

```siox
impl CustomOperator<"xor", Logic, Logic> for Logic {
    attr precedence = 35;
    fn apply(self, rhs: Logic) -> Logic { … }
}
```

- A standard symbol in `CustomOperator` is an error whose help names its trait
  (`` `+` is a standard operator: implement `Add<Rhs, Out>` ``); a comparison
  names `Eq`/`Ord`; reserved grammar symbols stay errors.
- `xor`, `nand`, `nor`, `xnor` stay custom operators declared in std.
- `Operator` is removed; an impl naming it is an error pointing here.

**Bounds** use the specific trait: `fn maxi<T: Ord<T>>`, `T: Add<T, T>`. The
capability bound `T: Operator` goes away. Kernel scalars and vectors satisfy
the standard traits as built-in capabilities, as today.

**Array lifts** keep their shape: `impl<T: And<T, T>> And<T, T> for T[]`, and
`impl<T: CustomOperator<"xor", T, T>> CustomOperator<"xor", T, T> for T[]`.

## Implementation

The compiler keeps keying operator impls by symbol: collection maps each named
trait's lang item to its symbol (`add` → `+`), as `Eq`/`Ord` already map to
`==`/`<`, so neither IR path changes shape. Precedence discovery in the parser
reads `CustomOperator` impls only.

## Migration

- `core::ops` (`Bool`'s operators, the array lifts), std (`logic`, `bits`,
  `math`, `fixed`, `float`), the corpus programs that implement operators or
  bound on `Operator`, and the compiler's Rust tests.
- language §3.25, std.md, architecture.md; the paper's operator section.
