# Tuples

Status: **proposal**. Nothing here is implemented.

## Problem

siox has structs but no anonymous product type. Two things are awkward
without one:

- **Several results from one function.** `divmod`, a value with an overflow
  flag (`checked_add` until payload enums land), a sum and its carry: each
  needs a named struct declared for one call site.
- **Matching on several signals at once.** A state machine's next-state logic
  is a decision over `(state, input, count)`. Today that is nested `match`es
  or an `if` chain; VHDL writes it as a `case` over a concatenation, which
  only works when every operand is a bit vector.

## Proposal

A tuple is an anonymous struct whose fields are its positions.

```siox
fn divmod(a: unsigned, b: unsigned) -> (unsigned, unsigned) {
    return (a / b, a % b);
}

let q, r = divmod(x, 7);            // destructuring, no parentheses
let pair: (Bit, unsigned[4]) = ('1', 9);
let low: unsigned[4] = pair.1;      // positional field access

match (state, start, count) {
    (State::Idle, '1', _)   => next = State::Run,
    (State::Run,  _, 0..15) => next = State::Run,
    (State::Run,  _, _)     => next = State::Done,
    _                       => next = State::Idle,
}
```

### Syntax

- **Type:** `(T1, T2, ..)` with two or more elements. `(T)` is just `T`.
- **Value:** `(a, b, ..)`. A parenthesized single expression stays grouping;
  a trailing comma is allowed but not required for two or more.
- **Field access:** `t.0`, `t.1`, as Rust. The lexer must split `t.0.1` into
  field accesses rather than reading `0.1` as a real.
- **Patterns:** `(p1, p2, ..)` where each position is any pattern (`_`, an
  enum variant, a character, a bit pattern, a range, or a nested tuple).
- **Destructuring** needs no parentheses: `let q, r = divmod(a, b);`, with
  per-name types when wanted (`let q: unsigned[4], r: unsigned[4] = ..;`).
  Parentheses appear only for nesting (`let (hi, lo), carry = ..;`). The
  same form assigns existing signals or locals, in a process or as a
  concurrent assignment: `q, r = divmod(a, b);`.

### Meaning

A tuple type lowers to the struct layout it is equivalent to: fields named
`0`, `1`, .. with the element types, in order. Everything a struct already
does follows: flattening into signals in hardware, ports, function returns in
both lowering paths, `'length` of its elements, printing (`(1, 2)`, Rust's
form), and waveforms (`t.0`, `t.1`).

- **Equality** is element-wise, through each element's `Eq`.
- **Assignment** is by position; element types must match exactly, as struct
  fields do (no implicit width changes).
- **Destructuring** `let a, b = e;` evaluates `e` once and binds each
  position; `_` skips one. The right side must be a tuple of that arity. In
  VHDL `signal a, b : bit := '0';` declares two signals with one initial
  value; siox has no such form, so `let a, b: Bit = '0';` is a type error
  (a `Bit` is not a pair), never a silent second meaning.
- **A tuple pattern** in `match` is the conjunction of its positions'
  conditions, so it inherits everything single patterns do: `Match` for bit
  patterns, `Ord` for ranges, structural enum variants. First match wins, as
  for every `match`.
- **Exhaustiveness** is checked per position when every position is an enum,
  `Bool` or a fully covered integer domain; otherwise a `_` arm is required.

### Hardware

A tuple is wires, exactly as a struct: no cost beyond its elements. A tuple
`match` lowers to the same priority chain as any `match`; with disjoint arms
synthesis flattens it to a parallel decoder.

## Not in this proposal

- The unit type `()`. Functions without a value already have no return type.
- Destructuring in function parameters (`fn f((a, b): (A, B))`).
- Single-element tuples `(T,)`.
- Tuple structs with a name (`struct Point(integer, integer)`): siox already
  has newtype structs for one field; more can follow if wanted.

## Work

Syntax (types, values, `t.0`, patterns, printer and formatter), a tuple type
in the checker (inference of `(a, b)`, compatibility, field typing,
exhaustiveness), lowering through the struct layout, `let` destructuring, and
tuple patterns in both lowering paths. Roughly the size of `Match` plus
expression ranges (sioxc#47).

## Open questions

1. Should `match (a, b)` on two bit vectors also accept a concatenated bit
   pattern, VHDL-style, or keep tuple patterns only?
2. Should tuples be allowed as entity ports, or only inside implementations
   and functions? They work either way; the question is style.
