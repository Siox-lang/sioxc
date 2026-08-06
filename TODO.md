# TODO

Outstanding work for the simulation-first siox compiler, organized by the
layer that owns each change. The compiler is one regular Rust package:

`source → AST → semantic analysis → elaboration → IR → LLVM → output`

Status audited 2026-07-29 against the compiler, standard library, documentation,
the `siox-tests` corpus, and CI.

Legend: 🔴 not started · 🟡 partial / constrained · ✅ implemented and covered.

## AST

Owns source syntax, tokens, parsing, formatting, names, types, and elaborated
hierarchy. Code: `src/syntax/`, `src/resolve.rs`, `src/types.rs`, `src/elab.rs`.

Current baseline:

- ✅ Partial ranges, custom operators/indexing, applied views, generics,
  visibility, persistent expression types, direction checks, and frontend
  diagnostics are implemented.
- ✅ Imports and qualified paths enforce `pub`; `pub using` aliases retain
  visibility.
- ✅ Entity, struct, view, trait, and function generic parameters participate
  in unused-parameter analysis, including uses in a separate `impl`.

Remaining:

- 🟡 **Nested generic type arguments.** Nested bounds and return types parse,
  but an inline argument such as `Box<Box<T>>` needs a typed
  `GenericArg::Type` node and `<` expression/type disambiguation. A type alias
  is the current workaround.
- 🟡 **Partial instance-array diagnostics.** Conditionally elaborated arrays
  work, but reading an element that was not built reaches a downstream unknown
  rather than a direct “instance element was not elaborated” diagnostic. Carry
  declared/built slot metadata in `Hierarchy`.
- 🔴 **Comment-preserving formatting.** The canonical printer intentionally
  declines LSP formatting when comments are present because comment trivia is
  not attached to AST nodes. Preserve trivia and anchor comments before
  enabling those edits.
- 🔴 **Incremental/query interface.** Phase products are explicit and stable,
  but compilation is pass-oriented. Add demand-driven caching only when the
  LSP or a future project tool needs incremental multi-file recomputation.

## IR

Owns the elaborated digital model: signals, processes, drivers, events,
initializers, type/enum metadata, and semantic lints. Code: `src/ir.rs`,
`src/ir.rs`.

Current baseline:

- ✅ Combinational and event-driven processes are distinct; delta cycles,
  derived clocks, initialized/undriven behavior, Logic metavalue companions,
  arbitrary-width initializers/module constants, order-independent constant
  aliases, and per-type word counts are covered.
- ✅ W-P003 unused internal signals, W-P010 combinational loops, W-P011
  undriven outputs/signals, W-P012 unconnected inputs, latch and driver lints
  operate on the normalized design.
- ✅ Scalar and vector IEEE 1076-2019 Logic behavior is represented, including
  wide X/Z storage and propagation.

Remaining:

- 🟡 **Persist complete source layouts.** Expression types reach lowering, but
  named/repeated/composite layout is still partly reconstructed from
  declarations. Store recursive source layouts directly in IR metadata.
- 🟡 **Non-flattened composite sizing.** Hardware structs and arrays flatten to
  leaves today. Any future aggregate IR value must calculate
  `count × element_layout` recursively, with checked arithmetic and cycle
  detection.
- 🟡 **Instance-array build facts.** See the AST item above: carry declared
  bounds and built slots into lowering so an unbuilt read has a precise source
  diagnostic.

## LLVM

Owns native lowering, state layout, optimization, and the word ABI. Code:
`src/llvm/`.

Current baseline:

- ✅ LLVM uses each value’s semantic `iN`; unrelated expressions are not widened
  to the design maximum.
- ✅ Default storage is width-sized. The optional `bitpack` layout packs small
  values, reserves consecutive words for wide values, and stores event flags in
  a dedicated one-bit-per-signal bitset.
- ✅ Native target optimization uses LLVM’s `default<O2>` pipeline and optional
  host SIMD features.
- ✅ Cross-word add/subtract, shifts, comparisons, initializers, high-word
  events, and the unbounded low-word-first ABI are covered.

Remaining:

- 🔴 **`f128`.** The feature is declared but not implemented. Add LLVM `fp128`
  expression lowering, constants/conversions, ABI rules, formatting, and a
  software-runtime path for hosts without scalar quad precision.
- 🟡 **Aggregate layouts.** Consume the persistent recursive IR layouts once
  the IR item above lands; do not infer source aggregate sizing in the backend.
- 🔴 **Optimization measurements.** Add repeatable size/runtime benchmarks for
  default, `bitpack`, and host-SIMD builds so optimizations are justified by
  data rather than only structural tests.

## Output

Owns compiler artifacts and generated native harnesses: objects, metadata,
source/AST/tree/IR/LLVM dumps, test executables, diagnostics, and waveforms.
Code: `src/driver/`.

