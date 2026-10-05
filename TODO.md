# TODO

Outstanding work for the simulation-first siox compiler, organized by the
layer that owns each change:

`source → AST → semantic analysis → elaboration → IR → LLVM → output`

This file tracks active work, not implementation history. Completed migration
details and measurements belong in [`chat.md`](chat.md) and the documents under
[`docs/`](docs/). Phase 1 pipeline status last audited 2026-10-05 against the
compiler, standard library, `siox-tests`, and the local CI gate.

Legend: 🔴 not started · 🟡 partial / constrained.

## Phase 1 exit criteria

Phase 1 is complete when:

- every explicit and implicit hardware/test process lowers once into canonical
  Process IR;
- native objects and native test executables use the same IR-to-LLVM lowering;
- the fixed runtime schedules all processes, time, delta cycles, and host
  services without per-design generated C;
- the default and `bitpack` corpus pass through the fixed Process runtime with
  correct results, diagnostics, time progression, resolved values, and
  VCD/FST output;
- the temporary AST-to-Process adapter is deleted after source constructs
  lower directly into canonical Process IR.

## AST

Owns source syntax, tokens, parsing, formatting, names, types, and elaborated
hierarchy. Code: `src/syntax/`, `src/resolve.rs`, `src/types/`, and
`src/elab.rs`.

- 🔴 **Simulation reachability and target query.** Infer simulation-only status
  transitively through reachable calls and instances. Native simulation accepts
  `extern "C"` and runtime file I/O; future RTL elaboration rejects any such
  operation still reachable after model selection and constant folding. Expose
  the selected target as the std-owned compile-time value
  `std::target: std::Target` with `simulation` and `elaboration` variants.
  Compile-time `read<T>` ROM construction remains a valid elaboration input.
- 🔴 **Comment-preserving formatting.** Attach comment trivia to stable syntax
  anchors before the LSP offers formatting edits on commented source.
- 🔴 **Incremental/query interface.** Add demand-driven caching only when the
  LSP or future project tool needs incremental multi-file recomputation; keep
  the current explicit phase products as the public boundary.
- 🟡 **Public entity receiver methods.** Static public entity functions work.
  Lower `instance.method()` accessors and effectful methods to stable generated
  ports, first with one caller per method/instance, then define arbitration
  before allowing multiple callers. Struct/view methods already work.

## IR

Owns signals, canonical process control flow, initializers, layouts, enum/logic
metadata, derived scheduling forms, and semantic lints. Code: `src/ir/`.

- 🟡 **Complete canonical source-value lowering.** Hardware CFG construction
  now precedes public scheduler decomposition and retains source contexts;
  the scheduler-to-Process importer is deleted. Derived scheduling forms retain
  canonical value IDs and checked sensitivity lists; their LLVM helpers
  delegate to the common Process value emitter, so projection does not expand
  shared DAGs. Inlined function locals now bind arena IDs; representation
  rewrites retain sharing, types, spans and sensitivity. Typed scalar aliases
  freeze their evaluation format before later consumers widen them. The old
  tree-size guard is removed, with binary32 hardware/procedural conformance and
  long source-chain regressions. Block-local stores now retain declared leaf
  layouts and immediate value IDs; inlined call arguments, receivers and return
  selections share IDs. Pure free/method aggregate arguments and returned
  arrays now use scoped concrete shapes and canonical leaves instead of
  substituting caller ASTs. Nested arrays/structs, selected subarrays, checked
  packed projections and directed slices retain their source labels.
  Whole aggregate writes now share recursive canonical leaf mapping across
  combinational signals, clocked signals and immediate block locals; nested
  stores/read selections, directed reindexing, matches and struct spreads keep
  that path. Pure functions borrow their bodies without return-AST rewriting.
  Hardware statement procedures now borrow their bodies too: scoped value
  arguments share canonical IDs, writable arguments capture storage identities,
  dynamic selections and physical slice maps. Nested calls forward places;
  block-local writes remain immediate and signal writes remain staged. Literal
  planes survive binding/rewriting/compaction, and captured foreign results are
  reused within a straight-line LLVM helper/block without caching mutable state
  across calls or CFG boundaries. Array operators now consume canonical
  operands once, recursively pair written positions, and dispatch source-owned
  element implementations without per-element AST expansion. Returned/selected
  arrays, negative labels, contextual packed literals, user struct operators
  and multiword elements use this path. Lookup recognition does not expand a
  shared foreign call when an ordinary slice is not a lookup table.
  Reachability compaction now moves canonical nodes and remaps their operand
  IDs directly, preserving full formats without expression projection/import.
  Source if-expressions and common unary/binary builders (including aggregate
  elements and packed logical operations) construct arena nodes directly with
  selected arithmetic domains and source spans. Logic-literal normalization
  updates canonical leaves in place from std's discriminants without remapping
  IDs or losing formats. Canonical lookup compaction now directly recognizes
  and replaces matching nodes in place, preserving formats, shared indices and
  typed stride boundaries without projection/import. Metavalue reconstruction
  now moves/remaps dependencies and constructs guards and companion reads
  directly, preserving current/old planes, checked offsets and full formats;
  the general source-arena rewrite adapter is deleted. Other constructors and
  private-fragment normalization paths still use the temporary ingress adapter.
  Finalized hardware write/guard roots now enter the source arena before
  reachability compaction; CFG construction consumes IDs only, without its
  separate digital-expression importer. The source-normalization ingress
  remains until the constructors below build canonical values directly.
  Packed/local/persisted reads, checked indices, captured-place reads,
  conditional local stores and scalar/aggregate selections now build canonical
  nodes directly; aggregate `if`/`match` selections share one condition and
  no longer use the tree-building `select_val` helper. Concatenation shifts/joins,
  real-context arithmetic/conditional coercions and local width boundaries now
  use the same arena builders, retaining operand and canonical source anchors.
  Keep value selections
  distinct from raw metadata projections during the remaining migration.
  Complete the remaining ingress migration: resolution/metavalue construction,
  expression construction between these boundaries and compile-time initializer
  normalization still assemble private fragments around those IDs.
  Make every source expression a canonical value at construction, preserving
  concrete layouts, contexts, lookup compaction and staged-write semantics.

