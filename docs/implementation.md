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
| `siox-ir`      | 6    | 🟢 partial | language-neutral IR; lowers behaviour to drivers + event blocks; `siox ir` |
| `siox-sim`     | 7, 8 | 🟢 partial | delta-cycle simulator (Stage 7) + `#[test]` runner with `assert!` (Stage 8) |
| `siox-wave`    | 9    | 🟢 partial | VCD waveform export from a traced run; `siox sim --wave` |
| `siox-cli`     | 12   | 🟢 working | `tokens`/`parse`/`ast`/`check`/`tree`/`ir`/`test`/`sim` (incl. `sim --wave` VCD) |

## Stage-by-stage

Each stage lists its acceptance criteria (from the spec) and current status.

### Stage 1 — Syntax freeze (`siox-syntax`, `docs/`) — 🟢
- **Acceptance:** the required examples (counter, register, mux, FSM,
  ready/valid, enum monitor, test entity, extern entity, attribute usage) have
  final syntax.
- **Status:** grammar is implemented in the parser; `examples/counter.siox` and
  `examples/counter_tb.siox` exist. More example programs still to be added as
  regression fixtures.

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
  enum-aware), and the `::ddt` Phase-2 guard (`E-P010`).
- **Status (deferred):** concrete-width mismatch (`uint[8]`→`uint[16]`) needs
  elaboration-time widths fed back in; method-call resolution needs the method
  tables. Both become tractable now that elaboration substitutes widths.

### Stage 5 — Elaboration (`siox-elab`) — 🟢 partial
- **Acceptance:** all params known post-elab; all required ports connected;
  direction violations reported; bus modes expand to leaf permissions; extern
  entities are black boxes; `siox tree` prints the tree.
- **Status (done):** instance hierarchy from `#[top]`/`#[test]` roots, parameter
  const-evaluation and substitution into concrete port types (`uint[W]` →
  `uint[8]`), `.clk`-shorthand connection resolution, missing-port (`E-P005`) and
  unknown-port checks, **port-connection width checking** (a port's width must
  match the signal it connects to, `E-P003`), extern black boxes, cycle
  detection, and `siox tree`.
- **Status (todo):** generated instances (loops/arrays), bus-mode leaf
  expansion, full direction analysis, and propagating concrete parameter widths
  down into instance signal types.

### Stage 6 — Digital IR (`siox-ir`) — 🟢 partial
- **Acceptance:** event vs. combinational deps explicit; sequential updates
  separated from local assignments; `::event`/`::old` represented directly;
  `siox ir` prints normalized IR.
- **Status (done):** a **language-neutral** IR (its own `BinOp`/`UnOp`, no AST
  imports — see the convergence-layer goal). `lower` walks each non-extern
  entity's behaviour into signals, combinational `Driver`s, and `EventBlock`s;
  detects event-controlled blocks (`::event`/`::rising`) and expands
  `clk::rising` to `Event(clk) && Old(clk)=='0' && Current(clk)=='1'`; nested
  `if`/`else` priority accumulates into next-state conditions. Signal widths are
  made concrete by substituting the entity's instance parameters (`uint[W]` with
  `W=8` -> width 8). `siox ir` prints it.
- **Status (todo):** cross-instance flattening/connections and multiple
  instances of one entity with differing params (widths come from the *first*
  instance today); match/index/slice/concat/method-call lowering; instance `let`
  bindings are currently listed as signals.

### Stage 7 — Simulator core (`siox-sim`) — 🟢 partial
- **Acceptance:** correctly simulates mux, register, counter, FSM, ready/valid
  handshake, enum monitor (`::old`), struct/array element events.
- **Status (done):** the delta-cycle `Simulator` over the IR `Design`:
  current/old/event state, IR-expression evaluation (`Event`/`Old`/`Current`,
  logical/comparison/arithmetic ops), combinational fixpoint, event blocks fired
  once per edge with next-state semantics, value masking to the signal width
  (arithmetic wraps at `2^width`), and `set`/`read`/`settle`/`advance`. The
  counter simulates correctly (increments on rising edges, sync reset, enable
  gating, wrap-around).
