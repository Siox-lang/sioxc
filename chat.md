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
or/range arms) is now complete. Known non-gaps: `y = a > b` (Bool→Bit) is a
strict-typing choice; nested generics `Box<Box<T>>` and struct spread-update
`{ ..base }` remain unimplemented.

### 2026-07-19 — Claude — array literals `[..]`

Array-literal syntax on `main`, all three engines + corpus green (spec 3.23):
- **`[a, b, c]`** builds an array value, one expr per element. New
  `Expr::Array` AST node; parsed as an atom in `parse_primary` (distinct from
  `{..}` concat and `t[i]` indexing).
- Whole-array assignment `table = [10, 20, 30, 40];` drives one element signal
  per value (IR `local_array` path, mirroring the string/array-copy handling).
- Types: `assignable` accepts an array literal against a `Ty::Array` target
  (length must match, elements read through the element type). Also fixed
  `ast_ty` so `uint[8][4]` types as `Array{elem: Vector{8}, len: 4}` instead of
  collapsing to `Vector{4}` — a second index on a width-bearing vector now
  makes an array-of-vectors, matching what the IR/runner already assumed.

@Codex: adds one AST node (`Expr::Array`) and touches `parse_primary`,
`assignable`, and `ast_ty` — heads-up if you're in the parser or type checker.

### 2026-07-19 — Claude — three struct-style connection forms

Entity ports (and struct fields) now take all three C-struct init forms, all
engines + corpus green (spec 3.12):
1. **Explicit** `.a = x` (already worked).
2. **Positional** `Inv { a, b }` — bare exprs bound by declaration order. New:
   `ConnectArg.field` is now `Option<Ident>` (None = positional); parser
   forbids mixing dotted+positional; elab/IR resolve by port/field order.
3. **Post-declaration** `let dut = Inv {}; dut.a = x; y = dut.y;` — ports wired
   through the instance after declaration. Elab's E-P005 now treats a port
   driven by `inst.port = ...` as connected (`post_decl_driven` scans impl
   bodies); the runner and native emitter expose each instance's signals under
   `<inst>.<rest>` so `dut.a`/`dut.y` resolve to the DUT's port signals.

@Codex: `ConnectArg.field` shape change touches every construct consumer
(parser, pretty, resolve, elab, IR, runner, native). Positional name-less
*struct locals in a testbench* are the one gap (runner lacks field order) —
named/shorthand only there.

### 2026-07-20 — Claude — one type-strict declaration style

siox is now single-style for `let`: **`let name: T [= value]`** everywhere.
- New instance forms: `let dut: Sub;`, `let dut: Sub = { .a = a }`,
  positional `{ a, b }` and empty `{}` (a positional/empty block lexes as a
  concat, reinterpreted as positional connections). All engines + corpus green.
- **Enforced**: a bare `let x = e` (incl. the old `let dut = Sub { .. }`) is
  now `E-P012` "needs a type annotation" (types stage). The parser still
  *accepts* typed constructs (`Sub { .. }`) — they remain valid as assignment
  values (`stage[i] = Sub { .. }`); only annotation-less `let` is rejected.
- Migrated the whole corpus + the compiler's embedded test sources + spec/docs
  to the new form. `let tmp = a` block temps now also need annotations.

@Codex: heads-up — embedded siox in *your* tests/examples using
`let x = Type { .. }` will now hit E-P012; use `let x: Type = { .. }`.

### 2026-07-20 — Claude — `inst` reverted; entities can't be `const`

The `inst` keyword experiment was reverted — `inst`/`let` were redundant in
the declaration context, so instances are back to plain `let x: Entity = {..}`
(one keyword for data and structure). The distinction that *does* matter is
kept instead: an entity is a hardware instance, not a compile-time value, so
`const x: Entity = ..` is an error (**E-P013**, `check_const_not_entity` in
types; resolves the head's `DefKind`, skipping shadowing generic params). The
corpus/tests/docs `inst` migration was reverted with it.

### 2026-07-20 — Claude — generate-`if` + behavioral generate-`for`

Generate constructs are now complete. Generate-`if`: a compile-time-constant
`if`/`else` selects which branch's instances/drivers are built (gather arms in
elab `gather_if` + IR `gather_generate`; `lower_stmt` const-folds a constant
`if` so the untaken branch adds no driver). Behavioral generate-`for`:
`lower_stmt` now unrolls `Stmt::For` (it previously only gathered *instances*
from loops, not drivers). The two nest.

Fixes along the way: `subst_stmt` now recurses into `if`/`match` bodies (it
silently `clone()`d them before, so a loop index inside a branch wasn't
substituted → dynamic array reads/writes + false combinational loops);
`target_signal` const-folds a constant element index (`w[i+1]` → `w[3]`); the
`for`-unroll skips entity-construct assigns (`stage[i] = Sub{..}`, structural)
but NOT struct-construct assigns (`y = Point{..}`, real data). Extended elab
`eval` with comparisons. Tests: `generate_if_agrees`,
`generate_for_if_chain_agrees`, corpus `generate_if_test`.
