# TODO

Outstanding work for the simulation-first siox compiler, organized by the
layer that owns each change:

`source → AST → semantic analysis → elaboration → IR → LLVM → output`

This file tracks active work, not implementation history. Completed migration
details and measurements belong in [`chat.md`](chat.md) and the documents under
[`docs/`](docs/). Status last audited 2026-09-20 against the compiler, standard
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
hierarchy. Code: `src/syntax/`, `src/resolve.rs`, `src/types.rs`, and
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
- 🟡 **Dynamic aggregate updates.** Represent runtime-selected aggregate
  projections and multiple partial destinations of one root as one explicit
  pre-write-snapshot/update operation. Reads, writes, checks, and merge order
  must be defined before either backend lowers it.
- 🟡 **Testbench metavalue storage.** Retain a companion plane for locally
  stored packed Logic values so direct comparisons and aggregate operations
  preserve X/Z just like finalized hardware signals.

## LLVM

Owns exact-width native code generation and the object-side runtime ABI. Code:
`src/llvm/`.

- 🟡 **Complete direct Process IR lowering.** Exact-width scalar and recursive
  packed values, branches, loops, matches, clocks, suspension, delayed writes,
  formatting, assertions, and scalar foreign calls execute directly today.
  Remaining executable forms are receiver methods, procedure-shaped calls,
  runtime recursion/general call CFGs, non-packed conversions, dynamic strings,
  and the dynamic aggregate operations defined above. Unsupported forms must
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
  process entries and runs 124 corpus cases in agreement without design C;
  all 124 also match VCD signal values and timestamps in default and `bitpack`.
  Finish the remaining LLVM/runtime coverage, make this path unconditional,
  run the full default and `bitpack` differential gates, then delete the
  AST-to-C statement/value translator and its dispatcher. Clang may remain a
  linker driver; it must not translate siox semantics through C.
- 🟡 **Direct VCD/FST output.** The LLVM object now exports immutable waveform
  descriptors and the fixed runtime writes VCD at settled change points,
  including hierarchy, arbitrary-width values, Logic X/Z, real values, enums,
  and monotonic multi-test timestamps. Add the fixed libfst consumer and accept
  non-`.vcd` output paths before removing the compatibility waveform writer.
- 🟡 **Runtime diagnostic parity.** Resolve Process source IDs/offsets through
  stable embedded source metadata so warnings and failures use the same
  filename, line, snippet, and caret form. `warn_test` is the remaining known
  stdout divergence; do not remove useful source locations merely to match the
  compatibility backend.
- 🔴 **Scalable mini-runtime scheduler (Phase 2 optimization).** Model the
  runtime as a small deterministic RTOS. With one configured host thread,
  language processes are logically concurrent but cooperatively share that
  thread: each runs until a scheduler boundary, then yields while the scheduler
  advances delta cycles or simulation time. A higher thread count partitions
  independent ready processes across worker threads without changing those
  language-level semantics. Replace process-global state with a per-run context;
  protect shared runtime structures with suitable mutexes or ownership, stage
  signal writes and side effects in worker-local buffers, synchronize at delta
  barriers, then merge and commit in stable process/instruction order.
  Resolution, source-order overrides, impure foreign calls, file I/O, and
  diagnostics remain serialized unless proven independent. Require identical
  results, diagnostics, and VCD/FST traces for one and many host threads in
  default and `bitpack` modes; enable extra threads only when benchmarks show a
  gain.
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
