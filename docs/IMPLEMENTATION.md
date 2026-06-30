# siox — Phase 1 Implementation Plan

`siox` ("silicon oxide") is a digital hardware description language and
simulator. This document tracks **what needs to be implemented** for Phase 1
(the digital core) and maps each spec stage to the crate that owns it.

> The frozen language design lives in [`siox_phase1_digital_spec.md`](siox_phase1_digital_spec.md)
> (written before the language was named `siox`; treat every "mhdl" there as
> "siox"). The roadmap for Phases 2–3 is in
> [`siox_three_phase_roadmap.md`](siox_three_phase_roadmap.md).

Phase 1 is **simulation-first**: no analogue, no schematic/design layer, no
synthesis backend.

---

## Workspace layout

The compiler is a Cargo workspace. Data flows top-to-bottom; each crate depends
only on the ones above it (plus `siox-diag`, which everything uses).

| Crate          | Spec stage(s) | Responsibility                                            | Status |
| -------------- | ------------- | -------------------------------------------------------- | ------ |
| `siox-diag`    | 10            | Spans, source map, diagnostics, error-code catalogue      | 🟡 skeleton |
| `siox-syntax`  | 1, 2          | Lexer, tokens, AST, parser, pretty-printer                | 🟡 skeleton |
| `siox-resolve` | 3             | Module tree, `using` imports/aliases, path resolution     | 🔴 stub |
| `siox-types`   | 4             | Type/kind checking, system-attribute typing               | 🔴 stub |
| `siox-elab`    | 5             | Entity specialization, instance hierarchy, bus modes      | 🔴 stub |
| `siox-ir`      | 6             | Digital simulation IR (drivers, event blocks, next-state) | 🔴 stub |
| `siox-sim`     | 7, 8          | Event-driven delta-cycle simulator, test runner           | 🔴 stub |
| `siox-wave`    | 9             | VCD (later FST) waveform export                            | 🔴 stub |
| `siox-cli`     | 12            | `siox` binary: `check/parse/sim/test/ast/ir/tree`         | 🟡 skeleton |

Legend: 🔴 stub (signature only) · 🟡 skeleton (types defined, logic TODO) ·
🟢 working · ✅ done with acceptance tests.

---

## Stage-by-stage work

Each stage below lists concrete work items and the **acceptance criteria** that
mark it done (lifted from the spec). Check items off as they land.

### Stage 1 — Syntax freeze (`siox-syntax`, `docs/`)
- [ ] Write `docs/syntax.md`: frozen grammar sketch + 10–20 valid examples.
- [ ] Decide exact syntax for: comments, modules, imports, type aliases,
      parameter lists, structs, enums, entities, impls, traits, trait impls,
      attribute decls/applications, fn/method decls, assignments, if/else,
      match, static-range loops, instance construction, array/range syntax,
      literals, `::` paths, `.` field access.
- **Acceptance:** the 10 required examples (counter, register, mux, FSM,
  ready/valid producer + consumer, enum monitor, test entity, extern entity,
  attribute usage) have final syntax. *(see `examples/`)*

### Stage 2 — Lexer & parser (`siox-syntax`)
- [ ] `lexer.rs`: tokenization with spans + error recovery.
- [ ] `parser.rs`: module/type/expr/stmt/attr parsers + entity/impl/trait/
      struct/enum/instance/pattern parsers.
- [ ] `ast.rs`: finalize node shapes (skeleton already drafted).
- [ ] `pretty.rs`: round-tripping pretty-printer.
- **Acceptance:** valid examples parse; invalid syntax gives useful spans;
  parser recovers after common mistakes; pretty-printer round-trips simple
  examples. `siox parse examples/counter.siox` prints a stable AST.

### Stage 3 — Name resolution (`siox-resolve`)
- [ ] Module namespace tree; `using` imports; type aliases; pub/private;
      `::` path resolution; associated items (`State::Idle`); trait/impl/
      instance/attribute name resolution.
- **Acceptance:** unknown names reported; ambiguous imports reported; private
  items inaccessible across modules; undeclared attribute usage fails;
  `State::Idle` resolves.

### Stage 4 — Type & kind checking (`siox-types`)
- [ ] Primitive digital types, integer widths, structs, enums, arrays, entity
      types, directional views, bus modes, fn signatures, trait bounds,
      attribute value typing, pattern typing.
- [ ] System attributes: `::event`/`::old` on all digital values; range attrs
      (`::width/::range/::high/::low/::left/::right/::direction`) on ranges.
- [ ] Reject `::ddt` and other Phase-2 analogue syntax.
- **Acceptance:** no implicit `uint[8]`→`uint[16]`; undeclared/mis-targeted
  attributes rejected; cannot write `in` ports; cannot read undriven outputs
  where detectable; invalid method calls rejected; bare `Logic` condition
  rejected (if rule kept).

