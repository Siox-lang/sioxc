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

## Representation: an array of element containers

A `Logic`-vector is what its type says — an **array of `std_ulogic` elements**,
each a container sized to hold the element (a **nibble** for a 9-value `Logic`;
one bit for a `Bit`). A `uint[N]` is `N` nibbles laid out as an array, *not* a
single `4N`-bit integer. This matters: it **decouples from `wide`**. `wide` is
for a single integer wider than 64 bits (wide *arithmetic*); the nibble-array is
storage, and every op reads/writes it through the ≤64-bit *value* it represents.

The value/metavalue split per element (nibble = disc 0–8):
- a **clean** element is disc `0`/`1` — its bit value is the low bit;
- **metavalue** = disc ≥ 2.

Ops therefore work on two ≤64-bit words gathered from the array — `val` (the
0/1 bits) and `meta` (1 where an element is a metavalue) — so **arithmetic
stays ≤64-bit and needs no `wide`** for any width the compiler already supports
(`uint[64]` → a 64-bit `val`, a 64-bit add). Only `uint[>64]` needs `wide`, the
same cap as today — X/Z does **not** widen that cap.

Scope by type:
- **Scalar `Logic`** — unchanged (a single nibble already holds all 9).
- **`Bit` / `Bit[]`** — 2-value by definition; one bit/element, no metavalue
  plane.
- **`Logic`-family vectors** (`uint`/`int`) — the nibble-array.

A metavalue-free vector has an all-zero `meta`, so it reads/writes exactly the
same `val` word as today — **bit-identical**, corpus unaffected. (Whether the
nibble array is materialized in memory always, or only for signals a driver can
actually make metavalue, is a storage-sizing question the container-sizing /
`bitpack` work already answers — see [[signal-container-sizing]].)

## Op formulas (per operand: array of nibbles; `val`/`meta` gathered)

Ops read each operand's nibble array as two ≤64-bit words — `val` (element low
bits) and `meta` (1 where an element is disc ≥ 2) — and write the result array
back. They reduce to the **same 9-value tables** the scalar `Logic` already
carries in std — the vector op is "apply the scalar cell to every element", so
the tables are shared, not re-derived, and the same `nvc` differential (333/333
cells) guards both.

- `not`: per-element `std_logic_1164` `not` over each nibble.
- `and/or/xor/nand/nor/xnor`: element-wise via the op's 9-value cell (a forced
  `0`/`1` element clears the metavalue, per the table).
- `add/sub/mul` (`numeric_std`): `anymeta = (a.meta | b.meta) != 0`; if set the
  whole result is all-`'X'` (every nibble ← disc 3); else the plain `val`
  arithmetic (≤64-bit — no `wide`), result nibbles clean.
- `eq/lt/…`: `anymeta ? false : (a.val op b.val)`.

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
- **Independent of `wide`.** A vector is an array of element containers; ops act
  on the ≤64-bit `val`/`meta` gathered from it, so arithmetic stays ≤64-bit at
  every width the compiler already supports. `wide` (single integers > 64 bits)
  is a separate feature; only `uint[>64]` touches both.
- No new undriven-`U` source (siox stays always-initialized), so the practical
  metavalue sources are explicit `'X'`/`'Z'` literals, tristate `'Z'`, and op
  outputs — not undriven signals.
