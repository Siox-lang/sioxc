# Plan: the LLVM backend (compiled processes + runtime)

Status: **plan, approved direction** — siox is a low-level language and its
execution should be native code. The basis is **bitcode generation from
processes** and **a runtime that runs those processes**. Type-level
optimization (narrowing `uint[]`/`int[]` to exact-width machine types) is
explicitly *later*: those types will be softcoded by the std library in
time, and the backend must not bake in assumptions about them now. The
interpreter (`siox-sim`) stays as the reference semantics and the
differential-testing oracle.

## The model: processes + kernel

This is the classic simulator architecture (VHDL-style), which fits the IR
as it already exists:

- A **process** is a unit of behavior with a sensitivity set:
  - each combinational `Driver(target, cond, expr)` is a process sensitive
    to the signals its expression reads;
  - each `EventBlock(condition, updates)` is a clocked process sensitive to
    the signals in its condition.
- The **runtime kernel** owns everything else: the signal state arrays
  (current / old / event), the delta-cycle loop, sensitivity dispatch,
  next-state commit, simulation time, and the VCD tap. Processes are pure
  compiled functions the kernel calls when their sensitivity fires.

```
          +-----------------------------+
          |        runtime kernel       |   Rust (or small C): state,
          |  state | deltas | sched | t |   delta cycles, dispatch
          +---+----------+----------+---+
              v          v          v        compiled bitcode: one
          proc_0()   proc_1()   proc_2()     function per process
```

## What compiles, what doesn't

- **Compiled**: process bodies — the `Expr` trees of drivers and event
  blocks, already pure dataflow (operator/suffix impls inline at lowering,
  so codegen never sees a call).
- **Runtime**: scheduling, state, commit — the delta-cycle contract from
  the spec, exactly what `siox-sim::settle` does today.
- **Interpreted**: testbench stimulus (`wait`/`tick`/`assert`) — cold code,
  stays in the host runner forever.

## Value representation (deliberately unoptimized)

Processes operate on the **generic slot representation the interpreter
already uses** — word-based values (u64/u128 words), width masks applied on
store, `real` as f64 bits in the low word. Signals load/store through the
kernel's state arrays.

`uint[N]` / `int[N]` are *not* special-cased into exact-width `iN` machine
types in the first backend: they will be softcoded by std (derived Logic
vectors with operator impls), and codegen must treat them the way it treats
any inlined-impl type. When that std migration lands, a **type-narrowing
optimization stage** can map finished value types onto native `iN`/`double`
and let LLVM legalize — that is an optimization pass over working bitcode,
not the foundation.

The process→LLVM mapping is still direct: word loads/stores, integer ops on
words + mask, `select`, `fadd`-family on the f64 payload — the compiled
process computes exactly what the interpreter computes, instruction for
instruction.

## Emission

Emit **LLVM IR text (`.ll`) / bitcode (`.bc`)** with no FFI dependencies
(`llvm-sys`/`inkwell` version-lock the build; textual IR is inspectable and
golden-file-testable). The system toolchain compiles it:
`clang -O2 -shared` (hosted) or `clang` + runtime `main` (standalone).

Execution modes, in delivery order:

1. **Hosted**: the Rust kernel `dlopen`s the compiled processes (one small
   dep, `libloading`, behind a cargo feature) and dispatches them through a
   tiny C ABI. The existing test runner, assertions, and VCD tracing work
   unchanged — only process evaluation changes engine.
2. **Standalone `siox build design.siox -o sim`**: same bitcode linked with
   a small runtime and a compiled form of the stimulus — a native simulator
   binary, no siox installation needed to run it.

The C ABI between kernel and processes:

```c
// kernel -> process: evaluate against the state arrays
void sx_proc_N(const uint64_t *cur, const uint64_t *old,
               const uint8_t *event, uint64_t *out);
// kernel side (hosted): stimulus drives it like the interpreter
void sx_set(uint32_t sig, uint64_t v);   uint64_t sx_read(uint32_t sig);
void sx_settle(void);                    uint64_t sx_time(void);
```

## Staging

- **B0 — IR hardening** (shared with the interpreter): defined semantics
  codegen can trust — div-by-zero = 0, X/Z as the 2-bit enum encoding,
  interim signedness rules for `int[N]` (pinned per op even though the type
  is slated for std softcoding), an IR validator (widths known, ids in
  range, no `Unknown` reaching a backend).
- **B1 — process extraction** in `siox-ir`: name each driver/event block as
  a process, compute its sensitivity set (read signals) and write set.
  The interpreter can adopt sensitivity-based dispatch immediately —
  correctness-neutral, observable speedup, and it validates the process
  model before any bitcode exists.
- **B2 — bitcode generation** (`siox-llvm` crate): one function per
  process over the word-based state ABI. Golden-file `.ll` tests
  (counter/mux/FSM). `siox build --emit-llvm` prints it; no execution yet.
- **B3 — runtime kernel + hosted mode**: kernel keeps living in Rust
  (it *is* today's `settle`, refactored against process tables);
  `libloading` behind a feature; `siox test --backend=llvm`.
- **B4 — differential harness**: every `#[test]` entity and example runs
  under both engines; results and VCD streams must match bit-for-bit.
- **B5 — standalone `siox build`**: compiled stimulus + runtime `main`,
  single native binary.
- **Later (optimization, in either order):**
  - **static scheduling** — topo-sort the process DAG so acyclic regions
    run as one straight pass instead of sensitivity-driven deltas
    (Verilator's trick; big constant-factor win);
  - **type narrowing** — once `uint[]`/`int[]` are std-softcoded, map
    finished value types to exact `iN`/`double` and drop the masks.

## Non-goals (now)

- **Type-level optimization** of `uint[]`/`int[]` — see above; explicitly
  deferred until std softcodes them.
- **Synthesis**: LLVM IR is software IR; netlists are a different backend
  over the same digital IR (Phase 3).
- **f128/f256**: f64 payload matches the interpreter; revisit with Phase 2.
- **Analogue**: Phase 2; the solver would be host code calling the same
  compiled digital core.

## Acceptance

- `siox test --backend=llvm` passes the entire example corpus with results
  and VCDs identical to the interpreter (B4).
- `siox build examples/counter_test.siox -o sim && ./sim` prints the same
  PASS and exit code as `siox test` (B5).
- A 10⁸-cycle counter benchmark shows a clear compiled-over-interpreted
  win before any optimization stage; static scheduling and type narrowing
  each add on top and are measured separately.