### Stage 5 — Elaboration (`siox-elab`)
- [ ] Parameter substitution; instance creation; port connection (+ `.clk`
      shorthand); nested hierarchy; extern stubs; bus-mode expansion;
      direction checking; const-expr evaluation for parameters.
- **Acceptance:** all params known post-elab; all required ports connected or
  defaulted; direction violations reported; bus modes expand to leaf
  permissions; extern entities are black boxes; `siox tree` prints the tree.

### Stage 6 — Digital IR (`siox-ir`)
- [ ] Represent signals/state, combinational `Driver`s, `EventBlock`s,
      next-state updates, instance connections, system-attribute reads,
      resolved method calls, match, assertions.
- [ ] Lower `clk::rising` → `Event(clk) && Old(clk)=='0' && Current(clk)=='1'`.
- **Acceptance:** event vs. combinational deps explicit; sequential updates
  separated from immediate local assignments; `::event`/`::old` represented
  directly; `siox ir` prints normalized IR.

### Stage 7 — Simulator core (`siox-sim`)
- [ ] Current/old/event state; delta cycle; driver eval; next-state queue;
      commit phase; wakeup scheduling; stable-state detection.
- **Acceptance:** correctly simulates mux, register, counter, FSM, ready/valid
  handshake, enum monitor (`::old`), struct event, array element event.

### Stage 8 — Tests, assertions, stimulus (`siox-sim`)
- [ ] `#[test]` entity discovery; `wait <t>`, `tick(clk)`, `assert!(cond,msg)`;
      time advance; clock stimulus.
- **Acceptance:** passing assertions report success; failures report
  file/span/message; multiple tests run; `siox test examples/` works.

### Stage 9 — Waveforms (`siox-wave`)
- [ ] Record changes with hierarchy paths; enums as symbolic names; struct
      fields recursively; VCD export (FST later).
- **Acceptance:** counter VCD shows `clk/rst/en/count`; FSM shows symbolic/
  encoded states; struct fields are separate trace paths.

### Stage 10 — Diagnostics & lints (`siox-diag` + all)
- [ ] Errors: unknown name, duplicate item, type mismatch, write-to-input,
      missing connection, bad attr target/value, bad method call, bad pattern,
      Phase-2 syntax use.
- [ ] Warnings: multiple drivers, possible latch, unused signal/param/import,
      unreachable arm, non-exhaustive match, suspicious `Logic` compare,
      suspicious reset.
- **Acceptance:** every diagnostic has code + main span + message + optional
  help + related spans (see `codes` in `siox-diag`).

### Stage 11 — Standard library (`std/`)
- [ ] `std::logic` (`Bit`, `Logic`, `Bool`, `Clock`, `ClockLike`),
      `std::bits` (`uint[N]`, `int[N]` + ops), `std::attrs`, `std::sim`,
      `std::assert`.
- **Acceptance:** counter/FSM/stream/tests compile with standard imports only
  (no compiler magic beyond primitives + system attributes).

### Stage 12 — CLI & workflow (`siox-cli`)
- [ ] `check`, `parse`, `sim`, `test` + debug `ast`, `ir`, `tree`.
- **Acceptance:** `siox check examples/counter.siox` succeeds; `siox sim ...
  --wave counter.vcd` produces a waveform; `siox test examples/` runs all;
  nonzero exit on failure.

---

## Recommended implementation order

Per spec §7 — the shortest practical path:

1. Syntax examples + grammar sketch (Stage 1)
2. Lexer / parser / AST (Stage 2)
3. Pretty-printer (Stage 2)
4. Name resolution (Stage 3)
5. Type checking (Stage 4)
6. Elaboration (Stage 5)
7. Digital IR (Stage 6)
8. Event-driven simulator (Stage 7)
9. Test runner + assertions (Stage 8)
10. Waveform output (Stage 9)
11. Diagnostics polish (Stage 10)
12. Standard library cleanup (Stage 11)

Do not start analogue (Phase 2) until the digital simulator is stable enough to
support tests, clocks, events, and waveforms.

---

## Phase 1 "done" checklist (spec §6)

The project is Phase-1 complete when it can:

- [ ] Parse the Phase 1 syntax
- [ ] Resolve modules, names, attributes, and paths
- [ ] Type-check entities, structs, enums, traits, impls
- [ ] Elaborate parameterized entities into a concrete hierarchy
- [ ] Lower designs into digital simulation IR
- [ ] Simulate combinational and sequential behavior
- [ ] Support `::event` and `::old` on all digital/discrete values
- [ ] Run `#[test]` entities
- [ ] Evaluate assertions
- [ ] Export waveforms
- [ ] Report useful diagnostics
