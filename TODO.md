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
- 🟡 **Public entity receiver methods.** Static public entity functions work.
  Lower `instance.method()` accessors and effectful methods to stable generated
  ports, first with one caller per method/instance, then define arbitration
  before allowing multiple callers. Struct/view methods already work.

## IR

Owns signals, canonical process control flow, initializers, layouts, enum/logic
metadata, derived scheduling forms, and semantic lints. Code: `src/ir/`.

The Phase 1 canonical-pipeline migration and exit audit are complete. Preserve
the [ingress invariants](https://github.com/Siox-lang/siox-paper/blob/main/docs/architecture.md#current-process-ir-ingress-boundary)
when extending the IR; executable extensions and optimization measurements are
listed under LLVM and Output below.

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

- 🟡 **Native source debugging follow-through.** Direct LLVM DWARF now supports
  source breakpoints/steps, named signal/test storage, process/loop locals and
  lexical shadowing, normalized non-byte-aligned wide scalars and source-enum
  `$logic` companion views (including recursive structs/arrays and weak states).
  Runtime-owned `Char[]` strings expose borrowed Unicode/UTF-8 content and
  lengths, including process locals and cleanup between tests. CFG calls retain
  distinct call-site scopes, return types and true formal parameters, including
  live mutable/indexed aliases, receivers, aggregates, wide/real projections
  and borrowed runtime/literal strings (including zero-storage empty values)
  across suspension. Procedural expression-shaped calls now preserve repeated,
  same-line and nested source frames, scalar/aggregate bindings, borrowed
  runtime-string arguments/returns, literal/empty string bindings and
  selected-call stops without ordinary-build copies. Consumed
  literal/empty string returns also retain their source frames through
  folded length and formatted text consumers. Constant arguments execute at
  the caller only when requested by the native body; empty specializations
  return void, with no fabricated payload. Readiness joins preserve one source
  call across both/first-only/second-only consumers, and none for untaken calls.
  GDB `finish` on the empty specialization adds no returned value.
  Pure function-body if/match branches retain nested lexical shadowing and
  selected alias/aggregate return stops. Hardware SourceValues calls now retain
  nested frames,
  selected return scopes, declaration-owned types and shared-helper/event owners.
  Native expression arguments retain the actual consumer width/sign/layout
  requests independently of inspection types, including narrow signed nested
  arithmetic and multiple formats used inside one callee.
  Multiply-used scalar call results retain exact-format SSA readiness through
  selected joins; native GDB counts cover both/first-only/second-only consumers
  and untaken calls across all four inline/shared caller/callee combinations.
  Field-backed hardware aggregate parameters/locals use LLVM fragments over
  already-emitted values, including nested structs, descending/negative-label
  arrays, non-byte-aligned wide signed/unsigned leaves and real fields. Pruned
  members remain unavailable; metadata never adds executable dependencies.
  Fully retained, typed pure hardware aggregate returns share
  one native source boundary and exact-layout SSA result across selected field
  consumers, including nested if/match return branches, independent field
  conditions and aliased values. Matching field selectors use one whole
  selected-return SSA join, with return scopes and lexical shadowing preserved.
  Complete pure nested-call aggregates now share a return only when every
  executable use first crosses a value that demands every retained field.
  Whole-value conditionals and provably valid constant array selectors preserve
  that proof; separate field uses and unknown/invalid selectors cannot justify
  eager grouping. Complete checked/effectful aggregates now share a native
  boundary when joint demand also proves an uninterrupted first-use field
  order. Construction order is not assumed to be consumption order; an
  intervening call/check/mutable read or conflicting branch orders keeps the
  original leaf evaluations. One-bit and/or RHSs are not unconditional demand.
  Total division can be grouped without introducing checks. Native/GDB coverage
  includes opposite field orders, an intervening putchar call, first-failure
  bounds diagnostics, zero divisors, unavailable short-circuited abort fields,
  changing inputs, exact call counts and stdout/VCD parity under both hints.
  Effectful expression source frames now stay in the caller's common emitter
  and effect cache, rather than introducing a debug-only native body. Hardware
  compatibility captures and direct CFG execution keep their ordinary lifetimes;
  effect-free native bodies remain shared across those contexts. Complete
  grouping for separately demanded/selected nested-call and partial aggregate
  returns; no debug
  boundary may force an unconsumed field or invent a zero for a pruned member.
  Independently selected nested partial returns now have a native/GDB
  regression for neither/left/right/both consumers and changing inputs.
  Existing fragment frames preserve the exact foreign effect order, including
  transient hardware evaluations, and keep an unused abort field unavailable.
  Inspection waits through lazy caller-owned argument ranges and the caller's
  actual fragment store; it never requires an earlier fabricated value.
  Direct history-slot checks prove partial source finish adds no native return.
  Reusing a foreign field across another call now preserves the ordinary stdout
  order/count, including narrow/wide consumer formats and changing source epochs.
  A seeded host-random regression also verifies one draw across consumer formats.
  GDB checks cached partial return fragments, complete inline scalar snapshots,
  normalized procedural pure-tail native returns and exact stop counts. Inline references
  reset before their visible source range, and the completion edge remains
  inspectable after the return snapshot is written. Cached actual foreign values
  populate inspection bindings without a new evaluation or a stale pure read.
  Normalized IR and native instruction stepping now confirm that the loop's
  pure-tail `formats` boundary does not consume its original formal `value`;
  it remains unavailable rather than forcing a debug-only argument. Its local
  `saved` becomes inspectable after reading the already-captured result, with
  repeated-entry checks and correct native finish values. Audit other normalized
  signatures without treating genuine pruning as a missing executable input.
  Effect-free partial leaf evaluations retain the source function/type as inline
  frames over their existing native leaf bodies, with fragmented `$return`
  snapshots. They no longer advertise a full native aggregate return ABI;
  discarded fields remain unavailable. Complete one-leaf wrappers keep their
  native result ABI. GDB checks both retained field positions, source types,
  caller bindings and untaken abort fields for integer/integer and integer/real
  structs across all caller/callee inline/shared hint combinations.
  Shared Process functions now retain their source frames directly on the
  canonical native body, including selected branches, lexical locals and
  declaration-owned scalar inspection types. Keep native body-count, actual
  call-count and memory regressions as coverage expands; LLVM ABI types must
  not be inferred from the debugger's read-only inspection representation.
  Hardware expression formals now prefer concrete declaration layouts, with
  inspection-only packed operator specializations (including rhs `Self`) kept
  separate from kernel execution shapes. Native GDB checks cover contextual
  unsigned widths, custom packed Self operands, integer/real suffixes, integer
  conversions and signed constrained/generic parameters in both packing modes.
  Resolve now retains the resolved signal/element's known receiver layout and
  concrete Self return type. Conversion inspection retains its known nominal
  target or sized packed family independently of the executable return shape.
  Native GDB checks custom-enum Resolve inputs/results, one-field From returns
  and 8-to-7-bit fixed-family arguments/returns, including actual finish values.
  Reference setup/native-input inspection has line-zero locations, so function-
  name breakpoints observe initialized inputs instead of stack-slot garbage.
  Packed operands with no recorded source range now expose explicit physical
  LSB-first `$logic.bits` positions, while declared ranges keep their labels.
  Fully known literals retain arbitrary-width bits; omitted nonconstant bits
  and unconsumed metadata remain unavailable. Function-entry GDB checks cover
  contextual operator literals without changing the native argument payload.
  A nonconstant source argument consumed only at a narrower width remains
  unavailable instead of zero/sign-padding omitted bits. Entry inspection
  prefers the widest already-consumed format when the same argument is used
  both narrowly and fully; no new ABI format is requested. Dynamic 81-bit
  hardware operands check parameter/local availability and epoch changes under
  both hints, alongside ordinary/debug output and VCD parity. Known constants
  and sound extension from a retained natural-width value remain inspectable.
  Mixed integer/real struct-to-struct From and signed integer-derived Resolve
  now have entry-signature and actual finish-value regressions under both inline
  hints. Nested packed returns check numeric-only callee views remain unavailable
  until the caller actually consumes metadata; caller companions retain all nine
  source states. Finish auditing other supported conversion specializations and
  companion-plane transports. Generic From constructor syntax rejected by the
  ordinary frontend is a separate language boundary, not missing DWARF coverage;
  these cases do not prove every signature or every argument's availability.
  Procedural 81/127/128/129/256-bit constructor and
  generic bindings now retain their checked signed/unsigned target type even
  without stored layouts, including narrow/wider consumer tests. Expression
  parameters/locals now expose recursive `$logic` companions over already
  consumed value/metadata planes, with 9/81-bit, selected-branch, negative-array
  label and partial-hardware-field checks. Unconsumed or pruned planes remain
  unavailable, not fabricated binary values. Audit other argument/return-plane
  transports and verify GDB `finish` for normalized wide scalars and remaining
  mixed-layout/partial aggregate return ABIs.
  GDB 18.1 on AMD64 does not classify arbitrary-width basic integer returns:
  an 81-bit signed return reports zero and a 129-bit return triggers an internal
  assertion, even with return printing disabled. Do not change source types to
  hide that limitation; wide scalar source calls now reuse an inline frame
  over the existing native body and expose a read-only `$return` snapshot only
  when the emitted result retains every declared bit. Narrowed results remain
  unavailable, and repeated entry clears the previous snapshot. GDB `finish`
  leaves the inline frame without invoking unsupported native return decoding.
  A small declared result with a private integer payload above 128 bits uses
  the same path: GDB cannot infer that hidden memory ABI from the source type.
  Ordinary register-return integers keep their native frames. Track the
  debugger fix independently from compiler return-ABI checks.
  Small complete aggregates containing odd-byte-sized basic fields now retain
  their true return type on an inline source frame over the existing native
  body. A nullable, read-only `$return` snapshot becomes available after the
  whole result executes; finish returns to the caller without invoking GDB's
  unsupported native classifier. Regressions cover 17/33/65/81-bit fields with
  an 8-bit sibling, procedural and hardware calls, both inline hints, repeated
  calls without stale snapshots, caller bindings and ordinary/debug VCD parity.
  This does not fix GDB's native arbitrary-width history-value decoder.
  Two-integer hardware aggregate
  `finish` values are checked for straight-line, nested/match, independent and
  alias returns with positive, negative and zero inputs. Scalar
  real native boundaries use double returns, with actual `finish` checks for
  positive/negative values and signed zero in
  inline/shared bodies; the internal emitter still exchanges integer bits.
  Complete small native SysV aggregates with one or two 64-bit scalar leaves
  now use integer/double return slots, with actual finish checks for both
  mixed field orders, nested wrappers, real arrays and one-real wrappers under
  both inline hints and packing modes. Complete aggregates whose normalized
  source size exceeds 16 bytes now use a caller-owned memory return, including
  24-byte integer/real structs, 19-byte unsigned[81]/real structs and nested
  43-byte signed[81]/real/descending-array values. Actual GDB finish checks
  all fields, signed values and signed zero under both inline hints and packing
  modes. Shared-body units verify void/sret signatures, canonical i192/i145
  results, entry-owned storage and no second execution wrapper. The caller reads
  only a separate canonical payload, never normalized inspection bytes.
  Runtime and nonempty-literal Char[] returns now use the same caller-owned
  transport: a borrowed 32-byte Unicode/UTF-8 view followed by the unchanged
  canonical handle or packed literal bits. Actual GDB finish checks embedded
  NULs, UTF-8, reuse after await and distinct test roots under both inline hints
  and packing modes. Empty specializations remain void without a new debugger
  history value; declared fixed Char[N] results retain their array type.
  Boundary units verify canonical i32/i64/i193 payloads, handle-only metadata
  queries, caller decoding and LLVM validity. This does not imply general
  dynamic-array support or source-string eligibility for shared Process bodies.
  Complete small byte-aligned aggregates now also support 8/16/32-bit integer
  leaves beside real fields; adjacent integer leaves share their SysV eightbyte.
  The codec retains the actual slot widths instead of assuming every slot is
  64 bits. Naturally unaligned source members reuse the memory carrier even
  below 16 bytes, including nested 12-byte Char/real and normalized 9-byte
  unsigned[3]/real results with an unchanged i67 execution payload. Actual GDB
  finish checks both Char/real field orders, nested alignment, Char[2] tails,
  byte fields and shared byte slots, including every transient input and settled
  negative/signed-zero cases under both inline hints and packing modes. Shared
  units check i67/i72/i80/i96/i112 carriers and mixed i8/i16/i32/i48 slots.
  Naturally aligned complete sub-byte aggregates now use reversible SSA
  packing between canonical field offsets and byte-normalized register slots.
  Signed leaves sign-extend their padding; executable callers decode the exact
  original packed widths. GDB finish checks unsigned[3]/signed[3] pairs,
  real/sub-byte tails, nested structs and three-element unsigned[3] arrays
  (including opposite element orders on selected branches), under both inline
  hints and packing modes. Shared units retain canonical i6/i70 results across
  i16 and mixed double/i16 native signatures without an extra execution wrapper.
  Extend the remaining odd-sized basic scalar, wide scalar and partial-layout
  transports without forcing discarded fields or reading inspection buffers
  as executable values.
  The supported runtime-sized native representation is currently Char[] handles;
  general dynamic arrays remain the separate host-service extension below.
  Keep debug-only aggregate inspection buffers faithful at source stops and
  document their read-only/snapshot nature. Verify default/bitpack debugger and
  debug-corpus gates; never resurrect per-design C translation for metadata.
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
  floating-point operators exist. Remaining: fixed-point division/configurable
  resize, floating-point division/square root/subnormals/rounding modes and
  fixed-point conversions; optionally generic vectors/matrices. Each needs
  executable conformance tests. See
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
