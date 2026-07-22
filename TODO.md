# TODO

Outstanding work for siox Phase 1. The pipeline runs end to end (parse →
resolve → type-check → elaborate → lower → run on the LLVM JIT or the native
AOT binary, with assertions and VCD waveforms); what remains is filling gaps and
deepening coverage. See [`docs/architecture.md`](docs/architecture.md) and the CHANGELOG for
per-stage status and [`docs/roadmap.md`](docs/roadmap.md) for Phase 2+.

Legend: 🔴 not started · 🟡 partial / has a workaround · 🟢 design known.

## Language features

- 🟢 **Nested generics** — nested generic **bounds** parse (`fn f<T: Bar<Bit>>`,
  `-> Bar<U>`; the `>>` token splits when closing angle levels). A nested
  generic **type argument** written inline (`Box<Box<T>>`) is the one remaining
  gap and is a **deliberate limitation**: a generic arg is parsed as an
  expression with no node for a nested generic application (supporting it means a
  `GenericArg::Type` variant threaded through ~8 consumers plus `<` type-vs-
  comparison disambiguation — wide for a shape hardware rarely uses). **Workaround:
  a type alias** — `using BoxT = Box<T>; ... Box<BoxT>` compiles cleanly.
- 🟢 **Partial instance arrays** — conditionally-built instance arrays
  (`let stage: Inc[3]` with a generate-`if` building a subset) work when the
  unbuilt elements aren't read. Reading an *unbuilt* slot (`stage[2].y` when only
  `stage[0]` was built) lowers to `Expr::Unknown` and surfaces as a confusing
  downstream error rather than a clear "element not built" diagnostic. Left as a
  **deliberate limitation**: a clear message needs instance-array metadata
  (declared size + built slots) threaded into expression lowering, and warning at
  *build* time would false-positive on the intentional generate-`if` subset — the
  program already fails to compile, only the message is unhelpful.

## Semantics & analysis

- 🟡 **Undriven signals** — **model: always initialized, may be undriven.** Every
  signal/port always holds a value (its `Init` value, see below); "undriven"
  means nothing drives over it, so it keeps that value forever — deterministic,
  never an undefined/error state. Undriven is therefore always a **warning**,
  never an error, and there is **no runtime `'X'` from undriven-ness**: a signal
  undriven on only *some* paths simply holds its init value there (the hold/latch
  case, already `W-P002 POSSIBLE_LATCH`); `'X'`/`'Z'` come only from real
  unknowns. Statically warned today (`W-P011`, 0 corpus false positives) for a
  never-driven **`out` port** and a never-driven **value-less internal `let`** in
  a component entity (excludes `#[test]`/`#[top]` harnesses, instance arrays, and
  initialized `let x = ..` constants). **To reconcile with the model:** a
  structurally unconnected *input* port is currently a hard error (`E-P005`) — it
  should become a **warning** uniform with `W-P011` (an unconnected input is just
  undriven → reads its init value), firing on a *sub-instance's* forgotten input
  but **not** a top-level entity's primary inputs (externally driven, same
  exclusion as `#[top]`/`#[test]`).
- 🟡 **Full direction analysis** — writing an `in` port is now caught in all
  shapes (bare `a = ..`, an `in` bus-mode leaf, and a field/index of a plain
  `in` port `a[3] = ..`/`p.f = ..`, `E-P004`), and a never-driven `out` port now
  warns (`W-P011`). Still open: reading your own `out` port from within the
  entity (allowed today; some HDLs flag it).
