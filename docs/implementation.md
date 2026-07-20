# Implementation plan and status

This document tracks **what needs to be implemented for Phase 1** and how far
along each piece is. It maps the 12 spec stages (from [spec.md](spec.md)) to the
crate that owns them, lists the acceptance criteria, and records current status.

For the crate layout and conventions, see [architecture.md](architecture.md).
Phase 1 is simulation-first: no analogue, no schematic layer, no synthesis
backend (those are [roadmap.md](roadmap.md)).

## Status at a glance

Legend: 🔴 stub (signature only) · 🟡 skeleton (types defined, logic TODO) ·
🟢 working · ✅ done with acceptance tests.

| Crate | Stage(s) | Status | Notes |
| ----- | -------- | ------ | ----- |
| `siox-diag`    | 10   | 🟢 working | spans, source map with line/col, diagnostics, code catalogue |
| `siox-syntax`  | 1, 2 | 🟢 working | lexer, parser, AST, round-tripping pretty-printer |
| `siox-resolve` | 3    | 🟢 working | defs/DefIds, imports, paths, enum variants, attributes |
| `siox-types`   | 4    | 🟢 partial | type-inference core; trait-driven (`Boolean`) conditions, attr target/value, input-write, assignment/init compatibility, `::ddt` |
| `siox-elab`    | 5    | 🟢 partial | instance hierarchy, param const-eval + substitution, connection width checking |
| `siox-ir`      | 6    | 🟢 partial | language-neutral IR; drivers + event blocks; **hierarchical lowering** (per-instance signals + connection drivers); process decomposition + validator for codegen; `sioxc ir` |
| `siox-run`     | 7, 8 | 🟢 partial | the engine-agnostic **kernel/test runner**: `Engine` trait, `#[test]` runner, `await`/`clock` timing + event wheel (**owns simulation time**), `assert!`, waveform sample recording. Drives whatever `Engine` it is given |
| `siox-sim`     | 7    | 🟢 partial | the delta-cycle **interpreter** — one `Engine`, kept as the differential oracle + >64-bit fallback (not in the default build; `--features interp`) |
| `siox-wave`    | 9    | 🟢 partial | VCD waveform export from a traced run (real timestamps, JIT-traced); `sioxc sim --wave` |
| `siox-llvm`    | B    | 🟢 partial | **default execution engine** (inkwell, `llvm` feature on by default): emit `.ll`, JIT-run (`sioxc test`), AOT native object (`sioxc <file>`), native test binary (`test --no-run`); differentially verified vs. the interpreter oracle |
| `sioxc`        | 12   | 🟢 working | rustc-shaped: bare `sioxc <file>` compiles; `check`/`test` (libtest output, `--no-run`)/`sim --wave`/`ir`/`ast`/`tree`/`tokens`/`emit-llvm` |

## Stage-by-stage

Each stage lists its acceptance criteria (from the spec) and current status.

### Stage 1 — Syntax freeze (`siox-syntax`, `docs/`) — 🟢
- **Acceptance:** the required examples (counter, register, mux, FSM,
  ready/valid, enum monitor, test entity, extern entity, attribute usage) have
  final syntax.
