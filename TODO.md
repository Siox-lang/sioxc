# TODO

Outstanding work for the simulation-first siox compiler, organized by the
layer that owns each change. The compiler is one regular Rust package:

`source → AST → semantic analysis → elaboration → IR → LLVM → output`

Status audited 2026-08-12 against the compiler, standard library, documentation,
the `siox-tests` corpus, and CI.

Legend: 🔴 not started · 🟡 partial / constrained · ✅ implemented and covered.

Bug-sweep status (2026-08-10): generated assignments are linted after parameter
specialization and loop unrolling, scoped hardware locals lower completely,
nested generic types survive the full pipeline, and concrete instance-array
build facts now reach IR diagnostics. Nested runtime array/field access lowers
to muxes and gated leaf writes, and runtime packed-vector indices preserve
declared labels and Logic metavalues. Native testbench runtime-indexed writes
now cover scalar leaves, struct fields and values, nested dimensions, packed
bits, declared nonzero/descending labels, composite copies/spreads, and
out-of-range no-op writes. Runtime aggregate reads use the documented
last-declared-element fallback in both engines; packed-vector reads retain
their distinct zero fallback. Testbench generate-loop lint parity is resolved
without normalization: W-P014 is explicitly scoped to hardware driver
contexts, because sequential testbench writes settle individually and can be
observable.
Native harness locals use an injective private C namespace, so user names and
distinct flattened source paths cannot shadow helpers or one another; linker
paths remain native OS paths rather than requiring UTF-8.
File construction is unified as `read<T>`: `string` selects UTF-8, `integer`
selects raw binary, and packed numeric types reuse that integer path followed
by their ordinary conversion. Scalar, fixed-array, runtime, compile-time ROM,
and arbitrary-width multiword cases are covered.

The first-valid-version policy decisions are resolved: native test fixtures
are runtime inputs, while hardware/top file initializers are compile-time ROM
images. The remaining sections are capability growth beyond that baseline.

## AST

Owns source syntax, tokens, parsing, formatting, names, types, and elaborated
hierarchy. Code: `src/syntax/`, `src/resolve.rs`, `src/types.rs`, `src/elab.rs`.

Current baseline:

- ✅ Partial ranges, custom operators/indexing, applied views, nested generic
  type arguments, visibility, persistent expression types, direction checks,
  and frontend diagnostics are implemented. Custom-operator precedence is
  discovered from the exact transitive import graph before the full parse,
  including local modules without admitting unrelated project files.
- ✅ Imports and qualified paths resolve against their exact module and enforce
  `pub`; loaded modules do not leak names into scope, import collisions are
  rejected, and `pub using` re-exports retain visibility. `sioxc` follows
  ordinary module imports relative to the entry source directory as well as
  `std::` imports relative to `--std`.
- ✅ Container-relative visibility covers module functions, extern functions,
  type-owned struct fields and literals, type-owned inherent methods, private
  entity implementation state, and public-interface privacy.
  Entity ports and trait methods are inherently visible; views explicitly
  expose backing fields. Public entity methods remain rejected until
  cross-hierarchy call/scheduling semantics are designed.
- ✅ Inherent impls are owned by the nominal type's module. Foreign extensions
  use traits; aliases and kernel types cannot gain inherent members; split impl
  blocks share a coherent member namespace, including applied-view identity.
- ✅ Entity, struct, view, trait, and function generic parameters participate
  in unused-parameter analysis, including uses in a separate `impl`.
- ✅ `Hierarchy` retains each concrete instance array's declared and generated
  slots, in declaration order and with the declaration span.

Remaining:

- 🔴 **Comment-preserving formatting.** The canonical printer intentionally
  declines LSP formatting when comments are present because comment trivia is
  not attached to AST nodes. Preserve trivia and anchor comments before
  enabling those edits.
- 🔴 **Incremental/query interface.** Phase products are explicit and stable,
  but compilation is pass-oriented. Add demand-driven caching only when the
  LSP or a future project tool needs incremental multi-file recomputation.
- 🟡 **Public entity methods.** Visibility is parsed and checked, but `pub fn`
  on an entity remains rejected: `instance.method()` has no defined hierarchy,
  scheduling, connectivity, or synthesis semantics. Decide whether such a call
  elaborates into ports/handshake logic or remains unsupported before exposing
  behavioral entity APIs; ordinary struct/view methods are already complete.
- 🟡 **Fully namespaced semantic identities.** Resolution owns declarations by
  `(module, name)`, exposes declaration-site `DefId`s, and type checking now
  keys nominal declarations and free-function contracts by stable identity;
  free-function lowering, const evaluation, extern dispatch, and the native
  test harness use the same resolved identity and allow equal leaves in
  different modules.
  Elaboration and IR now carry entity identity through hierarchy construction,
  generic specialization, recursive lowering, and instance connection lookup.
  Structs, enums, views, aliases, traits/operators, and constants still include
  leaf-keyed tables, and equal-named root entities would still collide in
  emitted signal/test symbols. Those remaining declaration categories stay
  crate-unique until their tables and qualified output identities are lifted.

## IR

Owns the elaborated digital model: signals, processes, drivers, events,
initializers, type/enum metadata, and semantic lints. Code: `src/ir.rs`;
consumed by `src/driver/` and `src/llvm/`.

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
- ✅ Scoped hardware block locals lower to storage-free expressions with
  immediate reassignment, lexical shadowing, conditional selection, aggregate
  fields/elements, packed slices, and event-block next-state separation.