- 🟢 **`new` — uninitialized value semantics** — model the default value of an
  undriven signal as the type's nullary constructor `T::new()`, not a hardcoded
  `0`. Naming it `new` (a `New` trait, `fn new() -> Self`) folds "default value"
  and "construction" into one concept rather than a separate `Default`/`Init`;
  the **nullary** `new()` is the value a signal falls back to, while any
  parameterized `new(args)` stays *explicit* construction. Written either
  `T::new()` or **`T()`** — the zero-argument member of the same `T(...)` family
  whose one-argument form `T(x)` is the conversion (§3.28); `T(...)` names the
  *constructor* (a function), not the inert data, consistent with
  `From::from`/`Ord::cmp`/`Boolean::as_bool`. `T()` is implemented (`lower_new`:
  enum → first variant, numeric/vector/`Char`/`real`/`integer` → 0, struct →
  field-wise `Val::Fields`). The *derived default*
  is structural — an enum yields its **first variant** (VHDL `T'LEFT`), a
  `Logic`/`Bit` vector yields all-`'0'` → `0`, a struct/array defaults
  field/element-wise — which unifies "0 for numerics" and "first variant for
  enums" under one recursive rule and fixes undriven enums with a
  **non-zero-based first discriminant** (today they read `0`, not a valid
  variant). Two stages: (1) ✅ **derived default landed** (siox-ir sets an enum
  signal's `init` to its first-variant discriminant via `enum_first_discriminants`;
  non-enum stays `0`; explicit `let x = V` still wins; `language.md` §3.29; 0
  corpus regressions); (2) **`impl New for T` overrides** wait on **trait
  resolution** (the same unlock as `Condition`/`Boolean`) and require the nullary
  body to be a **constant expression** foldable to the `u64` `init`. Note
  in the docs that a type-level default is a *simulation* power-on value, not a
  synthesizable reset (real reset comes from reset logic). Relates to
  **Undriven signals** above (this defines the value; the `'U'`-style runtime
  *visibility* of undriven is a separate `Logic`-domain change).
- 🟡 **Cross-module visibility** (resolve) — private items aren't yet enforced
  across modules (single global namespace); value identifiers resolve
  best-effort.
- 🟡 **X/Z propagation through vector arithmetic** — scalar `Logic` is exact
  (std_logic tables + `impl Resolve`); vector ops don't propagate metavalues.
- 🔴 **Cascaded event domains** (sim) — multi-clock event ordering edge cases.

## Engines

The whole corpus runs on both the LLVM JIT and the **native** AOT binary —
`real` / `Char` / `string` testbenches and `std::fs` reads are all emitted.
Remaining engine-specific notes:

- 🔴 **Native emitter — true runtime file read** — `read_to_string` is read at
  *build* time (fine for the stable fixtures) and baked in. A genuine runtime
  `fopen`/`fread`, for a file that changes between build and run, is a possible
  follow-up; it needs a dynamic-length string local in C.

## Diagnostics & lints (Stage 10)

- 🟡 **Unused signal / parameter** warnings — **fn generic params** warn today
  (`W-P004`). Still open: **unused signals** (needs use-tracking that spans the
  runner — the IR can't see a testbench's reads) and **entity/struct/trait
  generic params** (their decl and `impl` declare the param separately, so a
  param used only in the impl body reads as unused; needs decl↔impl
  unification).
- 🔴 **Suspicious `Logic` compare / reset** lint.

## Waveforms (Stage 9)

- 🔴 **FST output** for large designs (VCD works today).

## Tooling & integration

- 🔴 **cocotb integration** — drive the compiled design via VPI/GPI (the runtime
  ABI is already VPI-shaped for this). Tracked as the main open runtime task.

## Standard library (Stage 11)

- 🟡 **Fill out `std/`** — `std::logic`, `std::bits`, `std::attrs`, `std::sim`,
  `std::assert`, `std::math`, `std::text`, `std::fs` exist but want more
  coverage and the canonical example programs. The `.siox` conformance corpus
  lives in [Siox-lang/siox-tests](https://github.com/Siox-lang/siox-tests).

## Out of scope (Phase 2+, see roadmap)

- Analogue `domain`, `across`/`through`, `::ddt`, solvers, mixed-signal bridges.
- Schematic / design language, layout attributes.
- Synthesis backend.