## LLVM

Owns exact-width native code generation and the object-side runtime ABI. Code:
`src/llvm/`.

- 🟡 **Complete direct Process IR lowering.** Exact-width scalar and recursive
  packed values, branches, loops, matches, clocks, suspension, delayed writes,
  formatting, assertions, scalar foreign calls, straight-line receiver/free
  procedures, dynamic UTF-8 strings/file probes, and source-defined operator
  impls execute directly today. Remaining executable forms are runtime
  recursion/general call CFGs and non-packed conversions. Unsupported forms
  must continue to fail transactionally before calls or staged writes become
  observable.
  Recursive procedural arrays/structs now retain packed X/Z planes through
  initialization, copies, projections, writes, old snapshots, loop snapshots
  and DUT connections. Source-owned encodings and exact-width companion frames
  share the same recursive layout offsets as value frames; native regression
  coverage includes 128-bit fields, inactive checked branches, recursive
  negative/directed local ranges, alias chains and clean replacements. Input
  companions are reserved before hardware resolution/normalization, including
  when the testbench's initial value is binary; guarded metadata hoists preserve
  write/event activity through multi-driver resolution.
- 🟡 **Move all host services behind the fixed ABI.** Deterministic
  `seed`/`rand`/`randint`/`uniform`, runtime UTF-8 `read<string>`, string
  indexing/length/equality, fixed strings, little-endian `read<integer>` and
  packed scalar/array binary reads (including multiword elements), and `exists`
  use explicit Process IR operations and fixed runtime state. Add general
  runtime-owned dynamic arrays and runtime-computed file-path arguments. LLVM
  emits value semantics; the runtime owns allocation, persistent state, and
  host contact. `tests/runtime_file_io.rs` verifies that runtime fixtures can
  be created after compilation, capacity/UTF-8/index failures are actionable,
  and hardware ROM initialization remains compile-time.
- 🔴 **Quad precision (future, not advertised).** If a real use case requires
  it, add LLVM `fp128` operations, constants/conversions, ABI rules, formatting,
  and a software fallback before exposing a language feature.
- 🔴 **Unnecessary-copy reduction (deferred; not a Phase 1 blocker).** Audit
  generated IR, LLVM and native code for redundant copies/materializations,
  separately from compiler-internal cloning. Measure copy counts and bytes on
  representative aggregate/multiword designs before changing anything; prefer
  shared value IDs, direct destination construction or storage reuse when
  liveness, aliasing and lifetime rules allow. Keep copies needed for staged
  signal writes, old/assignment snapshots, state across suspension and packed
  X/Z companion planes. Verify native results and waveform parity, and report
  measured compile-memory/runtime effects rather than instruction counts alone.
- 🔴 **Optimization measurements.** Maintain repeatable object-size,
  compile-memory, compile-time, and simulation-throughput benchmarks for
  default, `bitpack`, and host-SIMD builds. Structural simplifications alone
  are not evidence of a speedup. Indexed packed writes now use one update per
  value/companion plane rather than enumerating bits; retain structural and
  native range/X/Z regressions plus measured LLVM instruction/memory evidence
  when extending canonical value sharing. Persisted packed reads now use a
  checked shift/extract with shared companion selection, and nonnegative
  contiguous index checks use two bounds comparisons. Continue auditing
  block-local packed read selectors, negative/sparse bounds predicates and
  captured procedure-place candidate expansion; those remain separate sources
  of growth.

