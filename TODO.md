# TODO

Outstanding work for siox Phase 1. The pipeline runs end to end (parse →
resolve → type-check → elaborate → lower → run on the LLVM JIT or the native
AOT binary, with assertions and VCD waveforms); what remains is filling gaps and
deepening coverage. See [`docs/architecture.md`](docs/architecture.md) and the CHANGELOG for
per-stage status and [`docs/roadmap.md`](docs/roadmap.md) for Phase 2+.

Legend: 🔴 not started · 🟡 partial / has a workaround · 🟢 design known.

## Language features

- 🟡 **Positional name-less struct locals in a testbench** — `let p: Pkt = { 3, 4 }`
  needs the struct's field order, which the runner/native emitter don't track;
  the explicit form `let p: Pkt = { .a = 3, .b = 4 }` works. Hardware and entity
  connections are fully covered.
- 🔴 **Nested generics** — `Box<Box<T>>` (generic-argument parsing ambiguity).
- 🔴 **Struct spread-update** — `{ ..base, .x = v }`.
- 🟡 **Partial instance arrays** — an `inst`-array whose elements are only
  conditionally built (`let stage: Inc[3]` with a generate-`if` creating a
  subset) works when the unbuilt elements aren't read; sizing/validation of the
  reserved-but-unbuilt slots is loose.

## Semantics & analysis

- 🟡 **Undriven signals** — a never-driven signal reads `0` rather than raising
  a runtime error or going `'X'`. Real undriven detection needs per-signal
  driven-flag / X-value tracking in the engine. (Structurally unconnected input
  *ports* are still caught statically, `E-P005`.)
- 🟡 **Full direction analysis** (elab) — reading an undriven `out`, driving an
  `in`, etc. beyond the current write-to-input-port check.
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

- 🟡 **Unused signal / parameter** warnings — needs use-tracking that spans the
  runner (the IR can't see a testbench's reads).
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
