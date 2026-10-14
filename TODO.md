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

- 🔴 **Type inference is quadratic on long operator chains.**
  `Checker::type_of` records each expression's type but never reads it back,
  so every level of a left-deep chain re-infers the whole chain below it:
  `y = x + x + … + x` takes 1.9 s at 1000 terms, 6.7 s at 2000 and 28 s at
  4000 (`--emit metadata`). A span-keyed memo must respect that one span can
  be checked under different symbol tables (generic bodies). Found
  2026-10-13 while probing stack depth.
- 🔴 **Nested `xor` in hardware.** `y = a xor b xor c` (or
  `(a xor b) xor c`) on `unsigned[8]` ports fails E-P017 ("cannot derive a
  digital scheduler expression from Invalid"); one `xor`, `and`/`or` chains
  and `xor` mixed with `or` all lower. Found 2026-10-13 writing the benchmark
  workload; reproduces on current main.
- 🔴 **Hardware scalar packed-index metavalues.** A pure source function
  returning a `Logic` element from a packed struct port currently loses the
  discriminant in hardware lowering (`copy.data[6]` on `"01XZUWLH-"` returns
  binary zero rather than `X`). Both ordinary and DWARF native builds reproduce
  the failure; the same procedural call is correct. Preserve the source enum
  and companion-plane dependency through SourceValues field/index expansion,
  then verify native results and waveform parity in both packing modes.
- 🔴 **Hardware returned-array negative-label consumers.** In a pure hardware
  function, reading `value.gains[-1]`, `[-2]` and `[-3]` from a returned nested
  struct with `real[-1..-3]` loses the real values: adding them to a retained
  real field yields just that field in both ordinary and debug native builds.
  Actual GDB `finish` exposes all three returned array values correctly; the
  source return ABI is not the failure. Trace field/index selection and signed
  labels through SourceValues, preserve the declared real leaf format, and
  verify native arithmetic and waveform parity rather than inspection alone.
- 🔴 **Inactive aggregate-return array checks.** A hardware function returning
  a struct with runtime-indexed array fields can latch bounds failures even
  when its caller is in an untaken conditional. Ordinary and DWARF native
  builds both reproduce this with index 999 on a declared two-element array;
  equivalent scalar-return array calls already have inactive-path coverage.
  Keep SourceValues aggregate field/check prefixes under the caller's active
  predicate, preserving first-failure source order. This is a lowering gap,
  separate from the debug return transport and its active-call parity checks.
- 🔴 **Hardware signed widening/aggregate-field equality.** Comparing a returned
  nested `signed[81]` field with the same negative source argument (`-7`)
  selects the unequal branch in both ordinary and DWARF native builds, even though actual
  GDB formals and `finish` show `-7` in both places. The argument is formed by
  hardware `signed[81](left)` from a 64-bit integer port; a full-width unsigned
  comparison of both operands also fails. Procedural `signed[81](-7)` also
  zero-extends the integer pattern to `18446744073709551609` instead of `-7`;
  constructing the complete 81-bit two's-complement literal avoids that path.
  Audit sign extension and contextual widths for typed field comparisons;
  verify full-width native comparisons as well as source inspection before
  claiming a fix.
- 🔴 **Narrow enum-to-integer arithmetic in hardware.** A custom Resolve body
  with `let sum: integer = integer(self) + integer(rhs)` on a two-value enum
  interprets two High inputs as negative (`sum > 0` selects false). An ordinary
  native executable reproduces this without DWARF. Audit contextual signed
  widths/carry bits for kernel arithmetic on narrow nonnegative inputs; the
  declared integer must represent `1 + 1` as positive two. Keep this separate
  from faithful debugger inspection of the emitted narrow computation.

## LLVM

Owns exact-width native code generation and the object-side runtime ABI. Code:
`src/llvm/`.

- 🟡 **Runtime recursion and non-packed conversions.** A pure recursive
  testbench function (one value: `if`/`return` over operators and calls
  without effects) runs through its shared native copy, each recursive call
  evaluated only on its selected path; other recursion is E-P038. Recursion
  too deep for the native stack fails the test, naming the function
  (`sx_runtime_frame_room`). Recursion needing a frame (loops, mutated
  locals, effects) remains: proposal sioxc#83.
  Extend the currently
  supported inline-call and packed-value contract only with explicit frame,
  ownership, conversion and suspension semantics. Other recursive runtime
  calls are rejected at compile time; unsupported forms must fail before argument/callee effects
  or staged writes become observable. Keep the existing native transactional
  failure regressions when adding these capabilities. A testbench `From`
  body that is not one value expression (a loop, a statement `if`) now
  expands in the CFG like any call; it used to fall back silently to a raw
  resize. In hardware such a body is still rejected (E-P017 on the loop's
  index), not silently resized.
- 🟡 **Host-service extensions.** Runtime-sized arrays exist for testbenches:
  `read<T[]>(path)` for `integer` and packed numerics up to 64 bits, with
  `'length`, checked indexing, iteration and `T[]` parameters, sharing the
  runtime string table and services. Still open: writable or growable
  arrays, arrays built from literals, multiword and `Logic` elements, and
  arrays of arrays.
  Runtime-computed file paths are done: a testbench `read`/`exists` path may
  be any string (fixed `Char[N]` values are formatted, runtime strings pass
  their UTF-8), resolved against the source directory by
  `sx_runtime_source_path` as a literal is at compile time. Hardware reads
  still take a literal. Keep allocation, persistent state
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
- 🟡 **Optimization measurements.** `scripts/bench.py` measures compile
  time, compile memory, binary size and simulation run time for default,
  `bitpack` and host-SIMD builds, over the corpus's compile outliers plus a
  million-cycle workload, and compares two runs (`--compare`). Report its
  before/after numbers with any optimization: structural simplifications
  alone are not evidence of a speedup. The 2026-10-13 baseline: the worst
  outlier, `float_sqrt_test`, compiled for 75–80 s at 0.5 GB into a 2.5 MB
  binary, until shared select arms expanded once (4.8 s, 83 MB, 341 KB).
  `hardware_local_values_test` compiled for 19 s because companion planes
  of a value both selection arms read were built once per arm, doubling at
  each block-local write; they are now built once (a reduced case went from
  20.9 s to 2.6 s; eight chained writes overflowed the stack before). A chain of
  guarded writes to one local carried every earlier write's companion
  selection inline (each companion temporary spanned the whole chain); a
  guarded companion above 8192 nodes now binds to a temporary (40 writes
  24 s → 10 s, 200 writes 280 s → 65 s). Lower limits are faster on chains
  but bind large unchained companions too: at 2048,
  `hardware_aggregate_writes_test` took 27 s against 16 s. Companion lowering also recurses once per write. The
  workload runs at about 1.1 µs a cycle (1.5 µs
  with `bitpack`). Indexed packed writes now use one update per
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

Native source DWARF is implemented and accepted; its requirements, permanent
checks and read-only inspection/debugger limits are recorded in the
[acceptance audit](https://github.com/Siox-lang/siox-paper/blob/main/docs/dwarf-audit.md).

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
  floating-point operators exist. Remaining: floating-point
  subnormals/rounding modes, and binary64 `*`/`/`/`sqrt` (their significand
  product, dividend and radicand need more than the 64-bit kernel word);
  optionally
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
