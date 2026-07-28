# TODO

Outstanding work for siox Phase 1. The pipeline runs end to end (parse →
resolve → type-check → elaborate → lower → compile and run as a native AOT
binary, with assertions); what remains is filling gaps and
deepening coverage. See [`docs/architecture.md`](docs/architecture.md) and the CHANGELOG for
per-stage status and [`docs/roadmap.md`](docs/roadmap.md) for Phase 2+.

Status audited 2026-07-24 against the compiler, standard library, package
tests, and `siox-tests` corpus.

Legend: 🔴 not started · 🟡 partial / has a workaround · 🟢 design known ·
✅ implemented and covered.

## Language features

- ✅ **First-class applied views** — `view Name for Struct`, used as
  `Name Struct`; directions live on view fields
  defines a nominal projection over a reusable layout. Views have their own
  inherent/trait impls, flatten onto the backing signals, preserve generic
  substitution, and enforce leaf permissions. Covered by parser/type/IR tests
  and the executable
  `view_bus_test` / `stream_bus_test` corpus examples. Applied views overload
  by backing struct, so `Source Stream` and `Source Queue` are distinct types.
- ✅ **Unified customisable operators** — all overloads use
  `Operator<"symbol", Input, Output>` and `apply`; `and`/`or`/`not` are core
  syntax but use the same type-directed contract. Library operators such as
  `xor`/`nand`/`nor` are ordinary implementations carrying `precedence`.
- ✅ **Partial ranges and extensible indexing** — `[..hi]`, `[lo..]`, and
  `[..]` inherit inclusive bounds from the indexed object's declared
  `'left`/`'right`; `..=` is rejected. Non-intrinsic types may implement
  `Index<I, Output>` and `IndexAssign<I, Value>`, including `Index<Range, _>`.

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

- 🟡 **Persistent typed IR from the checker** — `Typed` is still an empty
  marker and later stages recompute type facts from the AST. Likewise,
  `Ty::is_digital` currently treats every resolved named type as digital rather
  than recursively checking a struct's fields. Correctness checks exist, but
  retaining expression/signal/method types would simplify the LSP and remove
  duplicated inference in elaboration/lowering.

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
- ✅ **Full direction analysis** — writing an `in` port is now caught in all
  shapes (bare `a = ..`, an `in` view leaf, and a field/index of a plain
  `in` port `a[3] = ..`/`p.f = ..`, `E-P004`), and a never-driven `out` port now
  warns (`W-P011`). Still open: reading your own `out` port from within the
  entity. ✅ **Resolved: keep it allowed** — IEEE 1076-2019 (VHDL-2008) permits
  reading an `out` port; only pre-2008 VHDL forbade it, and we align with the
  2019 reference, so no lint. Direction analysis is otherwise complete.
- ✅ **`new` — uninitialized value semantics** — model the default value of an
  undriven signal as the type's nullary constructor `T::new()`, not a hardcoded
  `0`. Naming it `new` (a `New` trait, `fn new() -> Self`) folds "default value"
  and "construction" into one concept rather than a separate `Default`/`Init`;
  the **nullary** `new()` is the value a signal falls back to, while any
  parameterized `new(args)` stays *explicit* construction. Written either
  `T::new()` or **`T()`** — the zero-argument member of the same `T(...)` family
  whose one-argument form `T(x)` is the conversion (§3.28); `T(...)` names the
  *constructor* (a function), not the inert data, consistent with
  `From::from`/`Operator::apply`/`Boolean::as_bool`. `T()` is implemented (`lower_new`:
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
  corpus regressions); (2) ✅ **`impl New for T` overrides landed** — `ir`'s
  `compute_new_defaults` scans `op_impls[("New", T)]` and const-folds the nullary
  `new()` body to the `u64` `init` (no full trait resolution needed). std uses it
  for `New for Bit` → `'0'` and `New for Logic`/`ULogic` → `'U'`, so an undriven
  `Logic` reads `'U'`; threaded to hardware signals + native testbench
  locals. Note
  in the docs that a type-level default is a *simulation* power-on value, not a
  synthesizable reset (real reset comes from reset logic). Relates to
  **Undriven signals** above (this defines the value; the `'U'`-style runtime
  *visibility* of undriven is a separate `Logic`-domain change).
