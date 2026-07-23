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
  initialized `let x = ..` constants). ✅ **Reconciled with the model:** a
  structurally unconnected *sub-instance* input is now a **warning** (`W-P012`,
  "holds its default value"), not the old hard error `E-P005` (retired) — an
  unconnected input is just undriven → reads its init value (§3.29). Top-level
  primary inputs are unaffected (they aren't instantiated).
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
- 🟡 **Align the logic/value system with IEEE 1076-2019** (`std_logic_1164`) —
  the reference standard. (b) ✅ **Scalar `Logic` widened to the full 9-value
  `std_ulogic`** (`'U','X','0','1','Z','W','L','H','-'`) with the complete
  `std_logic_1164` operator tables + `resolved` resolution — **verified
  exhaustively (333/333 cells) against `nvc`**; `logic_ninevalue_test` guards
  it. (a) **Still open: X/Z propagation through vector arithmetic** — `uint`/
  `int` are stored as 2-value words, so metavalues don't propagate through
  vector `+`/`-`/…; needs a per-bit metavalue representation in the engine (the
  big one). Note: siox keeps `'0'` (not the standard's `'U'`) as the uninitialized
  default via `new`/first-variant — a separate decision if `'U'` is wanted.
- 🟢 **Cascaded event domains — a register clocked by a derived clock.**
  ✅ **Fixed 2026-07-22.** `sx_settle` is now a bounded **delta-cycle loop**:
  each delta settles combinational logic, computes `event[i] = cur[i] != old[i]`
  (and a `snap`), runs the event blocks with next-state staging, then advances
  `old <- snap` so a change made *in* one delta becomes an edge in the *next* —
  each edge firing exactly once. Comb settles *before* edge detection so a
  comb-driven clock (a port connection `C.clk <- T.clk`) updates first. Derived
  clocks, clock dividers, and ripple counters now simulate (`derived_clock_test`
  in the corpus, JIT + native). One change in `siox-llvm/emit.rs` — both engines
  call the same `sx_settle`. Bounded by a per-call delta cap.

## Engines

The whole corpus runs on both the LLVM JIT and the **native** AOT binary —
`real` / `Char` / `string` testbenches and `std::fs` reads are all emitted.
Remaining engine-specific notes:

- 🔴 **Native emitter — true runtime file read** — `read_to_string` is read at
  *build* time (fine for the stable fixtures) and baked in. A genuine runtime
  `fopen`/`fread`, for a file that changes between build and run, is a possible
  follow-up; it needs a dynamic-length string local in C.

## Optimizations (lower priority than semantics — finish those first)

Codegen/footprint work, opt-in and Cargo-gated (see
`crates/siox-llvm/Cargo.toml`). All lower priority than the semantics work
above — none of it blocks correctness, so it waits. (`bitpack`/`simd` and the
`event` bitset are pure speed/size; `wide`/`f128` add capability but are still
opt-in and non-blocking.)

Signal state is stored width-packed by default (a `Bit`/`Logic` takes one byte,
not eight; `uint[32]` four, `uint[64]` eight), shared by the JIT and AOT.
Composites already flatten to per-leaf signals, each minimally sized (an enum is
`⌈log2(variants)⌉` bits), so structs/arrays/enums pack for free under `bitpack`.

- 🔴 **`event` bitset** — under `bitpack` the `event`/changed-flags array still
  gives each signal a full-width slot for a 1-bit flag. Pack it as a dedicated
  1-bit-per-signal bitset (`⌈N/8⌉` bytes, independent of signal widths) — the
  last real density win. Its own layout since flags are always 1 bit.
- 🟢 **`bitpack`** *(implemented)* — pack many small signals into shared 64-bit
  words (a `Bit` takes 1 bit, a nibble `Logic` 4), instead of a byte each. Up to
  ~8× smaller state for `Bit`-heavy designs, at the cost of read-modify-write
  stores — a footprint win for huge designs; the default byte layout is faster
  for cache-resident ones. Correctness is covered by the corpus differential
  (all 61 pass identically packed and unpacked).
- 🟢 **`simd`** *(implemented)* — the JIT/AOT `TargetMachine` targets the host
  CPU's native features (AVX / AVX-512 → 256 / 512-bit vector registers) so the
  `-O2` vectorizer can use them for array/vector ops. Off by default the build
  targets a portable baseline (generic x86-64, SSE2 128-bit).
- 🔴 **`wide`** — signals wider than 64 bits (`uint[128]` / `[256]` / `[512]`).
  Feature flag declared; the base compiler is hard-capped at 64-bit. Needs
  wide integers (`i128`/`iN`) threaded through `ir::Expr`/`Signal`, the three
  backends (sized loads/stores + masked arithmetic on the wide types), and VCD.
  Until then a `>64`-bit signal is rejected rather than silently miscompiled.
- 🔴 **`f128`** — quad-precision float (LLVM `fp128`). Feature flag declared;
  needs `make_binary`/`emit` to carry `fp128` and a soft-float path for the
  runner (no native Rust `f128`).

## Diagnostics & lints (Stage 10)

- 🟡 **Unused signal / parameter** warnings — **fn generic params** warn today
  (`W-P004`). Still open: **unused signals** (needs use-tracking that spans the
  runner — the IR can't see a testbench's reads) and **entity/struct/trait
  generic params** (their decl and `impl` declare the param separately, so a
  param used only in the impl body reads as unused; needs decl↔impl
  unification).
- 🟡 **Suspicious `Logic` compare / reset** lint. ✅ **Compare done** (`W-P008`):
  comparing an enum-valued operand (`Bit`/`Logic`/`Bool`/user `enum`) to a bare
  integer literal (`b == 1` instead of `b == '1'`) warns — numeric vectors are
  excluded; 0 corpus false positives (it caught one real `ok == 1` in the
  corpus). Still open: the **reset** lint (`W-P009`) — needs a false-positive-safe
  definition (reset polarity / edge-detecting a level-sensitive reset).

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
