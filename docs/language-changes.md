# Language changes since the original spec

[`spec.md`](spec.md) is the original Phase 1 design. As the language is
implemented, some decisions have been refined or replaced. This page tracks the
intentional deviations so the spec's examples don't mislead.

**When this page and `spec.md` disagree, this page is authoritative** for current
syntax and semantics. It is a living document; the spec is the design baseline.

## Operators

### Logical operators are textual (not `&` / `|`)

The spec writes logical and/or as `&` and `|` (e.g. `a & b`,
`self::event & self::old == '0'`). The language now uses **textual operators**:

| Operator | Kind | Notes |
| -------- | ---- | ----- |
| `and`, `nand` | binary | highest of the logical group |
| `xor`, `xnor` | binary | between `and` and `or` |
| `or`, `nor`   | binary | lowest |
| `not`         | unary prefix | logical negation |

So `self::event & self::old == '0' & self == '1'` is written
`self::event and self::old == '0' and self == '1'`.

- **Precedence:** `and`/`nand` > `xor`/`xnor` > `or`/`nor`; comparisons bind
  tighter than all of them; `not` is a prefix. (VHDL/C-aligned.)
- **Why:** reads clearly, matches VHDL/Ada, and is the first use of the
  textual-operator mechanism below. The `&`/`|` characters are now reserved for
  future custom symbolic operators and no longer parse as operators.

### Operators are heading toward overloadable traits

Direction (not yet implemented beyond the logical operators above): operators
will be **string-named traits** — `pub trait "*" infixl 7 { … }` — covering three
tiers:

1. **Standard symbolic** operators (`+ - * / << >> == < >`): built-in.
2. **Textual** operators (`mod`, `rem`, `abs`, user `trait "foo"`): the
   extensibility path. They lex as identifiers, so no lexer changes and no clash
   with `<>` generics.
3. **Symbolic custom** operators: a reserved character pool
   `+ - * / % & | ^ ~ ! ?` — **excluding `< > =`** (owned by generics,
   assignment, and comparison) to avoid ambiguity. Reserved, not implemented.

Proper result types (e.g. `uint[N] * uint[M] -> uint[N+M]`) require associated
types on traits, which siox does not have yet.

### Width rules

- **Assignment and port connections require matching widths** (checked: port
  connections in elaboration, assignments/initializers in the type checker when
  widths are concrete).
- **Arithmetic operators widen**: mixed-width operands are allowed and the result
  takes the wider operand's width (per-operator — e.g. `*` sums widths). This is
  overload-driven and not yet implemented.

## Literals

### Bit-pattern / hex strings are not lexer tokens

The spec treats `b"01??"` (bit pattern, §3.22) and `x"05AB"` (hex) as literal
kinds. They are **not** built-in tokens. A prefixed string lexes as an
identifier glued to a string (`Ident` + `StrLit`) and will be interpreted via a
**string-overload** mechanism (a library feature, not yet implemented).

As a consequence, the `?` wildcard is **not a token** — inside `b"01??"` it is
ordinary string content.

## Conditions

### `if` is trait-driven (`Boolean`), not a hardcoded rule

The spec (§3.16) hardcodes "`Bool`/`Bit` are conditions, `Logic` needs an
explicit comparison." The language generalizes this: a condition's type must
implement the **`Boolean`** trait (`let as_bool(self) -> Bool`).

- `Bit` and `Bool` have built-in impls; `Logic` has none, so it still needs an
  explicit comparison (`== '1'`).
- User types opt in: `impl Boolean for State { … }` makes `if state { … }` legal.
- The canonical declaration lives in [`../std/ops.siox`](../std/ops.siox); the
  compiler seeds the built-in impls until `std/` is loaded.

## Removed

- **`fn` keyword** — functions and methods are declared `let name(self) { … }`.
  `fn` was reserved but never used by the grammar, so it is gone; `fn` is now an
  ordinary identifier.
- **`?` wildcard token** — see [Literals](#literals) above.
