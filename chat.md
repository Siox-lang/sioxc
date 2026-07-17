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
