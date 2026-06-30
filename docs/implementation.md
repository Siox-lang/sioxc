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
| `siox-types`   | 4    | 🟢 partial | type-inference core; Logic-condition, attr target/value, input-write, assignment/init compatibility, `::ddt` |
| `siox-elab`    | 5    | 🟢 partial | instance hierarchy, param const-eval + substitution, connection width checking |
| `siox-ir`      | 6    | 🔴 stub | |
| `siox-sim`     | 7, 8 | 🔴 stub | |
| `siox-wave`    | 9    | 🔴 stub | |
| `siox-cli`     | 12   | 🟢 working | `tokens`/`parse`/`ast`/`check`/`tree`; `sim`/`test`/`ir` report where the pipeline stops |

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
  table, `type_of`); `Logic`-as-bare-condition (`E-P003`), attribute target
  (`E-P006`) and value (`E-P007`), write-to-input-port (`E-P004`), assignment
  and initializer compatibility / no-implicit-conversion (`E-P003`, literal- and
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

### Stage 6 — Digital IR (`siox-ir`) — 🔴
- **Acceptance:** event vs. combinational deps explicit; sequential updates
  separated from local assignments; `::event`/`::old` represented directly;
  `siox ir` prints normalized IR.

### Stage 7 — Simulator core (`siox-sim`) — 🔴
- **Acceptance:** correctly simulates mux, register, counter, FSM, ready/valid
  handshake, enum monitor (`::old`), struct/array element events.

### Stage 8 — Tests, assertions, stimulus (`siox-sim`) — 🔴
- **Acceptance:** passing assertions report success; failures report
  file/span/message; multiple tests run; `siox test examples/` works.

### Stage 9 — Waveforms (`siox-wave`) — 🔴
- **Acceptance:** counter VCD shows `clk/rst/en/count`; FSM shows symbolic/
  encoded states; struct fields are separate trace paths.

### Stage 10 — Diagnostics & lints (`siox-diag` + all) — 🟢 (ongoing)
- **Acceptance:** every diagnostic has a code, a main span, a message, optional
  help, and related spans.
- **Status:** the infrastructure and the error-code catalogue exist and are in
  use; the CLI renders `severity[code]: message --> file:line:col`. Warnings
  (multiple drivers, latches, unused items, non-exhaustive match, ...) are not
  yet emitted.

### Stage 11 — Standard library (`std/`) — 🔴
- **Acceptance:** counter/FSM/stream/tests compile with standard imports only.
- **Status:** `std/` is empty; primitives and `std::attrs` are seeded as
  compiler builtins in the meantime (see architecture.md).

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
6. **Digital IR (Stage 6) — next**
7. Event-driven simulator (Stage 7)
8. Test runner + assertions (Stage 8)
9. Waveform output (Stage 9)
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
- [ ] Run `#[test]` entities
- [ ] Evaluate assertions
- [ ] Export waveforms
- [~] Report useful diagnostics *(errors done; warnings pending)*
