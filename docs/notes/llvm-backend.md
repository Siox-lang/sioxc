# Plan: the LLVM backend (compiled simulation)

Status: **plan, approved direction** — siox is a low-level language and its
execution should be native code, not tree-walking. This document plans the
lowering from the digital IR to LLVM IR and the staging to get there. The
interpreter (`siox-sim`) stays as the reference semantics and the
differential-testing oracle.

## Why compiled, and why LLVM

Today `siox-sim` interprets `Expr` trees per delta cycle. Compiled
simulators (Verilator is the proof) turn the *design itself* into native
code and win 10–1000×. LLVM is the right target because:

- **Arbitrary-width integers are native**: a `uint[37]` signal is an `i37`,
  a `uint[128]` an `i128`. The interpreter's slot-width problem (u64/u128
  slots, masking) disappears — every signal gets its exact type and LLVM
  legalizes it for the target CPU.
- `real` is a `double`; `Select` is `select`; slices are `lshr`+`trunc`;
  the whole IR `Expr` grammar maps 1:1 onto LLVM instructions.
- The same path later gives `siox build` a standalone native simulator
  binary — the language compiles like C, which is the low-level story.

## What already lines up

- The digital IR is **flat and language-neutral** (per-leaf scalar signals
  with widths; structs/arrays flattened; enums are integers with repr
  widths) — deliberately kept that way for backend convergence.
- **Operator/suffix impls inline at lowering**, so codegen never sees a
  call — the IR is pure dataflow by the time a backend reads it.
- The interpreter's `Slot` abstraction marks the exact IR↔execution
  boundary the compiled runtime slots into: the runner drives *either*
  backend through `set / settle / read`.

## The mapping

State is three flat arrays (current / old / event), one element per signal,
each at its natural LLVM type:

```llvm
; entity Counter { in clk: Clock; in rst: Logic; out count: uint[8]; }
@cur   = internal global %state zeroinitializer
@old   = internal global %state zeroinitializer
@event = internal global %events zeroinitializer
%state  = type { i2, i2, i8, i8 }   ; clk, rst, count, value
%events = type { i1, i1, i1, i1 }
```

| siox IR                          | LLVM |
| -------------------------------- | ---- |
| `Const(v)` / `Real(x)`           | `iN` constant / `double` constant |
| `Current(s)` / `Old(s)`          | load from `@cur` / `@old` field |
| `Event(s)`                       | load from `@event` field |
| `Binary(Add/Sub/Mul/Div)`        | `add`/`sub`/`mul`/`udiv` (wrap = masked by iN) |
| `Binary(FAdd..FDiv)`             | `fadd`..`fdiv` on `double` |
| comparisons                      | `icmp`/`fcmp` |
| `and/or/...` (logical)           | `icmp ne 0` + `and`/`or` on `i1` |
| `Slice{hi,lo}`                   | `lshr` + `trunc iN -> iM` |
| `Select`                         | `select i1, iN, iN` |
| width truncation                 | free — the type *is* the width |
| `Driver(target, cond, expr)`     | conditional store in `settle_comb()` |
| `EventBlock(cond, updates)`      | compare old/cur, compute nexts, commit in `eval_events()` |

Signed `int[N]` uses `sdiv`/`ashr`/`icmp slt` — pinning signedness per
operation is IR-hardening work the interpreter also needs (see Stage B0).

## Static scheduling (the real speed)

The interpreter re-evaluates *all* drivers to a fixpoint. The compiled
settle must not. Build the driver dependency DAG (driver reads signal →
edge from its driver); topologically sort:

- **Acyclic** (almost all real designs): `settle_comb()` is one straight
  pass in dependency order — no loop at all. LLVM then const-folds,
  vectorizes, and inlines across the whole design.
- **Cycles** (combinational loops/latches): fall back to bounded iteration
  over just the cyclic region, and emit the existing `W-P002` lint.

The scheduler is backend-independent — land it first and the *interpreter*
gets faster too, and its output order becomes the codegen contract.

## Emission strategy

Emit **textual LLVM IR** (`.ll`), not FFI bindings:

- zero Rust dependencies (matches the minimal-deps policy; `inkwell`/
  `llvm-sys` version-lock the build to a system LLVM),
- inspectable and diffable — golden-file tests of the emitted IR,
- compiled by the system toolchain: `clang -O2 -shared out.ll` (or
  `llc`+`ld`). `siox build` shells out; a missing clang is a clear error.

Execution modes, in delivery order:

1. **`siox build design.siox -o sim`** — standalone native simulator: the
   generated module plus a small C-ABI runtime `main` that runs the test
   stimulus compiled from the testbench statements. Verilator-style.
2. **Hosted**: the Rust runner `dlopen`s the shared object (one small,
   well-justified dep: `libloading`) and drives it through the same
   `set/settle/read` C ABI — the existing test runner and VCD tracer work
   unchanged, stimulus stays interpreted (it is cold code).
3. **JIT** (later, only if the edit-run loop hurts): ORC via textual IR
   still, or reconsider Cranelift as a pure-Rust JIT for `siox test`.

The C ABI is deliberately tiny and stable:

```c
void     sx_reset(void);
void     sx_set(uint32_t sig, const uint64_t *val_words);
void     sx_read(uint32_t sig, uint64_t *out_words);
void     sx_settle(void);
uint64_t sx_time(void);
```

## Staging

- **B0 — IR hardening** (shared with the interpreter):
  signedness pinned per op (`int[N]` compare/shift/divide are wrong-or-
  unimplemented today), div-by-zero semantics (defined: 0), X/Z semantics
  documented as the 2-bit enum encoding, an IR validator pass
  (widths known, signal ids in range, no `Unknown` reaching codegen).
- **B1 — static scheduler** in `siox-ir`: dependency DAG, topo order,
  cycle regions, `W-P002` wiring. Interpreter adopts the order.
- **B2 — `siox-llvm` crate**: Design → `.ll` text. Golden-file tests
  (counter/mux/FSM). No execution yet; `siox build --emit-llvm` prints it.
- **B3 — runtime + `siox build`**: C-ABI shell, clang invocation,
  standalone binary running the compiled stimulus; `--emit-llvm` keeps the
  `.ll` next to it.
- **B4 — differential harness**: every `#[test]` entity and example runs
  under both backends; results and VCD streams must match bit-for-bit.
  The examples corpus is the oracle.
- **B5 — hosted mode**: `libloading` behind a cargo feature; `siox test
  --backend=llvm` for the big regression suites.

## Non-goals (now)

- **Synthesis**: LLVM IR is software IR; netlists are a different backend
  (Phase 3 concern, different lowering from the same digital IR).
- **f128/f256**: `double` matches the interpreter; softfloat only when a
  Phase-2 need appears.
- **Analogue**: Phase 2; the solver would be host code calling the same
  compiled digital core.

## Acceptance

- `siox build examples/counter_test.siox -o sim && ./sim` prints the same
  PASS and exit code as `siox test`.
- All examples pass under B4 differential testing.
- A 10⁸-cycle counter benchmark shows ≥50× over the interpreter with the
  static scheduler on.