- **Status (todo):** verified against the remaining acceptance designs (mux/FSM/
  ready-valid/enum/struct/array); proper logic-value (X/Z) modelling; cascaded
  event domains. Driving it from a `#[test]` entity is Stage 8.

### Stage 8 — Tests, assertions, stimulus (`siox-sim`) — 🟢 partial
- **Acceptance:** passing assertions report success; failures report
  file/span/message; multiple tests run; `siox test examples/` works.
- **Status (done):** `run_tests` discovers `#[test]` entities, maps their
  signals to the DUT via the elaborated connections, and interprets the
  stimulus (`let` initials, assignments, `tick(clk)`, `wait`, `for` over a
  static range, `if`, `assert!(cond, "msg")`) against the simulator. `siox test
  [name]` runs all or a name-filtered subset, prints `PASS`/`FAIL` with the
  failing assertion's `file:line:col`, and exits nonzero on failure.
- **Status (todo):** `siox test <dir>` over a directory; `wait`/time-based
  stimulus; richer stimulus (clock generators, `i` in `for` bodies).

### Stage 9 — Waveforms (`siox-wave`) — 🟢 partial
- **Acceptance:** counter VCD shows `clk/rst/en/count`; FSM shows symbolic/
  encoded states; struct fields are separate trace paths.
- **Status (done):** a traced run (`siox_sim::run_test_traced`) records a signal
  sample per simulation step; `siox-wave::write_vcd` emits a valid VCD
  (`$timescale`, `$scope`/`$var` per entity, `#time` value changes). `siox sim
  <file> --wave <out.vcd>` writes the counter's waveform (clk/rst/en/count over
  ~100 ns, count reaching 10).
- **Status (todo):** enum values as symbolic names; struct fields as separate
  paths; FST.

### Stage 10 — Diagnostics & lints (`siox-diag` + all) — 🟢 (ongoing)
- **Acceptance:** every diagnostic has a code, a main span, a message, optional
  help, and related spans.
- **Status:** the infrastructure and the error-code catalogue exist and are in
  use; the CLI renders `severity[code]: message --> file:line:col`. Warnings
  (multiple drivers, latches, unused items, non-exhaustive match, ...) are not
  yet emitted.

### Stage 11 — Standard library (`std/`) — 🔴 (started)
- **Acceptance:** counter/FSM/stream/tests compile with standard imports only.
- **Status:** `std/ops.siox` declares the `Boolean` trait (boolean
  representation for conditions). The compiler does not load `std/` yet, so
  primitives, `std::attrs`, and the built-in `Boolean` impls (`Bit`/`Bool`) are
  seeded as compiler builtins in the meantime (see architecture.md).

### Stage 12 — CLI & workflow (`siox-cli`) — 🟢
- **Acceptance:** `siox check` succeeds; `siox sim --wave` produces a waveform;
  `siox test` runs all; non-zero exit on failure.
- **Status:** `tokens`/`parse`/`ast`/`check`/`tree` work; `sim`/`test`/`ir` run
  the pipeline as far as it goes and report the first unimplemented stage.

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

## Phase 1 "done" checklist (spec §6)

The project is Phase-1 complete when it can:

- [x] Parse the Phase 1 syntax
- [x] Resolve modules, names, attributes, and paths
- [~] Type-check entities, structs, enums, traits, impls *(core checks done)*
- [~] Elaborate parameterized entities into a concrete hierarchy *(in progress)*
- [ ] Lower designs into digital simulation IR
- [ ] Simulate combinational and sequential behavior
- [ ] Support `::event` and `::old` on all digital/discrete values
- [x] Run `#[test]` entities
- [x] Evaluate assertions
- [x] Export waveforms
- [~] Report useful diagnostics *(errors done; warnings pending)*
