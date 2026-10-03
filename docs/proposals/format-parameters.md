# Number formats as type parameters

Status: **proposal**, decided; being implemented.

## Problem

`std::fixed` and `std::float` spell a format as an index range, after VHDL's
`fixed_pkg`: `ufixed[3..-4]` is 4.4, `float[8..-23]` is binary32. That reads
only once you know the convention (negative indices are fraction bits,
`x'high` is the exponent width), and it spends `[...]`, which otherwise means
an array's size or range, on something that is not one.

## Decision

The format is the type's parameters:

| type | parameters | examples |
| --- | --- | --- |
| `float<W, M>` | `W` total bits, `M` mantissa (fraction) bits; the exponent is `W - M - 1` | `float<32, 23>` binary32, `float<16, 10>` binary16 |
| `ufixed<W, F>`, `sfixed<W, F>` | `W` total bits, `F` fraction bits | `ufixed<8, 4>` (4.4), `sfixed<16, 8>` (8.8) |

```siox
let x: float<32, 23> = float<32, 23>(1.5);
let gain: ufixed<8, 4> = ufixed<8, 4>(2.5);
type binary16 = float<16, 10>;
```

Both stay pure std. The mechanism is general, in the language:

- **A struct whose parameters shape its base's index range** —
  `pub struct float<W: integer, M: integer>(Logic[W - M - 1 .. 0 - M]);` —
  is the family `float` with that range: `float<32, 23>` is `float` over
  `[8..-23]`. Impls stay family-wide (`impl Mul<float, float> for float`) and
  read the format through `self'high`/`self'low` as today; a constructor's
  `From` body reads `Self'high`/`Self'low`. Only a base indexed by a *range*
  takes this meaning; `struct Word<N: integer>(Logic[N])` is unchanged.
- The applied form is written with its arguments, in a type and in call
  position (`float<32, 23>(1.5)`); writing the range itself (`float[8..-23]`)
  is an error that names the parameter form, so `[...]` keeps meaning an
  array's size or range.
- A call's explicit generic arguments may be values, not only types
  (`T<32, 23>(x)`), as a type's already may.

## Implementation

The import pass already substitutes generic aliases (`type Pair<T> =
Packet<T>`), values into index expressions included. Such a struct is
indexed as its own generic alias — `float<W, M>` standing for
`float[W - M - 1 .. 0 - M]` — and its declaration becomes the plain family
`float(Logic[])`, so later stages see exactly what they handle today.

## Migration

`std::fixed`, `std::float`, the corpus programs, std.md, language.md and the
paper move to the parameter form.