- 🟡 **Cross-module visibility** (resolve) — 🟢 **soft-enforced:** importing a
  non-`pub` item from another module warns (`W-P013 PRIVATE_IMPORT`; std files
  exempt; type aliases exempt until `pub using` carries visibility in the AST) —
  0 corpus false positives. Resolution still uses one global namespace, so a
  *qualified* cross-module reference isn't checked; promoting the import warning
  to a hard error waits on a `pub using` visibility marker and a std `pub` audit.
- 🟡 **Align the logic/value system with IEEE 1076-2019** (`std_logic_1164`) —
  the reference standard. (b) ✅ **Scalar `Logic` widened to the full 9-value
  `std_ulogic`** (`'U','X','0','1','Z','W','L','H','-'`) with the complete
  `std_logic_1164` operator tables + `resolved` resolution — **verified
  exhaustively (333/333 cells) against `nvc`**; `logic_ninevalue_test` guards
  it. (a) 🟢 **X/Z propagation through vectors** — **functionally complete**
  (design: [`docs/proposals/xz-vector-propagation.md`](proposals/xz-vector-propagation.md)).
  A `unsigned` is `Logic[]`, so a metavalue vector carries a per-element
  discriminant **companion** (`$meta`, 4 bits/element, ≤16 elements), made only
  where a metavalue appears — metavalue-free designs stay bit-identical.
  **Working natively**, guarded by `xz_vector_test`/`xz_poison_test`/
  `xz_logical_test`: 9-value contextual bit strings (`"1X10"`); storage +
  per-element reconstruction (`v[i]` reads its `std_ulogic`); `numeric_std`
  **arithmetic** + **relational** poisoning; per-element `std_logic_1164`
  **logical** (`0 and X = 0`, `1 or X = 1`, `not X = X`); propagation through
  copies / port connections / muxes; **VCD** `x`/`z` rendering. **Minor
  follow-ons:** a metavalue literal in *driver* position (`out = "1X10"`) loses
  its disc in the IR `Const` (init-position and all propagation work); vectors
  wider than 16 elements (array companion); width-1 vectors (`unsigned[1]`) element
  typing. Logic-vector literals now use bare contextual strings (`"1X10"`);
  the removed `b"..."` spelling is no longer accepted. `Logic`/`ULogic`
  default to `'U'` through their std `New` implementations, while `Bit`
  defaults to `'0'`.
- ✅ **Cascaded event domains — a register clocked by a derived clock.**
  ✅ **Fixed 2026-07-22.** `sx_settle` is now a bounded **delta-cycle loop**:
  each delta settles combinational logic, computes `event[i] = cur[i] != old[i]`
  (and a `snap`), runs the event blocks with next-state staging, then advances
  `old <- snap` so a change made *in* one delta becomes an edge in the *next* —
  each edge firing exactly once. Comb settles *before* edge detection so a
  comb-driven clock (a port connection `C.clk <- T.clk`) updates first. Derived
  clocks, clock dividers, and ripple counters now simulate (`derived_clock_test`
  in the corpus). `src/llvm/emit.rs` emits the shared `sx_settle` delta loop,
  bounded by a per-call delta cap.

## Native execution

The whole corpus runs as native AOT binaries — `real` / `Char` / `string`
testbenches and `std::fs` reads are all emitted. Remaining backend-specific
notes:

- 🔴 **Native emitter — true runtime file read** — `read_to_string` is read at
  *build* time (fine for the stable fixtures) and baked in. A genuine runtime
  `fopen`/`fread`, for a file that changes between build and run, is a possible
  follow-up; it needs a dynamic-length string local in C.

