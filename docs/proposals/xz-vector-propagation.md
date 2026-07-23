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

**Full 9-value on vectors** — a `uint`/`int` is `Logic[]`, so each element is
the same `std_ulogic` (`'U','X','0','1','Z','W','L','H','-'`) the scalar
already has. No 4-state reduction; the vector tables are the *same*
`std_logic_1164` / `numeric_std` tables the scalar `Logic` already validated
against `nvc` (333/333), applied element-wise.

- **Logical** (`and/or/xor/not`, per-element `std_logic_1164`): `0 and X = 0`,
  `1 or X = 1`, `1 and X = X`, `not X = X`, … — the scalar table, per element.
- **Arithmetic** (`+ - *`, `numeric_std`): a metavalue in **any** element ⇒ the
  **whole** result is all-`'X'` (numeric_std poisons + warns).
- **Relational** (`== < …`, `numeric_std`): a metavalue operand ⇒ false (+
  warning).
- **`to_integer` / reads**: a vector with any metavalue reads `0` (+ warning);
  the waveform shows each element's metavalue.

## Representation: 9-value elements, bit-sliced

Each vector element is a 4-bit `std_ulogic` discriminant (0–8, the scalar
encoding). Rather than pack 4 bits/element in one word (slow per-element
extraction for word-parallel ops), store a `Logic`-vector of `N` elements as
**4 bit-planes** of `N` bits each — `p0,p1,p2,p3`, where element *i*'s
discriminant is `p3[i]p2[i]p1[i]p0[i]`. This makes every op **word-parallel**
across all `N` elements:

- `is01[i]` (a clean bit) = `~p1[i] & ~p2[i] & ~p3[i]` (disc 0 or 1); the bit
  value is `p0`.
- `anymeta` = `(p1 | p2 | p3) != 0` — drives arithmetic poisoning.
- logical ops = boolean formulas over the four planes (derived from the 9-value
  tables, validated cell-by-cell vs `nvc`).

Scope by type:
- **Scalar `Logic`** — unchanged (its single 4-bit discriminant already carries
  all 9); no planes.
- **`Bit` / `Bit[]`** — 2-value by definition; stays one bit/element, no planes.
- **`Logic`-family vectors** (`uint`/`int`) — get the plane representation.

Storage: `N`-element vector = `4N` bits (4 `N`-bit planes). A `uint[16]` fits a
64-bit word; wider (`uint[32]`/`[64]`) exceeds the 64-bit cap and rides on the
`wide` feature — so **X/Z on wide vectors depends on `wide`**, and the two
land together. The planes are held as companion signals `S$p1/$p2/$p3` (`p0` is
the existing value word), so a metavalue-free vector is `p1=p2=p3=0` and
bit-identical to today.

## Op formulas (planes `p0..p3` per operand)

Element-wise ops are boolean formulas over the planes, one word-parallel
evaluation covering all `N` elements. They reduce to the **same 9-value truth
tables** the scalar `Logic` already carries in std — the vector op is "apply the
scalar cell to every element", so the tables are shared, not re-derived, and the
same `nvc` differential (333/333 cells) guards both.

- `not`: per-element `std_logic_1164` `not` LUT over the planes.
- `and/or/xor/nand/nor/xnor`: the two operands' planes combine via the op's
  9-value LUT (a forced `0`/`1` element clears the metavalue, per the table).
- `add/sub/mul` (`numeric_std`): `anymeta = (a.p1|a.p2|a.p3|b.p1|b.p2|b.p3)!=0`;
  if set, result is all-`'X'` (every element disc 3 → `p0=1,p1=1,p2=p3=0`);
  else the plain 2-value `p0` arithmetic, other planes `0`.
- `eq/lt/…`: `anymeta ? false : (a.p0 op b.p0)`.

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

- **Full 9-value on vectors** (same `std_ulogic` set as the scalar) — the vector
  and scalar tables are one and the same, shared from std.
- Wide 9-value vectors (`uint[>16]`) need the `wide` feature (4 planes ×
  width > 64 bits), so X/Z-on-wide-vectors and `wide` ship together.
- No new undriven-`U` source (siox stays always-initialized), so the practical
  metavalue sources are explicit `'X'`/`'Z'` literals, tristate `'Z'`, and op
  outputs — not undriven signals.