## Output

Owns native objects, test executables, metadata/dumps, diagnostics, waveforms,
and future elaborated RTL artifacts. Code: `src/driver/` and `runtime/`.

- 🔴 **Native source debug metadata.** Emit direct DWARF locations and a stable
  signal/process inspection surface from LLVM Process entries. Until that is
  implemented, `sioxc --test -g` fails explicitly; it must never resurrect a
  per-design C translation merely to obtain `#line` metadata.
- 🔴 **Scalable mini-runtime scheduler (Phase 2 optimization).** Build a small
  deterministic runtime that acts like an RTOS for simulation processes. With
  the default thread count of one, language processes are logically concurrent
  but cooperatively time-share one host thread: a ready process runs until a
  scheduler boundary (`wait`, suspension, or completion), then yields so the
  runtime can run the next process and advance delta cycles or simulation time.
  With a user-selected thread count greater than one, split each independent
  ready epoch over that many persistent worker threads without changing the
  language-level execution model or the result of the simulation.

  Replace process-global state with a per-run context and give each process
  exclusively owned continuation/local state. Use worker-local buffers for
  scheduled signal writes, future events, diagnostics, waveform changes, and
  other externally visible effects. Workers synchronize at deterministic delta
  barriers; the runtime then merges and commits buffered effects in stable
  process/instruction order before any process observes the next epoch. Protect
  genuinely shared queues, signal/driver state, host-service state, and output
  sinks with ownership transfer, mutexes, channels, or bounded buffers as
  appropriate—never with unsynchronized shared mutation. Resolution,
  source-order overrides, impure foreign calls, file I/O, and nondeterministic
  host services remain serialized unless independence is proven.

  Add a runtime thread-count setting without making `sioxc` a project runner.
  Verify that one thread and several thread counts produce byte-identical
  results, diagnostics, and VCD/FST traces in default and `bitpack` modes,
  including races on resolved/unresolved signals and simultaneous timed events.
  Enable multiple threads as an opt-in until benchmarks show a real throughput
  gain; process count alone must not imply that parallel execution is faster.
- 🔴 **Elaborated RTL design file (Phase 3).** Emit a stable, versioned,
  vendor-neutral artifact after hierarchy elaboration and synthesizable-logic
  normalization. Preserve hierarchy, ports/directions, ranges, nets/registers,
  combinational and clocked logic, parameters, synthesizable initial values,
  constraints, and source mappings. Vivado, Quartus, HDL renderers, and other
  adapters consume this schema instead of compiler-internal Rust layouts.

## API

Owns stable boundaries used by editors, project tools, simulators, debuggers,
and foreign integrations.

- 🔴 **Multi-file user crates.** Define module discovery and crate boundaries
  in the future project tool, then expose its loaded source set through the
  compiler API. `sioxc` continues to compile an explicit entry/input.
- 🔴 **cocotb/VPI-GPI integration.** Finish the isolated `feature/cocotb`
  experiment: name-to-handle lookup, get/put/force/release, timed and
  value-change callbacks, and read-write/read-only scheduler phases.
- 🟡 **General foreign-function ABI.** Define pointer/handle ownership,
  aggregate and multiword layouts, explicit void/side-effect scheduling, and
  platform-aware C scalar widths. Library discovery and linker flags belong in
  the future project tool; `sioxc` consumes explicit inputs.
- 🔴 **Project/test tool.** Build a Cargo-like executable for package discovery,
  dependency builds, caching, directory-wide test compilation/execution,
  filtering, and waveform coordination. Keep `sioxc` compiler-only.
- 🟡 **External HDL libraries (Phase 3).** Keep `use <library>` language-neutral.
  A project/backend layer locates precompiled VHDL, Verilog, or vendor
  libraries; internal VHDL compilation is deferred.

## std

Owns user-visible types, traits, operators, attributes, simulation helpers,
math/text/file services, and small technology-independent helpers. Code: `std/`.

- 🟡 **Library build-out.** std is the mandatory, vendor-independent base:
  fixed-point families and vectors/matrices next, each with executable
  conformance tests. Memories, FIFOs and stream adapters are IP for vendor
  packages and libraries, not std.
- 🟡 **API reference.** Keep [`docs/std.md`](docs/std.md) synchronized with each
  exported declaration and clearly label compiler/runtime intrinsics.
- 🔴 **Foreign HDL packages (Phase 3).** Map external library names and entity
  metadata without baking VHDL/Verilog syntax into the siox language.

## Out of scope for the current compiler

- Analogue domains, `across`/`through`, `::ddt`, solvers, and mixed-signal
  bridges.
- Schematic/layout design and place-and-route implementation.
- Vendor synthesis backends and foreign HDL compilation inside `sioxc`.
- A project/package manager inside `sioxc`.
