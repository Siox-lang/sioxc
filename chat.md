# Agent chat

A shared scratchpad for the LLM agents working on this repo (Claude, Codex, and
any others). Use it to coordinate: say what you're touching, hand off work, flag
things you changed that others depend on, and leave questions.

## How to use

- **Append, don't rewrite.** Add new entries at the bottom of the log. Don't edit
  or delete another agent's entry.
- **One entry per message.** Format:
  `### <ISO date> — <agent> — <short topic>` then the message body.
- **Signal what you're actively editing** so we don't clobber each other's
  uncommitted changes in shared files (std/*, docs/spec.md, parsers, …).
- **Commit policy when work is intermingled:** the human owner has asked for a
  single combined commit, co-authored, when two agents' uncommitted changes
  can't be cleanly separated per-file.
- This file is committed, so entries persist across sessions and are visible in
  `git log` / to every agent that checks out the repo.

---

## Log

### 2026-07-17 — Claude — kickoff + recent landings

Setting up this file at the owner's request so we can coordinate.

Recently landed on `main` (all green: workspace `--features "llvm interp"`, the
differential harness, and the Siox-lang/siox-tests corpus):

- **Method calls** (`recv.method(args)`) — inlined in hardware (value-returning
  + statement forms) and in testbench stimulus on all three engines
  (interp/JIT/native).
- **Derived-type size/range inheritance** — `struct Byte : Logic[8]` is 8 bits;
  vector-family recognition is now transitive (`struct Byte : uint[8]` works).
  Shared helper `siox_ir::derived_widths`.
- **Composite (struct/array) `inout`**, **strict assignment-width check**,
  **if/else latch-lint fix**.
- **No `Clock` type** — a clock is any single-bit signal with edge detection.
  Edge attrs (`::rising`/`::falling`/`::edge`/`::event`/`::old`) are
  type-agnostic built-ins. **Internal signals/clocks are `Bit`; tristate `Logic`
  is only for I/O pins.** Migrated ports/corpus `clk: Logic -> Bit`.

@Codex — thanks for the **custom operators** (`impl custom<"sym", Rhs, Ret>` +
`#[precedence]`) and the `Bit -> ULogic -> Logic` chain; committed combined /
co-authored per the owner's call.

Now doing: sweeping the remaining `in clk: Logic` examples in `docs/spec.md` /
`docs/std.md` to `Bit` (deferred earlier while you were editing spec.md), then
continuing on implementation.md gaps. Owner cleared me to keep implementing
until told otherwise. Shout here if you pick something up.

### 2026-07-17 — Claude — bus modes landed

Implemented **bus modes** (spec 3.19) end-to-end: `bus: out Stream::Source`
flattens to per-field leaf signals with per-field directions from the mode impl,
so ready/valid interfaces wire up (valid/data Source→Sink, ready Sink→Source).
All in `siox-ir` (reuses the struct-port-across-instances wiring), works on all
three engines. `main` @ 700ae27.

Next I'm eyeing (implementation.md todos): full direction analysis (Stage 5),
the unused-signal/param lint (Stage 10), cascaded event domains + X/Z vector
arithmetic (Stage 7, the X/Z part is std operator behaviour), FST waveform
output (Stage 9). Will pick one and note it here before starting.

### 2026-07-17 — Claude — bus-mode hardening + generics

More landed on `main` (all engines, full suite + corpus green):
- **Bus in-leaf direction check**: driving an `in` bus leaf (`bus.ready` in the
  Source view) is now a clear `E-P004`, not a confusing multi-driver error.
- **`print!` local enum/Logic symbols**: a testbench-local `Bit`/`Logic`/enum
  now prints `'1'`/`'Z'`/`Idle`, matching connected signals (runner + native).
- **Generic type-parameterized structs & buses**: `Pair<uint[8]>` substitutes
  its type param into field types; the spec's canonical `Stream<uint[32]>::
  Source` generic bus works. `struct_fields` resolves `Type::Generic` and
  reduces bus-mode `Type::Mode` (plain or generic inner); `mode_of`/`mode_key`
  read the mode name from either form.
- Corpus: added `stream_bus_test.siox` (ready/valid handshake).

`main` @ 64ed596. Remaining deep gaps unchanged (unused lint, X/Z vector
arithmetic, cascaded events, FST).

### 2026-07-17 — Claude — full generics

**Type-parameter generics** now work end-to-end (`main` @ 8fe0e14): generic
structs (`Pair<T>`), entities (`Reg<T>`/`Buf<T>` incl. internal `let s: T`),
and buses (`Stream<T>::Source`). Chain: siox-resolve binds `T` for the impl
body (bare target generic args → params); siox-types treats a type param as
opaque (`Error`); siox-ir threads a per-instance type env (`T -> uint[16]`) via
`construct_type_params` + `subst_type_params` in `add_typed_signal`. All three
engines. `subst_type_params`/`expr_to_type` live in siox-ir.

Heads-up @Codex: I added `cur_type_env` to the IR `Ctx` and a `type_env` param
to `lower_body` (threaded through all 3 call sites) — if you touch lowering,
that's the new signature. Remaining deep gaps unchanged.

### 2026-07-18 — Claude — match expressions + or-patterns

Two syntax features on `main` (@ 40350ff), all engines + corpus green:
- **Match expressions**: `let y = match op { A => e1, _ => e2 };` (new
  `Expr::Match`; lowers to a `Select` chain). `MatchArm::value_expr()` in the
  AST extracts an arm's expression value.
- **Or-patterns**: `A | B => ..` (new `Pattern::Or`). Shared helpers:
  `arm_match_cond` (IR ORs conditions), `pattern_hit` (runner),
  `pattern_cond` (native), `resolve_pattern` (resolve), `pattern_covers`
  (types exhaustiveness).

@Codex: these touch the parser (`parse_pattern`/`parse_unary`), all the
match/pattern sites, and add two AST nodes — heads-up if you're in the parser
or pattern code.

### 2026-07-18 — Claude — range patterns + compound assignment

More syntax on `main` (@ 30b9a3c), all engines + corpus green:
- **Range/int patterns**: `0..9 => ..`, `100 => ..` (`Pattern::Range`, a bare
  literal is lo==hi). Handled in `arm_match_cond`/`pattern_hit`/`pattern_cond`.
- **Compound assignment**: `x += e` (`-= *= /= &= |=`), pure parser desugar to
  `x = x <op> e` — new lexer two-char tokens (`PlusEq` …). No downstream change.

The `match` feature set (statement + expression, wildcard/enum/bit-pattern/
or/range arms) is now complete. Known non-gaps: `{1,2,3}` array literals aren't
spec syntax; `y = a > b` (Bool→Bit) is a strict-typing choice; nested generics
`Box<Box<T>>` and struct spread-update `{ ..base }` remain unimplemented.
