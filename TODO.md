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

- 🟡 **Undriven signals** — a never-driven **`out` port** now warns statically
  (`W-P011`, plain non-bus/non-`inout` ports; 0 corpus false positives). Still
  open for **internal signals**: a never-driven `let` reads `0` rather than going
  `'X'`, which needs per-signal driven-flag / X-value tracking in the engine (and
  the runner drives testbench signals invisibly to the IR). Structurally
  unconnected input *ports* are still caught statically (`E-P005`).
- 🟡 **Full direction analysis** — writing an `in` port is now caught in all
  shapes (bare `a = ..`, an `in` bus-mode leaf, and a field/index of a plain
  `in` port `a[3] = ..`/`p.f = ..`, `E-P004`), and a never-driven `out` port now
  warns (`W-P011`). Still open: reading your own `out` port from within the
  entity (allowed today; some HDLs flag it).
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