- ✅ Runtime indices traverse nested arrays and intervening struct fields.
  Reads become nested muxes with last-element fallback at each dimension;
  writes become one condition per flattened leaf and match no leaf when an
  index is out of range.
- ✅ Packed-vector positions may be runtime values in hardware, block-local,
  and native testbench code. Nonzero declared labels map onto compact storage;
  writes use arbitrary-width read-modify-write masks, and Logic value and
  metavalue planes update together.
- ✅ Reads and writes through an in-range but conditionally unbuilt instance
  slot produce E-P022 at the reference, label the array declaration, and do not
  misdiagnose unreferenced slots or unresolved generic specializations.
- ✅ Parallel driver contexts fold without warning when their type implements
  `Resolve`; otherwise they are an E-P014 error with contributing source spans.
  The obsolete W-P001 category is retired.
- ✅ Every scalar IR signal retains its source declaration span, including
  flattened aggregate leaves and metavalue companions. Every diagnostic
  emitted by IR lowering has a stable code and primary source span; late
  signal lints anchor to the owning port or `let` declaration.
- ✅ `Design::source_layouts` retains a language-neutral recursive layout for
  every concrete declared value and leaf: nominal structs/views and view-field
  directions, inherited and generic-substituted fields, arrays with ranges,
  packed vector families, enums, kernel scalars, and ranged numeric domains.
  Signal flattening and range/index lowering consume this tree, whose checked
  width/leaf-count operations cannot silently overflow.
- ✅ Testbench-owned values retain the same layouts without becoming hardware
  signals. Native declaration storage, positional aggregate materialization,
  composite choice/copy handling, ranges, scalar families, and widths consume
  those concrete IR layouts. The remaining nominal field-order table contains
  names only and is restricted to source syntax with no concrete value path.

Remaining:

- 🟡 **Non-flattened composite sizing.** Hardware structs and arrays flatten to
  leaves today. Any future aggregate IR value must calculate
  `count × element_layout` recursively, with checked arithmetic and cycle
  detection.
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
- ✅ LLVM obtains each flattened signal width through its persisted
  `SourceLayout` when present. IR validation rejects aggregate layouts at leaf
  signal paths and rejects stale `Signal::width` metadata that disagrees with
  the layout before code generation.
- ✅ Every advertised Cargo feature changes a real compiler boundary: `cli`
  and `llvm` select dependencies/components, `simd` selects host target
  features, and `bitpack` selects the alternate storage layout. Arbitrary-width
  integers are always available; no legacy `wide` or unimplemented `f128` flag
  is exposed.

Remaining:

- 🔴 **Quad precision (future, not advertised).** If a real use case requires
  it, add LLVM `fp128` expression lowering, constants/conversions, ABI rules,
  formatting, and a software-runtime path for hosts without scalar quad
  precision before exposing a Cargo feature or language type.
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
  reporting. User locals are injectively mangled outside the harness namespace,
  and output filenames use native OS strings.
- ✅ Native aggregate stimulus supports runtime-indexed reads and writes across
  declared array labels, nested dimensions, struct fields/values and packed
  bits. Composite right-hand sides are staged before writes; aggregate reads
  fall back to the last declared element at each out-of-range dimension, and
  out-of-range writes are no-ops.
- ✅ Generated test executables accept `--vcd <path>` and `--fst <path>` and
  write hierarchy, femtosecond timestamps, changed arbitrary-width values,
  Logic x/z, real values, and symbolic enums directly from the same native
  scheduler change points. FST uses the embedded, pinned libfst writer and is
  interoperability-tested through its reader.
- ✅ Late lowering diagnostics retain source anchors through IR metadata;
  compile-time file failures use E-P023 and all normalized signal lints point
  at their declarations.
- ✅ Native `#[test]` file services execute in the generated binary. Fixed raw
  arrays preserve declared labels and arbitrary-width little-endian elements;
  runtime-owned UTF-8 strings support dynamic length, indexing, iteration,
  comparison, and formatting. Missing/invalid/oversized fixtures fail the test
  deterministically. Hardware/top file initializers remain compile-time ROM
  images.

Remaining:

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
- ✅ Foreign C declarations have a checked scalar ABI: `real`, signed
  `integer`, and packed numeric values up to one 64-bit word work in
  combinational logic, clocked logic, and native testbenches. Unsupported
  aggregate, multiword, generic, character, and void signatures fail before
  lowering instead of being truncated or dropped.
- ✅ `sioxc` keeps rustc-like scope: project graphs, execution, dependency
  management, and directory-wide testing remain outside the compiler.
- ✅ `siox::compiler` provides the shared disk/in-memory `CompileRequest` →
  `Compilation` boundary. It retains structured diagnostics and partial phase
  products, separates host failures, returns typed text/file artifacts, and is
  used by `sioxc` without requiring LLVM for frontend consumers.

Remaining:

- 🔴 **Multi-file user crates.** Standard-library modules load transitively,
  while user compilation still starts from one source entry. Define module
  discovery and crate boundaries in the future project tool, then expose the
  loaded source set through the compiler API.
- 🔴 **cocotb/VPI-GPI integration.** Build name→handle lookup,
  get/put/force/release, timed callbacks, value-change callbacks, and
  read-write/read-only phase callbacks over the native scheduler ABI.
- 🟡 **General foreign-function ABI.** Define pointer/handle ownership,
  aggregate and multiword layouts, explicit void/side-effect scheduling, and
  platform-aware C scalar widths. Custom library discovery and linker flags
  belong in the future project tool; `sioxc` should consume explicit inputs,
  not discover packages itself.
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
