# Bug-hunt log

A running record of the systematic post-0.1.0 bug hunt: what was probed, what
broke, what was fixed, and what is still open. Newest round last. Method notes
at the bottom.

## Round 1 — width / signedness / masking (fixed in `2776eb5`)

Probes: wrap at 2^8 and 2^64, shift ≥ width, signed division and comparison.

| # | Finding | Verdict |
|---|---------|---------|
| 1.1 | `let c: uint[8]; c = 255 + 1` gave **256** in a testbench (hardware wraps to 0) — the runner stored locals as raw u128 with no width. | **Fixed**: locals record their vector-family width; every write masks. |
| 1.2 | `let a: int[8] = 0 - 7` drove the 8-bit DUT input as 2^64−7 — the runner's `engine.set` didn't mask. Downstream, the int sign-tests saw garbage and `-7 / 2` returned **252** instead of −3. | **Fixed**: runner signal writes mask to the signal's declared width. `-7/2` = 253 (−3, truncation toward zero) on all engines. |
| 1.3 | `-7 / 2`, `-7 < 2` on testbench **locals** give unsigned results — the runner never dispatches std operator impls. Hardware is correct. | **Open** (task #20). Design-level: the testbench evaluator needs operator-impl dispatch. |

Verified correct along the way: hardware wrap/shift masking, division by zero
(→ 0, deterministic), `std::bits` int Div/Ord impl logic (once inputs are
masked).

## Round 2 — patterns, fanout, lint precision (fixed in `c3614a6`)

Probes: bit-pattern match, operator precedence, cross-width ops, two DUTs
sharing a testbench name.

| # | Finding | Verdict |
|---|---------|---------|
| 2.1 | `match op { b"01??" => … }` **didn't parse** — spec 3.22 promises bit patterns, the AST node existed, the parser never produced it. | **Fixed**: patterns parse (`b"…"`, `x"…"`, `?` don't-cares, `_` separators) and lower to `(scrut & mask) == value`, first-match priority, comb + clocked. Shared decoder: `siox_ir::bit_pattern_mask`. |
| 2.2 | A testbench name connected to **several DUT ports drove only the last one** (`.x` into two instances; one clock into many). The signal map kept one binding per name. | **Fixed**: alias multimap; assignments, delayed one-shots, and `clk = not clk after` clocks fan out to every connected port. |
| 2.3 | The possible-latch lint **false-fired on combinational `match` with a wildcard arm** (all lowered drivers conditional despite exhaustive arms). | **Fixed**: wildcard-arm assignments count as the match's default. |
| 2.4 | String escapes (`\"`, `\t`, `\\`) are kept **raw** — the lexer skips an escaped quote for termination but nothing unescapes the text. | **Open** (task #21). |
| 2.5 | A `match` statement in a **testbench body is silently skipped** (`exec` has no `Stmt::Match` arm; the native emitter same). | **Open** (task #22). |
| 2.6 | `a xor (0xFF and 0x0F)` rejected — a compound literal expression doesn't inherit the integer-literal mask exemption. | **Open question** (types policy; the workaround is writing the folded constant). |

Verified correct: precedence (`and` > `xor` > `or`), division by zero, slices
in both directions, nested concat, cross-width widening arithmetic.

## Round 3 — metadata, unary ops, local arrays (fixed in `dbd0f99`)

Probes: `::width`/`::len`, ranged numerics, unary operators.

| # | Finding | Verdict |
|---|---------|---------|
| 3.1 | `not a` on a `uint[8]` was **boolean** (gave 0, not 0xFE) — the binary boolean ops were made per-bit earlier; unary `not` was missed. | **Fixed**: `not` on a vector-valued signal reference lowers to `x xor mask`; 1-bit operands, compound conditions, and enum-typed signals keep the boolean/impl form. |
| 3.2 | `x::width` in a hardware expression lowered to `Unknown`, so the JIT refused the **whole design** — and with no fallback engine, the test reported nothing. | **Fixed**: `::width` folds to the signal's width constant (like `::len`). |
| 3.3 | A value-less `let xs: uint[8][5];` local had **no element slots**: `xs[i]` writes and `xs::len` (= 0) misbehaved silently. | **Fixed**: the let creates one slot per element — while carefully *not* shadowing DUT-connected arrays (`xs[0]` in the signal map), which the first version of the fix broke (caught by the corpus). |

Verified correct: ranged-numeric dynamic checks (`integer<0..10>` violation
fires with signal/value/time), `-a` negation on vectors.

## Round 4 — silent lowering holes (fixed in `0dc5abe`)

Probes: concat as assignment target, deep if-expr nesting, generic widths.

| # | Finding | Verdict |
|---|---------|---------|
| 4.1 | `{hi, lo} = w;` parsed, type-checked, then the lowering **silently dropped it** — outputs stayed 0 with no diagnostic. | **Fixed**: concat targets unpack MSB-first (each part takes its width's slice), in combinational drivers and clocked next-state updates. |
| 4.2 | Any assignment target the lowering didn't understand was silently ignored. | **Fixed**: emits `cannot lower this assignment target` — corpus/suites confirmed nothing legitimate relied on the silence. |

Verified correct: nested if-expr clamps, generic width parameters, concat as
an rvalue.

## Round 5 — the native C emitter (this round)

Method: build every corpus program and probe as a **native binary** and diff
pass/fail against the JIT — a three-way differential (interpreter is the
oracle; JIT and native must both agree).

| # | Finding | Verdict |
|---|---------|---------|
| 5.1 | p1/p2/p12 (width wrap, shift mask, shared-name fanout) **fail natively but pass on JIT/interp** — the C translation has its own testbench codegen and was missing both Round-1/2 runner fixes. | **Fixed**: C locals record declared widths and mask on write; signal writes fan out through the same alias multimap; a shared `after`-clock arms every connected port's slot. |
| 5.2 | `sx_set` (the LLVM-emitted store used by the JIT runner, the native harness, and any future FFI) stored **unmasked** values — the interpreter's `set` masks. An engine asymmetry the differential harness never caught, because its stimuli always fit the signal width. | **Fixed**: `sx_set` masks to each signal's width in the switch case, matching the interpreter exactly. |
| 5.3 | `siox test --no-run` failures said only "unsupported testbench expression" — no hint *which* expression. | **Fixed**: the error now includes the pretty-printed expression. |
| 5.4 | Native testbench gaps (now loudly reported): struct literals (`{ .re = 10 }`), string literals in expressions, enum-variant references, module consts (`LOW`), runtime `read`/`read_to_string`. | **Open, by design for now** — the native path declares what it can't translate; `sioxc test` (JIT) covers those programs. |
| 5.5 | `test --no-run` on a file with no `#[test]` errors, while the JIT path reports "0 tests, ok". | Accepted asymmetry: asking for a test binary with no tests is an error. |

## Round 6 — testbench control flow (this round)

Probes: `match` and `else if` in `#[test]` bodies, all three engines.

| # | Finding | Verdict |
|---|---------|---------|
| 6.1 | A `match` in a testbench body was **silently skipped** by the runner (no `Stmt::Match` arm — locals kept their old values, no diagnostic). | **Fixed**: the runner executes the first hitting arm (enum variant / bit pattern via `siox_ir::bit_pattern_mask` / wildcard). |
| 6.2 | `else if` chains in a testbench were **silently skipped** too (the runner's If handler had "else-if: skip for now"). | **Fixed**: recursive `exec_if`. |
| 6.3 | The native emitter had both holes as well — it *built* the binary and the match body never ran. | **Fixed**: `match` translates to a C if/else-if chain over the scrutinee; else-if recurses (`c_if`). |

## Round 7 — string escapes (this round)

| # | Finding | Verdict |
|---|---------|---------|
| 7.1 | String escapes (`\n`, `\t`, `\"`, `\\`) were kept raw (round-2 finding 2.4): `print!("tab\there")` printed the backslash. | **Fixed**: the parser unescapes the literal body once (unknown escapes keep the backslash, best-effort). |
| 7.2 | With real control characters in strings, the native emitter's C-literal embedding was incomplete — a `\` + `e` sequence corrupted the printf format. | **Fixed**: one shared `c_escape` (backslash first, then quote/newline/tab/CR) for print formats, assert/warn messages, and enum symbols. |

## Round 8 — full-surface survey (findings only; fixes follow)

Per instruction: finish the hunt first, then fix. Areas swept: VCD waveforms,
the scheduler, >64-bit signals, nested composite ports, std library, parser
recovery, the CLI surface, and derived-type conversions.

| # | Finding | Severity |
|---|---------|----------|
| 8.1 | **Derivation conversions are broken everywhere.** `Clock(b)` / `ULogic(b)` / `Logic(u)` — which `std/logic.siox` promises the compiler synthesizes from the derivation chain — return **0** in testbench evaluation, and in hardware lower to `Unknown`, making the JIT refuse the whole design. | **High** — **FIXED**: the IR's source-type inference sees through enum-conversion calls (so nested `Logic(ULogic(b))` reaches the existing `derived_conversion`); the runner and native emitter pass enum conversions through as representation-identity. All three engines agree. |
| 8.2 | **JIT-unavailable + no fallback prints no test summary.** A >64-bit design (or any unlowerable one) on the default build prints one stderr note and exits 1 — no `test result: FAILED`, nothing that looks like a test ran. | **Medium** — **FIXED**: prints a proper `test result: FAILED. no engine can run this design (…)` with the `--features interp` hint; exit stays 1. |
| 8.3 | **4-value `Logic` dumps raw codes in VCD.** A 2-bit `$var wire 2` shows `b10`/`b11` where VCD has native `z`/`x` scalar states — waveform viewers show 2/3 instead of Z/X. | **Medium** — **FIXED**: a logic-scalar enum (every variant a quoted logic char) declares `$var wire 1` and dumps `0/1/z/x` states (L/H fold to 0/1, U/W/- to x). Uses the `enum_syms` map, so it also covers user-defined logic enums. |
| 8.4 | `std/rand.siox` declares nothing — `using std::rand::{randint}` errors while bare `randint(..)` calls work (runtime-provided, undeclared). | Low — surveyed further: `std::fs` has the same comment-only convention, so this is the *design* (runtime fns need no import). **FIXED as a diagnostic**: the unresolved-import error for `std::rand`/`std::fs` now says to call the function directly. |
| 8.5 | `sioxc test <dir>` fails with the raw OS error "Is a directory". The directory runner is a known todo; the message should say so. | Low — **FIXED**: a directory input now explains the limitation and shows the per-file form. |
| 8.6 | VCD cosmetics: duplicate `#0` timestamp blocks; no `$dumpvars` section. Viewers tolerate both. | Info — left as-is. |

Verified correct in the sweep: VCD structure/timing (per-change dumps, correct
timestamps), scheduler (zero-duration await, condition await, falling edges,
two independent clocks incl. a coincident edge), 128-bit arithmetic via the
interpreter fallback (carry across the 64-bit boundary), nested struct ports
across instances, `std::math` (sqrt/abs/min/max/pow), rand determinism
(identical sequences on all three engines), parser error recovery (multiple
errors + keep-going), 80-deep expression nesting, all CLI debug commands
(`ast`/`ir`/`tree`/`tokens`/`emit-llvm`), and bare AOT compilation of a
`#[top]` design.

## Round 9 — testbench operator dispatch (fix for #20 / 1.3)

The one big design gap left from Round 1: testbench expressions evaluated
every binary operator with raw unsigned semantics, so `-7 / 2` and `-7 < 2`
on `int[8]` locals gave unsigned results while the same expressions in
hardware inlined int's signed Div/Ord impls.

| # | What landed | Where |
|---|-------------|-------|
| 9.1 | Binary operators on a family-typed name (declared `int[8]`/`uint[8]`, connected or local) dispatch to the family's std operator impl: the fn body evaluates with `self`/`rhs` (+ `::width`) bound; comparisons derive from `Ord::cmp` via the same Less/Equal/Greater table the IR uses; results mask to the operand width. Inside an impl body operands are plain bound names, so nested operators stay raw — no recursive dispatch, mirroring the IR's inlining rule. | runner (`dispatch_binop`) |
| 9.2 | The native C emitter does the same: the impl body inlines as a C expression through the existing `fn_env` substitution stack (`c_dispatch_binop`); `resize(x, self::width)` inside a body resolves the bound width. | sioxc build.rs |
| 9.3 | `==`/`!=` on a family-typed name compare **at the type's width** (both sides masked), so `q == 0 - 3` matches the 253 bit pattern like hardware does. | both |
| 9.4 | `x::width` now evaluates in testbench expressions (bound width inside impl bodies; declared/connected width elsewhere). | both |

Verified: `-7/2 = -3`, signed `<`/`>`, arithmetic `>>`, and width-masked `==`
identical on interpreter, JIT, and native; suites, corpus, and the three-way
sweep all green. Regression test: `signed_local_test.siox` in the corpus.

## Still open (task list)
- **#11** — testbench loopback (DUT out→in via one local) doesn't propagate.
- **Round 5 item 5.4** — native-emitter expression coverage (struct/string
  literals, enum refs, module consts) — loud errors, not silent.
- **Round 2 item 2.6** — compound literal masks (`0xFF and 0x0F` as an
  operand) rejected by types; policy question.

## Method notes

What has worked, in order of yield:

1. **Hardware-vs-testbench comparison** — run the same computation through an
   entity and through testbench locals; they must agree. Found the entire
   masking/fanout family (the runner was the buggiest layer).
2. **Three-way engine diff** — interpreter (oracle) vs JIT vs native binary on
   identical programs. Found `sx_set`'s missing mask and the native emitter's
   missing runner fixes.
3. **Spec-conformance probes** — write the program the spec promises
   (bit patterns, `::width`) and see if it works. Found unimplemented-but-
   parseable and parseable-but-silent features.
4. **Silent-drop audits** — grep the lowering for catch-all `_ => {}` arms and
   probe each shape. Silent wrong answers (concat targets, testbench match)
   are the worst failure class; every fall-through should either work or
   error loudly.

Probe corpus: `$CLAUDE_JOB_DIR/tmp/bughunt/p*.siox` (session-local; promoted
to `Siox-lang/siox-tests` when they become regression tests). Differential
tests live in `crates/siox-llvm/tests/differential.rs`.
