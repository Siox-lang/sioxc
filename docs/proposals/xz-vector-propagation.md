# X/Z propagation through vectors

Status: **proposed** (greenlit; foundational, staged). Owner: TBD.

## Problem

Scalar `Logic` is a full 9-value `std_ulogic` (`'U','X','0','1','Z','W','L',
'H','-'`) — its discriminant carries the metavalue, so `1 and X = X` etc. work
(`logic_ninevalue_test`). But **vectors** (`uint`/`int`, i.e. `Logic[]`) are
stored as **2-value packed words** (one bit per element). That loses the
per-bit metavalue, so:

- A bit-string literal with a metavalue (`"01X0"`) can't be represented — it
  parses via `u64::from_str_radix` and falls back to `0`.
- Metavalues don't propagate through vector `and`/`or`/`+`/`-`/`==`/…
  (`a + 1` with an `X` bit gives a clean number instead of the standard's
  poisoned result).

Reference: IEEE 1076-2019 `std_logic_1164` (logical, per-bit) + `numeric_std`
(arithmetic/relational).

## Model (what "correct" means)

Reduce vector metavalues to **4-state** (`0/1/X/Z`) — a deliberate reduction of
the 9-value scalar set (the rare `W/L/H/-` collapse to `X` on vectors; `U`
does not arise because siox is *always-initialized*, so there is no undriven-`U`
source — see the Undriven-signals item). Scalar `Logic` keeps all 9.

- **Logical** (`and/or/xor/not`, per-bit, `std_logic_1164` tables): a result
  bit is a metavalue unless a *dominant* operand forces it — `0 and X = 0`,
  `1 or X = 1`, `1 and X = X`, `0 or X = X`, `not X = X`.
- **Arithmetic** (`+ - *`, `numeric_std`): if **any** operand bit is a
  metavalue, the **whole** result is `X` (numeric_std poisons + warns). No
  partial propagation.
- **Relational** (`== < …`): a metavalue operand yields a false / `X` result
  (numeric_std returns false + warning). `==`/`!=` compare metavalue bits by
  identity is *not* std — follow numeric_std: metavalue ⇒ unknown ⇒ false.
- **`to_integer` / reads**: a vector with any metavalue reads as `0` (+ warning
  in a strict mode); the waveform shows the metavalue bits.

## Representation: value word + `xmask` word

Each metavalue-capable vector signal carries a companion **`xmask`**: bit *i*
set ⇔ element *i* is a metavalue. The value word keeps `0/1` (metavalue
positions read `0` in the value word). Two masks (`xmask`, `zmask`) distinguish
`X` from `Z` if/when tristate-on-vectors matters; **stage 1 ships `xmask` only**
(`Z` folds to `X` for arithmetic, which is its numeric_std behaviour anyway).

- **Scalar `Logic`** is unchanged — its 9-value discriminant already encodes the
  metavalue; no `xmask`.
- **`Bit` / `Bit[]`** are 2-value by definition — no `xmask` (a `Bit` can never
  be `X`).
- Only **`Logic`-family vectors** (`uint`/`int`) get an `xmask`.

Storage: a paired hidden signal `S$x` per vector signal `S` (scales to the
64-bit width cap; the width-packed / `bitpack` layouts already handle extra
signals for free). The alternative — a second word interleaved in `S`'s
slot — breaks the 64-bit cap for `uint[64]`, so paired signals win.

## Op formulas (value `v`, mask `x`)

- `not`:            `v' = ~v`,            `x' = x`
- `and(a,b)`:       `x' = (a.x|b.x) & ~( (~a.v&~a.x) | (~b.v&~b.x) )`  (a forced-0 clears)
                    `v' = a.v & b.v & ~x'`
- `or(a,b)`:        symmetric (a forced-1 clears)
- `xor(a,b)`:       `x' = a.x | b.x`,     `v' = (a.v ^ b.v) & ~x'`
- `add/sub/mul`:    `poison = (a.x|b.x)!=0`; `x' = poison ? allones : 0`,
                    `v' = poison ? 0 : (a.v op b.v)`
- `eq/lt/…`:        `poison ? false : (a.v op b.v)`

(All derived from and validated cell-by-cell against `nvc`, the way the 9-value
scalar tables were — 333/333.)

## Where it touches (staging)

1. **Literals + representation** — parse `X`/`Z` (etc.) digits in `b"…"` bit
   strings into `(value, xmask)`; add the `xmask` companion signal in `ir`;
   thread it through `Signal`/lowering. Reads/writes of a plain vector keep
   `xmask = 0`.
2. **Arithmetic poisoning** (`emit`, `run`, `build`) — the simplest, highest-
   value slice; a metavalue operand ⇒ all-`X`.
3. **Per-bit logical** (`and/or/xor/not`) with the mask formulas.
4. **Relational** + `to_integer`/read-as-0.
5. **VCD** — emit `x`/`z` for metavalue bits (the format already has them).

Each stage is behind the existing 9-value test discipline and nvc differential;
the corpus must stay green throughout (vectors with no metavalues are
bit-identical to today, `xmask = 0`).

## Scope / non-goals

- 4-state on vectors, not full 9-value (scalar stays 9).
- No new undriven-`U` source (siox stays always-initialized).
- `Z` on vectors folds to `X` for arithmetic in stage 1; a real `zmask` for
  vector tristate is a later stage if a design needs it.
