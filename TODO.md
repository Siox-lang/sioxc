# TODO

Outstanding work for the simulation-first siox compiler, organized by the
layer that owns each change:

`source → AST → semantic analysis → elaboration → IR → LLVM → output`

This file tracks active work, not implementation history. Completed migration
details and measurements belong in [`chat.md`](chat.md) and the documents under
[`docs/`](docs/). Status last audited 2026-09-24 against the compiler, standard
library, `siox-tests`, and CI.

Legend: 🔴 not started · 🟡 partial / constrained.

## Phase 1 exit criteria

Phase 1 is complete when:

- every explicit and implicit hardware/test process lowers once into canonical
  Process IR;
- native objects and native test executables use the same IR-to-LLVM lowering;
- the fixed runtime schedules all processes, time, delta cycles, and host
  services without per-design generated C;
- the default and `bitpack` corpus agree with the compatibility oracle in
  results, diagnostics, time progression, resolved values, and VCD/FST output;
- the direct path becomes the default, then the generated-C translator and the
  temporary AST-to-Process adapter are deleted.

The executable migration plan and current ABI decisions live in
[`docs/proposals/testbench-software-ir.md`](docs/proposals/testbench-software-ir.md)
and
[`docs/proposals/native-process-runtime.md`](docs/proposals/native-process-runtime.md).

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

- 🟡 **Make Process IR the lowering authority.** All finalized hardware and
  test behavior is present in `Design::process_ir`, but normalized
  `Driver`/`EventBlock` hardware is still imported through a migration bridge.
  Lower explicit processes and implicit continuous behavior into Process IR
  first, then derive optimized driver/event scheduling from that one product.
  Delete `test_ir` after it no longer performs a separate AST-to-value/CFG
  translation.
- 🟡 **Canonical composite sizing.** Put the checked packed width on canonical
  aggregate values so IR consumers do not rediscover struct/array widths from
  `SourceLayout`. Source type-cycle rejection remains the cycle boundary.
- 🟡 **VHDL delayed-assignment semantics.** The fixed queue implements
  transport-like one-shot writes. Add the required inertial cancellation and
  rejection behavior, keyed by target/driver and source order, without adding
  a backend-specific scheduling rule.
- 🟡 **Canonicalize dynamic aggregate updates.** Process assignments already
  retain runtime-selected projections, and direct LLVM evaluates every index
  and right-hand leaf before one packed root merge. Make that pre-write
  snapshot/update invariant explicit in Process IR validation or a dedicated
  operation before deleting the temporary typed-AST adapter.
## LLVM

Owns exact-width native code generation and the object-side runtime ABI. Code:
`src/llvm/`.

- 🟡 **Complete direct Process IR lowering.** Exact-width scalar and recursive
  packed values, branches, loops, matches, clocks, suspension, delayed writes,
  formatting, assertions, scalar foreign calls, and source-defined operator
  impls execute directly today. Remaining executable forms are receiver
  methods, procedure-shaped calls, runtime recursion/general call CFGs,
  non-packed conversions, and dynamic strings. Unsupported forms must
  continue to fail transactionally before calls or staged writes become
  observable.
- 🟡 **Move all host services behind the fixed ABI.** Add runtime-owned UTF-8
  strings/dynamic arrays, `read<T>` and file failures, deterministic random,
  and any remaining simulation-only calls. LLVM emits value semantics; the
  runtime owns allocation, persistent state, and host contact.
- 🔴 **Quad precision (future, not advertised).** If a real use case requires
  it, add LLVM `fp128` operations, constants/conversions, ABI rules, formatting,
  and a software fallback before exposing a language feature.
- 🔴 **Optimization measurements.** Maintain repeatable object-size,
  compile-memory, compile-time, and simulation-throughput benchmarks for
  default, `bitpack`, and host-SIMD builds. Structural simplifications alone
  are not evidence of a speedup.

## Output

Owns native objects, test executables, metadata/dumps, diagnostics, waveforms,
and future elaborated RTL artifacts. Code: `src/driver/` and `runtime/`.

- 🟡 **Retire generated C.** The fixed scheduler/CLI already links LLVM-emitted
  process entries and runs 165 corpus cases in agreement without design C;
  all 165 also match VCD signal values and timestamps in default and `bitpack`,
  while focused tests establish decoded FST parity for hierarchy, all value
  kinds, multiword values, and monotonic multi-test timelines.
  Finish the remaining LLVM/runtime coverage, make this path unconditional,
  run the full default and `bitpack` differential gates, then delete the
  AST-to-C statement/value translator and its dispatcher. Clang may remain a
  linker driver; it must not translate siox semantics through C.
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
math/text/file services, and reusable hardware models. Code: `std/`.

- 🟡 **Library build-out.** Add canonical counters, synchronizers, memories,
  FIFOs, stream adapters, and fixed-point families with executable conformance
  tests.
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