## Optimizations (lower priority than semantics — finish those first)

Codegen/footprint work, opt-in and Cargo-gated (see
`src/llvm/`). All lower priority than the semantics work
above — none of it blocks correctness, so it waits. (`bitpack`/`simd` and the
`event` bitset are pure speed/size; `wide`/`f128` add capability but are still
opt-in and non-blocking.)

Signal state is stored width-packed by default (a `Bit`/`Logic` takes one byte,
not eight; `unsigned[32]` four, `unsigned[64]` eight).
Composites already flatten to per-leaf signals, each minimally sized (an enum is
`⌈log2(variants)⌉` bits), so structs/arrays/enums pack for free under `bitpack`.

- 🔴 **`event` bitset** — under `bitpack` the `event`/changed-flags array still
  gives each signal a full-width slot for a 1-bit flag. Pack it as a dedicated
  1-bit-per-signal bitset (`⌈N/8⌉` bytes, independent of signal widths) — the
  last real density win. Its own layout since flags are always 1 bit.
- ✅ **`bitpack`** — pack many small signals into shared 64-bit
  words (a `Bit` takes 1 bit, a nibble `Logic` 4), instead of a byte each. Up to
  ~8× smaller state for `Bit`-heavy designs, at the cost of read-modify-write
  stores — a footprint win for huge designs; the default byte layout is faster
  for cache-resident ones. Correctness is covered by the packed/unpacked corpus
  differential.
- ✅ **`simd`** — the AOT `TargetMachine` targets the host
  CPU's native features (AVX / AVX-512 → 256 / 512-bit vector registers) so the
  `-O2` vectorizer can use them for array/vector ops. Off by default the build
  targets a portable baseline (generic x86-64, SSE2 128-bit).
- 🟡 **`wide` — per-type multi-word values** — signals wider than one ABI word
  (`unsigned[128]` / `[256]` / `[512]`). A value's semantic width belongs to
  its own type; the backend must not widen every expression to the widest
  signal in the design. For a repeated/array type, calculate
  `total_bits = element_count * element_type_size_bits`, then allocate
  `ceil(total_bits / largest_ABI_word_bits)` words (with checked arithmetic).
  Struct/array layouts apply this recursively; a view uses its backing
  struct's layout. LLVM carries each logical value as its corresponding `iN`
  and legalizes its arithmetic, while `sx_set_word`/`sx_read_word` split only
  the external ABI representation into low-word-first chunks. ✅ The first
  LLVM and the word ABI now impose no global word-count limit: `words_for`
  derives the required storage from every type's width. `unsigned[128]`
  cross-word carry and `i512` lowering are covered. Remaining work: persist
  source type layouts in the IR instead of
  backend inference, apply recursive element sizing to non-flattened
  composites, define wide C-FFI and `bitpack` behavior, extend runner/waveform
  module-constant evaluation beyond `u128`, wide Logic literal initialization,
  and add borrows, cross-word shifts/slices, comparisons, and high-word-only
  event coverage. LLVM literals, pattern masks, native testbench values, and
  waveform samples are word-vector/arbitrary-width. Structural type walks use
  cycle detection rather than fixed nesting limits.
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

- 🔴 **Native trace ABI** — enable a separate waveform tool by streaming timestamped
  low-word-first signal values from the native test executable to the VCD
  writer. The former tracing path was removed with the JIT.
- 🔴 **FST output** for large designs (VCD works today).

## Tooling & integration

- 🟡 **Documentation/spec synchronization** — `docs/language.md` tracks the
  current grammar, but the Phase-1 examples in `docs/roadmap.md` still contain
  early spellings such as trait `let` methods and symbolic boolean `&`.
  Refresh or clearly label those sketches so they are not mistaken for
  accepted source.
- ✅ **LSP repository split** — the working protocol server and editor tests now
  live in `Siox-lang/siox-lsp`, with this compiler referenced as a Cargo Git
  dependency.
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