Current baseline:

- ✅ `sioxc` is compiler-only: one input produces an object, metadata/dump, or
  native `#[test]` executable. It never executes the artifact.
- ✅ Native tests support filtering, assertions, timing/`await`, multiple
  clocks, arbitrary-width stimulus, symbolic values, and deterministic
  reporting.
- ✅ Generated test executables accept `--vcd <path>` and write hierarchy,
  femtosecond timestamps, changed arbitrary-width values, Logic x/z, real
  values, and symbolic enums directly from the native scheduler.

Remaining:

- 🔴 **FST output.** Add compressed waveform output for large simulations using
  the same scheduler-side change points as VCD.
- 🔴 **Runtime file reads.** `read`/`read_to_string` fixtures are currently read
  while building a test executable and baked into it. Add runtime
  `fopen`/`fread` plus dynamic-length string/array ownership.
- 🟡 **Diagnostic source coverage.** Frontend diagnostics carry spans and stable
  codes. Continue threading declaration/source spans into IR-only warnings so
  every late diagnostic can highlight its origin.
- 🔴 **Elaborated RTL design file (Phase 3).** Emit a stable, vendor-neutral
  artifact after hierarchy elaboration and synthesizable-logic normalization.
  Vivado, Quartus, and other vendor adapters should consume it for synthesis,
  place-and-route, and bitstream implementation. It must retain module and
  instance hierarchy, ports and directions, logical ranges/widths, nets,
  registers, combinational and clocked logic, clock/reset metadata, parameters,
  initial values where synthesizable, constraints, and source-name/debug
  mappings. Define a versioned schema plus validation/import tooling so adapters
  do not depend on compiler-internal Rust/IR layouts; HDL and netlist renderers
  can remain separate consumers of the same artifact. This is not a current
  simulation milestone.

## API

Owns stable programmatic boundaries used by editors, project tools, simulators,
and foreign integrations.

Current baseline:

- ✅ `siox-lsp` lives in its own repository and consumes this compiler through
  a Cargo Git dependency without depending on LLVM.
- ✅ The native design ABI exposes reset, settle, and low-word-first signal
  get/set operations.
- ✅ `sioxc` keeps rustc-like scope: project graphs, execution, dependency
  management, and directory-wide testing remain outside the compiler.

Remaining:

- 🔴 **Compiler embedding API.** Stabilize an options/result interface above the
  current driver so an LSP or future Cargo-like tool does not compose internal
  passes itself.
- 🔴 **Multi-file user crates.** Standard-library modules load transitively,
  while user compilation still starts from one source entry. Define module
  discovery and crate boundaries in the future project tool, then expose the
  loaded source set through the compiler API.
- 🔴 **cocotb/VPI-GPI integration.** Build name→handle lookup,
  get/put/force/release, timed callbacks, value-change callbacks, and
  read-write/read-only phase callbacks over the native scheduler ABI.
- 🔴 **Project/test tool.** A future Cargo-like executable should discover
  packages, cache builds, compile directories, run/filter generated tests, and
  coordinate waveform output. None of this belongs in `sioxc`.
- 🟡 **External HDL libraries (Phase 3).** `use <library>` should remain
  language-neutral. A project/backend layer can locate precompiled VHDL,
  Verilog, or vendor libraries; compiling VHDL internally is deferred.

## std

Owns user-visible types, traits, operators, attributes, simulation helpers,
math/text/file services, and reusable hardware models. Code: `std/`.

Current baseline:

- ✅ `std::logic`, `bits`, `ops`, `attrs`, `sim`, `assert`, `math`, `text`,
  `fs`, and the prelude are real siox source.
- ✅ Logic resolution/truth tables, numeric operators, custom operator
  precedence, literal hooks, `New`, conversions, and clock helpers are visible
  as library traits/implementations rather than hidden compiler-only surfaces.
- ✅ The runnable conformance suite lives in
  [Siox-lang/siox-tests](https://github.com/Siox-lang/siox-tests) and is checked
  by CI.

Remaining:

- 🟡 **Library build-out.** Add canonical reusable counters, synchronizers,
  memories, FIFOs, stream adapters, and fixed-point families with executable
  conformance tests.
- 🟡 **API reference.** Keep [`docs/std.md`](docs/std.md) synchronized with each
  exported declaration and clearly label compiler/runtime intrinsics.
- 🔴 **Foreign HDL packages (Phase 3).** Map external library names and entity
  metadata without baking VHDL/Verilog syntax into the siox language.

## Out of scope for the current compiler

- Analogue domains, `across`/`through`, `::ddt`, solvers, and mixed-signal
  bridges.
- Schematic/layout design and place-and-route attributes.
- Vendor synthesis backends and foreign HDL compilation.
- A project/package manager inside `sioxc`.
