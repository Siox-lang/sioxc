# TODO

Outstanding work for the simulation-first siox compiler, organized by the
layer that owns each change:

`source → AST → semantic analysis → elaboration → IR → LLVM → output`

This file tracks active work, not implementation history. Completed migration
details and measurements belong in [`chat.md`](chat.md) and the documents under
[`siox-paper/docs/`](https://github.com/Siox-lang/siox-paper/blob/main/docs/README.md). Phase 1 completion audited 2026-10-06 against the compiler,
standard library, both 225-program corpora, native VCD/FST results and the full
local CI gate. See the [acceptance evidence](https://github.com/Siox-lang/siox-paper/blob/main/docs/phase1-audit.md). The tasks
below are remaining extensions and later-phase work, not completed history.

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
- 🔴 **Macro expansion identity.** Keep expansion/call-site identity beyond
  token spans so diagnostics in a macro body identify the invocation that
  produced them, including nested expansions. Macro syntax, hygiene,
  repetition, built-ins and runtime call-site reporting are already implemented.
- 🟡 **Compiler foundations followups.** Add UI diagnostic snapshots,
  explanations/structured suggestions, a shared constant evaluator and
  declaration-ID registries. Hook lang-item registration and core loading are
  implemented; residual leaf-name lookups still need removal. See
  [the remaining proposal](docs/proposals/compiler-foundations.md).
- 🔴 **Incremental/query interface.** Add demand-driven caching only when the
  LSP or future project tool needs incremental multi-file recomputation; keep
  the current explicit phase products as the public boundary.
- 🔴 **Tuples.** Anonymous product types, `let` destructuring and
  multi-signal `match`. See [the proposal](docs/proposals/tuples.md).
- 🔴 **Payload enums.** Tagged-union variants, binding patterns,
  `Option`/`Result` and `checked_*`; builds on tuples. See
  [the proposal](docs/proposals/payload-enums.md).
- 🟡 **Public entity receiver methods.** Static public entity functions work.
  Lower `instance.method()` accessors and effectful methods to stable generated
  ports, first with one caller per method/instance, then define arbitration
  before allowing multiple callers. Struct/view methods already work.
  **Deferred** (2026-10-10): a struct method is marked by its `self`
  parameter, while an entity's implementation has no `self`, so entity
  methods would look unlike struct methods. Settle the syntax first.

## IR

Owns signals, canonical process control flow, initializers, layouts, enum/logic
metadata, derived scheduling forms, and semantic lints. Code: `src/ir/`.

The Phase 1 canonical-pipeline migration and exit audit are complete. Preserve
the [ingress invariants](https://github.com/Siox-lang/siox-paper/blob/main/docs/architecture.md#current-process-ir-ingress-boundary)
when extending the IR; executable extensions and optimization measurements are
listed under LLVM and Output below.

## LLVM

Owns exact-width native code generation and the object-side runtime ABI. Code:
`src/llvm/`.

- 🔴 **Loops in nested function bodies.** A function with a `for` loop called
  from an operator body (`impl Div for float`) fails hardware lowering with
  "no value named …" for the caller's locals, and in a testbench a loop that
  shifts a kernel integer (`rest = rest << 1`) widens its inferred width past
  64 bits. Free functions calling looped functions work.
- 🔴 **Runtime recursion and non-packed conversions.** Extend the currently
  supported inline-call and packed-value contract only with explicit frame,
  ownership, conversion and suspension semantics. Recursive runtime calls are
  rejected today; unsupported forms must fail before argument/callee effects
  or staged writes become observable. Keep the existing native transactional
  failure regressions when adding these capabilities.
- 🔴 **Host-service extensions.** Add general runtime-owned dynamic arrays
  and runtime-computed file-path arguments. Keep allocation, persistent state
  and host contact behind the fixed runtime ABI; LLVM emits value semantics,
  not another host-service implementation. Current RNG, UTF-8/string, fixed
  binary-read and file-probe services are implemented and verified; their
  behavior is documented in [interoperability.md](https://github.com/Siox-lang/siox-paper/blob/main/docs/interoperability.md).
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

- 🔴 **Editor macro expansion views.** Expose generated declarations and their
  originating invocation through the compiler API so the separate LSP can
  distinguish expansion output from handwritten source.
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

- 🟡 **Library build-out.** Synchronizers, fixed-point families and initial
  floating-point operators exist. Remaining: floating-point square
  root/subnormals/rounding modes, and binary64 `*`/`/` (their significand
  product and dividend need more than the 64-bit kernel word); optionally
  generic vectors/matrices. Each needs
  executable conformance tests. Fixed point is complete, `resize` styles
  included. See
  [the remaining proposal](docs/proposals/std-buildout.md). Memories, FIFOs and
  stream adapters are IP for vendor packages and libraries, not std.
- 🟡 **API reference.** Keep [`siox-paper/docs/std.md`](https://github.com/Siox-lang/siox-paper/blob/main/docs/std.md) synchronized with each
  exported declaration and clearly label compiler/runtime intrinsics.
- 🔴 **Foreign HDL packages (Phase 3).** Map external library names and entity
  metadata without baking VHDL/Verilog syntax into the siox language.

## Out of scope for the current compiler

- Analogue domains, `across`/`through`, `::ddt`, solvers, and mixed-signal
  bridges.
- Schematic/layout design and place-and-route implementation.
- Vendor synthesis backends and foreign HDL compilation inside `sioxc`.
- A project/package manager inside `sioxc`.
