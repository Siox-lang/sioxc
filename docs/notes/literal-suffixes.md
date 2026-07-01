# Proposal: std-defined literal suffixes

Status: **draft for approval**. `1ns` / `10MHz` / `x"AB"` already work
(spec 3.24) with a fixed compiler table scaling to integer fs/Hz, and
`std::math::Complex` addition works via operator traits — but `5i` as a
*literal* still needs a way for std to say "suffix `i` builds a Complex".

## Proposed syntax

A suffix declaration is a named constructor, sugar-free and inlined exactly
like operator impls (spec 3.25):

```siox
// std/math.siox
pub suffix i(v: integer) -> Complex {
    return Complex { .re = 0, .im = v };
}

// std/sim.siox (would replace the compiler's fixed fs table)
pub struct Time { fs: integer }
pub suffix ns(v: integer) -> Time {
    return Time { .fs = v * 1000000 };
}
```

`10 + 5i` then lowers as `"+"(10, i(5))` — requires `impl "+"` for
integer+Complex, i.e. *mixed-operand* operator impls (today impls are
`Self × Self`). That is the real design fork:

- **(A) Homogeneous operands only (today).** `5i` works, `z + w` works, but
  `10 + 5i` must be written `Complex { .re = 10, .im = 0 } + 5i`. Cheap.
- **(B) Mixed operands.** Trait fns may take a different rhs type
  (`fn apply(self, rhs: integer) -> Complex` under the same `trait "+"`),
  and integer literals coerce through a suffix-typed operand's impl set.
  This is what makes `10 + 5i` literal. Moderate: impl lookup keys become
  (op, lhs type, rhs type).

Recommend **(B)** — it is the form you asked for (`10 + 5i`), and the same
mechanism gives `uint[8] + integer` a principled home when the shim retires.

## Open questions

- Approve `pub suffix <name>(v: integer) -> T { ... }` as the declaration
  form? (Alternative: a `trait "suffix i"` spelling to reuse trait machinery,
  but a suffix has no `self`, so a dedicated item reads more honestly.)
- Keep time/frequency suffixes compiler-fixed until `Time`/`Freq` structs
  exist, or move them to std in the same change?
- Float suffixes (`1.5ns`): scale then truncate to integer fs, or reject?
