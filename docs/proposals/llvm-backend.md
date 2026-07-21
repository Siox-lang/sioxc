# Plan: the LLVM backend (compiled processes + runtime)

Status: **decided (2026-07-05)** — siox is a low-level language, so its
execution is **native code via LLVM (inkwell)**, not the interpreter. One
emitter builds an LLVM module from the process-extracted IR; it forks only
at the tail into the two modes we want:

- **`siox test` → JIT**: inkwell's `ExecutionEngine` compiles the design
  in-process and runs it — no `clang`, no temp files, fast edit-run.
- **`siox build` → AOT**: the same module goes through `TargetMachine` to an
  object/binary (or `.bc` handed to `clang`/`lld`) — a standalone simulator.

LLVM is a load-bearing project dependency, the way it is for rustc/Swift —
we cannot emit native code without it, so we stop pretending otherwise.
inkwell also hands us `.ll` text (`print_to_string`) and `.bc` for free, so
there is **no hand-written textual `.ll` emitter** (that only earned its
keep by building without LLVM, which the mission moots).

The **interpreter (`siox-sim`) stays as the correctness oracle** for
differential testing during backend bring-up (B4) — not as the way you run
siox. It keeps `siox-sim` and the default `cargo build` LLVM-free so CI and
frontend contributors work on a bare box; inkwell lives behind a cargo
feature until JIT is trusted, then the feature goes default-on.

Type-level optimization (narrowing `uint[]`/`int[]` to exact-width machine
types) is explicitly *later*: those types will be softcoded by std, and the
backend must not bake in assumptions about them now — the first cut matches
the interpreter's word semantics so B4 is bit-identical.

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

The current emitter is **64-bit-word only**: it stores every signal in an
`i64` slot and masks to width, so signals wider than 64 bits (which the
interpreter handles via `u128` slots) are *rejected* at codegen rather than
silently truncated. Wide-word codegen arrives with the type-narrowing work.

The process→LLVM mapping is still direct: word loads/stores, integer ops on
words + mask, `select`, `fadd`-family on the f64 payload — the compiled
process computes exactly what the interpreter computes, instruction for
instruction.

## Emission (inkwell)

`inkwell = { version = "0.9.0", features = ["llvm22-1"], optional = true }`
(match the feature to the local `llvm-config`; this box has LLVM 22.1.6)
behind a `llvm` cargo feature. The emitter walks the process-extracted
`Design` and builds one LLVM function per process plus a `settle` that runs
the delta cycle (events → comb fixpoint → commit → roll). State is three
globals (current/old/event) at word width, masked to each signal's width —
matching the interpreter for bit-identical differential testing. `Expr` maps
1:1 onto `Builder` calls (`build_int_add`, `build_select`, `lshr`+`trunc`
for `Slice`, `build_float_*` for the `real` path).

The two modes share this module and split at the tail:

1. **JIT (`siox test --backend=llvm`)**: `create_jit_execution_engine`, then
   call `settle`/`set`/`read` in-process. The Rust test runner keeps
   interpreting the (cold) testbench and drives the JIT'd DUT.
2. **AOT (`siox build design.siox -o sim`)**: `TargetMachine::write_to_file`
   for an object, linked with a small runtime `main` compiled from the
   stimulus — a standalone native simulator.

`--emit-llvm` prints `module.print_to_string()`; golden-file tests diff that
text (they need inkwell built, but not a full LLVM at *emitter* test time
beyond linking). The C ABI the runtime/host uses stays tiny:

```c
void     sx_reset(void);
void     sx_set(uint32_t sig, uint64_t v);
uint64_t sx_read(uint32_t sig);
void     sx_settle(void);          // whole delta cycle, compiled
uint64_t sx_time(void);
```

## Staging

- **B0 — IR hardening** (shared with the interpreter): **validator DONE** —
  `Design::validate()` flags out-of-range signal ids, `Unknown` (unlowered)
  expressions, unknown widths on *referenced* signals, and malformed slices;
  the emitter runs it as a pre-codegen gate (clear error, not bad LLVM).
  Div-by-zero = 0 is settled (both engines + the codegen guard). **Still
  open (needs a decision):** `int[N]` signedness — both engines currently
  treat everything as unsigned words, so they agree; formalizing signed
  compare/shift/divide is a semantics call for the user.
- **B1 — process extraction** in `siox-ir`: name each driver/event block as
  a process, compute its sensitivity set (read signals) and write set.
  The interpreter can adopt sensitivity-based dispatch immediately —
  correctness-neutral, observable speedup, and it validates the process
  model before any bitcode exists.
- **B2 — `siox-llvm` crate (inkwell)** behind the `llvm` feature: **DONE
  (combinational)** — state globals (`cur`/`old`/`event`), `sx_reset`/
  `sx_set`/`sx_read` accessors, and `sx_settle` evaluating combinational
  processes in topological (dependency) order. Full `Expr`->builder mapping
  (int/logic/float, slices, selects, div-by-zero guard). Verified against
  LLVM 22 with `module.verify()` + golden tests; default workspace build
  stays LLVM-free. **B2.1 sequential codegen DONE**: `sx_settle` now emits
  the full delta cycle — event flags from stimulus, comb pass, event blocks
  with compute-all-then-commit next-state (spec 3.13), re-settle, roll
  `old<-cur`. Counter and register match the interpreter across clock edges
  in the differential harness. (Combinational-cycle fixpoint iteration is
  still single-pass; acyclic designs are exact.)
- **B3 — JIT (`siox test --backend=llvm`)**: `create_jit_execution_engine`;
  the Rust test runner drives the JIT'd DUT via `sx_set/settle/read`. Same
  runner, assertions, and VCD tap — only the DUT engine swaps.
- **B4 — differential harness**: every `#[test]` entity and example runs
  under the interpreter (oracle) and the JIT; results and VCD streams must
  match bit-for-bit. This is what lets the interpreter later step back.
- **B5 — AOT `siox build`**: **DONE**. `emit_object` writes a native `.o`
  via `TargetMachine`; `siox build <file> -o <out>` translates the `#[test]`
  stimulus to a C `main` driving the `sx_*` ABI, links with clang, and
  produces a standalone ELF that runs the testbench and returns PASS/FAIL.
  Verified: counter, register, mux, fsm all build + run + PASS. First cut is
  integer/logic/bool; real/char/string testbenches error cleanly (follow-on).
- **Flip default-on (DONE 2026-07-05)**: `llvm` is a default feature of
  siox-cli/siox-llvm, so LLVM is a default build dependency and `siox test`
  JIT-runs by default. The interpreter is kept as the reference oracle and
  the automatic fallback (wide >64-bit designs, invalid IR, or a
  `--no-default-features` build without a toolchain) — reachable explicitly
  with `--backend=interp`.
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
- `sioxc test counter_test.siox --no-run -o sim && ./sim` prints the same
  PASS and exit code as `siox test` (B5).
- A 10⁸-cycle counter benchmark shows a clear compiled-over-interpreted
  win before any optimization stage; static scheduling and type narrowing
  each add on top and are measured separately.
