# Numeric promotion

Status: **phase 1 implemented** (promotion to `real`); phase 2 is a proposal.

## Rule

Numbers mix in arithmetic and comparisons. The result takes the higher rung
of one ladder, and the lower operand converts to it through the `From` impl
std declares for that pair:

```
integer  →  unsigned / signed  →  ufixed / sfixed  →  float<W, M>  →  real
 (exact integers)                    (exact fractions)    (approximate)
```

1. **The higher rung wins**, whichever side it stands on: `u + s` and `s + u`
   have the same type.
2. **Same rung, different sizes: the result covers both**, as VHDL's
   `fixed_pkg` sizes its results (enough integer and fraction bits for each
   operand; a `signed` wide enough for an `unsigned` beside it; the wider
   float format).
3. **Storing stays explicit.** Assignment and connection keep the strict
   width rule, so the place a value takes a hardware format, and pays its
   rounding or logic cost, is still visible:
   `let y: sfixed<8, 4> = sfixed<8, 4>(a + r + u - s);`

`time`, `frequency`, enums and `Logic` are not on the ladder: they are not
plain numbers, and `time + real` stays an error.

## Phase 1 (implemented)

The top rung. A `real` beside a number whose type has `impl From<T> for real`
promotes it; std declares the impl for `unsigned`, `signed`, `ufixed`,
`sfixed` and `float`, and `integer` promotes as before. The checker accepts the
pair (`Checker::promotes_to_real`); lowering converts the lower operand
through the impl before any operator dispatch (`lower_process_real_promotion`
in testbenches, `promoted_real_operands` in hardware) and uses the float
operation. `sqrt(i + r + u - s)` is a `real`.

## Phase 2

The lower rungs and same-rung widening:

- `integer` beside a family with `impl From<integer> for T` converts through
  it, so `s + 1` adds `1.0` in `s`'s format. Today an integer literal beside a
  fixed format reads as raw steps (`s + 1` adds `2^-F`), as it does for
  `unsigned`, where steps and value coincide. Changing it changes the meaning
  of existing code, so it needs its own decision.
- `unsigned/signed` beside `ufixed/sfixed`, and `ufixed/sfixed` beside
  `float`, convert the lower one into the higher one's family.
- Formats on one rung widen to cover both.

The checker's types record a vector's length but not a fixed or float format
(`sfixed<8, 4>` is `sfixed` of length 8), so it cannot compute rule 2's result
format. Phase 2 starts by carrying the format (`'high`/`'low`, or the format
parameters) in the checked type, so the promoted type and the conversion's
target exist before lowering; the conversions themselves are the `From` impls
std already has.
