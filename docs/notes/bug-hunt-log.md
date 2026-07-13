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

## Still open (task list)

- **#20** — testbench eval doesn't dispatch operator impls (signed int ops on
  locals give unsigned results; hardware correct).
- **#21** — string escapes kept raw.
- **#22** — testbench-level `match` silently skipped (runner and native).
- **#11** — testbench loopback (DUT out→in via one local) doesn't propagate.
- **#23 items 5.4** — native-emitter expression coverage (struct/string
  literals, enum refs, module consts).

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