- **Status:** grammar is implemented in the parser; the runnable example/
  conformance corpus (counters, FSMs, a FIFO, SPI, RISC-V fragments, …) lives in
  the [Siox-lang/siox-tests](https://github.com/Siox-lang/siox-tests) repo, which
  CI runs against every build.

### Stage 2 — Lexer & parser (`siox-syntax`) — 🟢
- **Acceptance:** valid examples parse; invalid syntax gives useful spans; the
  parser recovers after common mistakes; the pretty-printer round-trips.
- **Status:** done — hand-written lexer, recursive-descent + Pratt parser with
  forward-progress recovery, and an idempotent pretty-printer. `siox parse` and
  `siox ast` work.

### Stage 3 — Name resolution (`siox-resolve`) — 🟢
- **Acceptance:** unknown names reported; private items inaccessible across
  modules; undeclared attribute usage fails; `State::Idle` resolves.
- **Status:** top-level definitions, `using` imports/aliases, type/enum-variant/
  attribute resolution, and a use-site → `DefId` map. Reports `UNKNOWN_NAME` and
  `DUPLICATE_ITEM`. Cross-module visibility is not yet enforced (single global
  namespace); value identifiers resolve best-effort.

### Stage 4 — Type & kind checking (`siox-types`) — 🟢 partial
- **Acceptance:** no implicit `uint[8]`→`uint[16]`; undeclared/mis-targeted
  attributes rejected; cannot write `in` ports; bare `Logic` condition rejected;
  `::ddt` rejected.
- **Status (done):** type-inference core (annotation → `Ty`, per-impl symbol
  table, `type_of`); a trait-driven condition check (a condition's type must
  implement `Boolean`; `Bit`/`Bool` are built in, `Logic` is excluded, user
  types opt in via `impl Boolean for T`) (`E-P003`), attribute target (`E-P006`)
  and value (`E-P007`), write-to-input-port (`E-P004`), assignment and
  initializer compatibility / no-implicit-conversion (`E-P003`, literal- and
  enum-aware), the type-strict `let` rule (every binding declares its type;
  `let x = e` is `E-P012`; an entity may not be declared `const`, `E-P013`),
  and the `::ddt` Phase-2 guard (`E-P010`).
- **Status (done, cont.):** **method-call typing** — `recv.method(args)` types
  as the impl method's declared return type (inherent and trait impls), so the
  result flows into downstream checks (a `Logic`-returning method used as a bare
  condition is rejected).
- **Status (done, cont.):** **strict assignment width** — the checker rejects a
  literally-annotated mismatch (`y: uint[8] = b: uint[16]`, `E-P003`); widths
  that come from parameters (`uint[W]`) are only concrete after elaboration, so
  IR lowering additionally checks a scalar target against a direct
  signal-reference value (name/field/element/slice/concat) once widths are
  resolved. Arithmetic is exempt — results are not auto-widened (overflow wraps;
  a different width is an explicit `resize`), so `sum = a + b` is untouched.

### Stage 5 — Elaboration (`siox-elab`) — 🟢 partial
- **Acceptance:** all params known post-elab; all required ports connected;
  direction violations reported; bus modes expand to leaf permissions; extern
  entities are black boxes; `siox tree` prints the tree.
- **Status (done):** instance hierarchy from `#[top]`/`#[test]` roots, parameter
  const-evaluation and substitution into concrete port types (`uint[W]` →
  `uint[8]`), `.clk`-shorthand connection resolution, missing-port (`E-P005`) and
  unknown-port checks, **port-connection width checking** (a port's width must
  match the signal it connects to, `E-P003`), **generate constructs** — a
  `for i in lo..hi { .. }` loop unrolled over an inclusive, directional static
  range (the loop index substituted into names and indexed connections), and a
  generate-`if`/`else` whose compile-time-constant condition selects which
  branch's instances and drivers are built; the two nest freely. Non-constant
  conditions stay behavioral. Also extern black boxes, cycle detection, and
  `siox tree`.
- **Status (done, cont.):** **sub-instance port access** — an instance's ports
  are readable as `<inst>.<port>`, so an output may be left unconnected at
  construction and read directly (only `in` ports must be wired). **Instance
  arrays** — `let stage: Sub[N]` built element-wise (`stage[i] = Sub { .. }`, in
  a generate loop) creates named, indexable instances whose ports read as
  `stage[i].port`.
- **Status (done, cont.):** **bus modes** (spec 3.19) — a directional view over
  a struct (`bus: out Stream::Source`) flattens to per-field leaf signals, each
  tagged with its direction from the mode impl (`out valid; in ready;`), so
  valid/data flow Source→Sink and ready flows Sink→Source over the shared net.
- **Status (done, cont.):** **type-parameter generics** — a struct (`Pair<T>`),
  entity (`Reg<T>`), or bus (`Stream<T>::Source`) parameterized by a type
  specializes to its type argument; the type parameter is bound in the impl
  body, treated as opaque by the checker, and substituted into signal types at
  lowering. Writing to an `in` bus leaf is a clear `E-P004`.
- **Status (todo):** full direction analysis (reading an undriven `out`, etc.).
  Concrete parameter widths already propagate into instance signal types
  (an internal `uint[W]` wraps at the substituted width).

### Stage 6 — Digital IR (`siox-ir`) — 🟢 partial
- **Acceptance:** event vs. combinational deps explicit; sequential updates
  separated from local assignments; `::event`/`::old` represented directly;
  `siox ir` prints normalized IR.
- **Status (done):** a **language-neutral** IR (its own `BinOp`/`UnOp`, no AST
  imports — see the convergence-layer goal). `lower` walks each non-extern
  entity's behaviour into signals, combinational `Driver`s, and `EventBlock`s;
  detects event-controlled blocks (`::event`/`::rising`) and expands
  `clk::rising` to `Event(clk) && Old(clk)=='0' && Current(clk)=='1'`; nested
  `if`/`else` priority accumulates into next-state conditions. `match` lowers to
  first-match `scrutinee == variant` guards with enum discriminants (`Idle=0`,
  ...). Signal widths are made concrete by substituting the entity's instance
  parameters (`uint[W]` with `W=8` -> width 8). Struct- and array-typed signals
  flatten to one scalar per leaf (`s: Packet` -> `s.valid`, `s.data`; `a: Bit[4]`
  -> `a[0]..a[3]`); field/constant-index access resolves to the flattened signal.
  Constant bit slices (`data[7..4]`) lower to a `Slice` IR op; concatenations
  (`{hi, lo}`, including nested `{a, {b, c}}`) fold into a shift/add tree sized
  from each part's source width. `siox ir` prints it.
- **Status (done):** hierarchical lowering — each instance lowers into its own
  signal namespace (`Add2.s1.a`), sub-instances (`let s = Sub {..}`) recurse
  under `path.s`, and every port connection becomes a driver (`in` reads the
  parent, `out` drives it). Multiple instances of one entity take per-instance
  params (`Reg<8>` and `Reg<4>` in one parent size correctly).
- **Status (done):** tops-only lowering — only `#[top]`/`#[test]` roots lower;
  everything else lowers recursively per-instance from there (no entity is
  lowered standalone by type). A testbench's DUTs lower under the testbench
  path (`CounterTest.dut.*`), so two instances of one entity stay distinct.
- **Status (done):** dynamic array indexing (`regs[addr]` at a runtime index —
  a mux tree to read, per-element gated writes); **struct/array-typed ports
  across instances** (each leaf wires: `.s = link` -> `s.valid`<->`link.valid`);
  **`inout` bidirectional ports** (alias the shared net so parallel drivers fold
  through `Resolve` — Verilog's model).
- **Status (done, cont.):** **method-call lowering** — `recv.method(args)`
  inlines the impl method's body during lowering, reusing the operator-impl
  inlining machinery, so all three engines get the same primitive tree. Two
  forms: **value-returning methods** in expressions (`a.cmp(b)`, `p.sum()`,
  branching on `self`) inline to a value, and **statement methods** (`s.send(v)`
  whose body drives `self.valid`/`self.data`) inline as drivers on the
  receiver's flattened fields — via a general `self`->receiver, param->argument
  substitution over the body. Covers inherent and trait impls.
- **Status (done, cont.):** **composite (struct/array) `inout` ports** — a
  struct- or array-typed `inout` port aliases each flattened leaf (`bus.hi`,
  `pin[0]`) onto the matching leaf of the shared net, so every leaf's parallel
  drivers fold through `Resolve` independently (scalar/vector inout already
  worked).
- **Status (done, cont.):** **method calls in testbench stimulus** — all three
  engines inline a `recv.method(args)` call, resolving the receiver type from
  the local's declaration and substituting `self`/parameters into the body, so
  a struct-typed testbench local can drive a DUT through a method result. The
  native `--no-run` emitter materializes an unconnected struct-typed testbench
  local as one C local per field (nested structs recursed, vector families kept
  as scalar leaves) and inlines the method body as a C expression, reaching
  parity with the interpreter and JIT.

### Stage 7 — Simulator core (`siox-sim`) — 🟢 partial
- **Acceptance:** correctly simulates mux, register, counter, FSM, ready/valid
  handshake, enum monitor (`::old`), struct/array element events.
- **Status (done):** the delta-cycle `Simulator` over the IR `Design`:
  current/old/event state, IR-expression evaluation (`Event`/`Old`/`Current`,
  logical/comparison/arithmetic ops), combinational fixpoint, event blocks fired
  once per edge with next-state semantics, value masking to the signal width
  (arithmetic wraps at `2^width`), and `set`/`read`/`settle`/`advance`. The
  counter simulates correctly (increments on rising edges, sync reset, enable
  gating, wrap-around). Verified on **counter, register, mux, an FSM** (`match`
  over an enum), and an **enum `::old` transition monitor** (`started` pulses on
  Idle → Run), a **ready/valid handshake** (compound condition in an event
  block), **struct-field signals** (`p.data` per field), and **array-element
  signals** (`a[2]` per element).
- **Status (done):** four-value logic — a `Logic` scalar carries `'0'/'1'/'Z'/'X'`
  and the std_logic truth tables (`and`/`or`/`xor`/`not`) inline correctly
  (`'X' and '1' == 'X'`, `'0' and 'X' == '0'`); parallel drivers fold through
  `impl Resolve for Logic`; verified interpreter == JIT == native.
- **Status (todo):** cascaded event domains; X/Z propagation through vector
  arithmetic (scalar Logic is exact).

### Stage 8 — Tests, assertions, stimulus (`siox-run`) — 🟢 partial
- **Acceptance:** passing assertions report success; failures report
  file/span/message; multiple tests run; `sioxc test <dir>` runs a whole corpus.
- **Status (done):** the runner discovers `#[test]` entities, maps their signals
  to the DUT via the elaborated connections, and drives the stimulus (`let`
  initials, assignments, `tick(clk)`, `wait`, `for`, `if`, `assert!(cond,
  "msg")`) against any backend via the `Engine` trait. **Timing:** `clock(clk,
  period)` starts a background clock and `await` waits on time / an edge
  (`clk::rising`) / a condition — driven by the runner-owned event wheel, so
  multiple clocks interleave. `sioxc test [name]` runs all or a filtered subset
  in libtest style (`test … ok`/`FAILED`, `file:line:col`), exits nonzero on
  failure, and `--no-run` links a native multi-test binary. **`print!`** renders
  each argument by kind: reals as floats, `Char` as the character, and
  enum/`Logic` values as their variant symbol (`'X'`, `Idle`) via the design's
  `enum_syms` map — on all three engines.
- **Status (done, cont.):** `sioxc test <dir>` runs every `.siox` file in a
  directory (sorted), each under its own header, then an aggregate line;
  exit code is nonzero if any file failed. Files with no `#[test]` entity
  report zero tests instead of failing an engine build.

### Stage 9 — Waveforms (`siox-wave`) — 🟢 partial
- **Acceptance:** counter VCD shows `clk/rst/en/count`; FSM shows symbolic/
  encoded states; struct fields are separate trace paths.
- **Status (done):** a traced run (`siox_sim::run_test_traced`) records a signal
  sample per simulation step; `siox-wave::write_vcd` emits a valid VCD
  (`$timescale`, `$scope`/`$var` per entity, `#time` value changes). `siox sim
  <file> --wave <out.vcd>` writes the counter's waveform (clk/rst/en/count over
  ~100 ns, count reaching 10).
- **Status (done, cont.):** struct fields and array elements appear as separate
  trace paths (`p.valid`, `a[2]`) since composite signals are flattened in the IR.
- **Status (done, cont.):** **symbolic values** — a logic scalar (`Bit`/`Logic`) dumps a 1-bit `0/1/z/x`; a named enum (an FSM `State`, `Bool`) dumps a
  VCD `string` var (`$var string 1 …`, `sIdle …`), so viewers show `Idle`/`Run`
  instead of a discriminant. Backed by `Design::enum_syms`.
- **Status (todo):** FST output for large designs.

### Stage 10 — Diagnostics & lints (`siox-diag` + all) — 🟢 (ongoing)
- **Acceptance:** every diagnostic has a code, a main span, a message, optional
  help, and related spans.
- **Status (done):** the infrastructure and error-code catalogue are in use; the
  CLI renders `severity[code]: message --> file:line:col`, help lines, and
  related-span labels. Errors carry actionable help + "did you mean?"
  suggestions (edit distance) and related spans (duplicate items). Warnings
  emitted: **non-exhaustive enum match** (`W-P007`), **unreachable match
  arm** (`W-P006`), **possible latch** (`W-P002`, a combinational signal only
  assigned under a condition), **combinational loop** (`W-P010`, a comb signal
  that depends on itself with no register in the path), and **unused import**
  (`W-P005`, per-file, std excluded). **Multiple drivers on an unresolved type** is a hard error (the
  `Resolve` safety rule), now surfaced by `check` (which elaborates + lowers).
- **Status (todo):** the remaining warnings — unused signal/param (the IR can't
  see a testbench's reads, so this needs use-tracking that spans the runner),
  suspicious `Logic` compare/reset.

### Stage 11 — Standard library (`std/`) — 🟢 (documented in docs/std.md)
- **Acceptance:** counter/FSM/stream/tests compile with standard imports only.
- **Status:** the CLI loads `std::` modules transitively from `--std <dir>`
  (default `./std`), mapping `std::a::b` → `<dir>/a/b.siox`. Imports resolve
  against the loaded modules' `pub` declarations; an import that matches
  nothing is a hard error (`E-P011`, with a "did you mean?" suggestion).
  Shipped modules: `std::logic` (canonical `enum` declarations of
  Bit/Logic/Bool + LOW/HIGH), `std::bits` (uint/int as derived Logic
  vectors, docs), `std::ops` (the `Boolean` condition trait, `as_bool ->
  integer`, 1 = true — no longer seeded), `std::attrs` (the five system
  attributes), `std::assert` (`Severity`), `std::sim` (`Time`/`Freq` with
  literal-suffix impls — `10ns`, `100MHz` — plus FS..MS integer constants),
  `std::math` (`Complex` over `real` — f64 in simulation — with `"+"`/`"-"`
  impls incl. mixed-operand `10 + 5i` and the `i` suffix; exercised by
  `complex_test`; core operator/Suffix/Prefix hooks are compiler bootstraps).
  `std::logic` now carries the **four-value truth tables** for
  core `and or not` and custom `xor nand nor xnor` on `Logic` as ordinary operator impls
  (X/Z propagation, `logic_test`) — std_logic_1164's core as
  library source. Full reference: docs/std.md.
  The kernel's base types are only `integer` and `real` (spec "type kernel");
  the checker/IR still recognize the std::logic/std::bits names intrinsically
  as a shim until operator overloading carries their semantics in source.
  `std_test` exercises every module through real imports.

### Stage 12 — CLI & workflow (`sioxc`) — 🟢
- **Acceptance:** `sioxc check` succeeds; `sioxc sim --wave` produces a waveform;
  `sioxc test` runs all; non-zero exit on failure.
- **Status:** rustc-shaped. A bare `sioxc <file>` compiles the `#[top]` design to
  a native object; `check`/`sim --wave` (JIT-traced)/`test` (JIT, libtest output,
  `--no-run` native binary)/`ir`/`ast`/`tree`/`tokens`/`emit-llvm` all work. LLVM
  is the default backend; `--features interp` adds the interpreter and
  `--backend interp`. (The cargo-like project layer is future `pcb`/`circuit`.)

## Recommended order

Per spec §7 — the shortest practical path. Strikethrough marks completed work.

1. ~~Syntax examples + grammar sketch (Stage 1)~~
2. ~~Lexer / parser / AST + pretty-printer (Stage 2)~~
3. ~~Name resolution (Stage 3)~~
4. Type checking (Stage 4) — *in progress*
5. Elaboration (Stage 5) — *in progress*
6. Digital IR (Stage 6) — *in progress*
7. Event-driven simulator (Stage 7) — *in progress*
8. Test runner + assertions (Stage 8) — *in progress*
9. Waveform output (Stage 9) — *in progress*

The whole pipeline now runs end to end (source → simulate → assertions +
waveforms). Remaining work is **filling gaps**: Stage 10 warnings/lints,
Stage 11 stdlib, and deeper per-stage coverage.
10. Diagnostics polish (Stage 10)
11. Standard library cleanup (Stage 11)

Do not start analogue (Phase 2) until the digital simulator is stable enough to
support tests, clocks, events, and waveforms.

Beyond Phase 1 stages: the **LLVM backend** (`siox-llvm`, inkwell, `llvm`
feature **on by default**) is the execution engine, designed in
[notes/llvm-backend.md](notes/llvm-backend.md) and built:

- **B0 validator** ✅ — `Design::validate` gates codegen against bad ids /
  `Unknown` / unknown widths / bad slices; div-by-zero → 0 on both engines.
  (Signedness is *not* compiler work — it moves to std operator impls, #37.)
- **B1 process extraction** ✅ — `Design::processes` (sensitivity + write sets).
- **B2 emitter** ✅ — combinational + sequential codegen (full delta cycle);
  `Expr`→builder 1:1; `module.verify()`-clean.
- **B3 JIT** ✅ — `sioxc test` runs the compiled design in-process (default).
- **B4 differential harness** ✅ — JIT matches the interpreter oracle
  signal-for-signal across the expression surface (`--features interp`).
- **B5 AOT** ✅ — `emit_object` writes a native `.o`; `sioxc <file>` compiles a
  top; `sioxc test --no-run` links a standalone native test binary.

The default `cargo build` stays LLVM-free (feature-gated); the interpreter
is the differential oracle. Type-level optimization of `uint[]`/`int[]`
(exact-width `iN`) is deliberately later — those types are slated to be
softcoded by std.

## Phase 1 "done" checklist (spec §6)

The project is Phase-1 complete when it can:

- [x] Parse the Phase 1 syntax
- [x] Resolve modules, names, attributes, and paths
- [~] Type-check entities, structs, enums, traits, impls *(core checks done)*
- [~] Elaborate parameterized entities into a concrete hierarchy *(in progress)*
- [x] Lower designs into digital simulation IR
- [x] Simulate combinational and sequential behavior
- [x] Support `::event` and `::old` on all digital/discrete values
- [x] Run `#[test]` entities
- [x] Evaluate assertions
- [x] Export waveforms
- [~] Report useful diagnostics *(errors done; warnings pending)*
