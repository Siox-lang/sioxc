# TODO

Outstanding work for siox Phase 1. The pipeline runs end to end (parse →
resolve → type-check → elaborate → lower → simulate on interpreter / LLVM JIT /
native, with assertions and VCD waveforms); what remains is filling gaps and
deepening coverage. See [`docs/implementation.md`](docs/implementation.md) for
per-stage status and [`docs/roadmap.md`](docs/roadmap.md) for Phase 2+.

Legend: 🔴 not started · 🟡 partial / has a workaround · 🟢 design known.

## Language features

- 🟡 **Positional name-less struct locals in a testbench** — `let p: Pkt = { 3, 4 }`
  needs the struct's field order, which the runner/native emitter don't track;
  named/shorthand (`.a = x`, `.a`) work. Hardware and entity connections are
  fully covered.
- 🔴 **Nested generics** — `Box<Box<T>>` (generic-argument parsing ambiguity).
- 🔴 **Struct spread-update** — `{ ..base, .x = v }`.
- 🟡 **Partial instance arrays** — an `inst`-array whose elements are only
  conditionally built (`let stage: Inc[3]` with a generate-`if` creating a
  subset) works when the unbuilt elements aren't read; sizing/validation of the
  reserved-but-unbuilt slots is loose.

## Semantics & analysis

- 🟡 **Undriven signals** — a never-driven signal reads `0` rather than raising
  a runtime error or going `'X'`. Real undriven detection needs per-signal
  driven-flag / X-value tracking across all three engines. (Structurally
  unconnected input *ports* are still caught statically, `E-P005`.)
- 🟡 **Full direction analysis** (elab) — reading an undriven `out`, driving an
  `in`, etc. beyond the current write-to-input-port check.
- 🟡 **Cross-module visibility** (resolve) — private items aren't yet enforced
  across modules (single global namespace); value identifiers resolve
  best-effort.
- 🟡 **X/Z propagation through vector arithmetic** — scalar `Logic` is exact
  (std_logic tables + `impl Resolve`); vector ops don't propagate metavalues.
- 🔴 **Cascaded event domains** (sim) — multi-clock event ordering edge cases.

## Engines

- 🟡 **Native emitter — real / char / string testbenches** — `siox test --no-run`
  rejects a testbench that reads a `real`/`Char`/`string` DUT signal
  (`complex_test`, `string_test`); these run on `siox test` (JIT/interp). Native
  is integer-word only for stimulus.
- 🟡 **Native emitter — file I/O expressions** — `std::fs` reads with a string
  path (`fs_test`) aren't emitted to C yet.
- 🟡 **Interpreter — FFI** — `extern "C"` calls need a linked C symbol, which the
  pure interpreter build has no way to resolve (`ffi_test` passes on JIT/native
  only).

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
