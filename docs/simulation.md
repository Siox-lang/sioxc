# Simulation

How siox runs a design: the delta-cycle model, the execution engines, simulation
time, and waveform output. For the language semantics these implement see
[language.md](language.md); for the compiler pipeline that produces the IR a
simulation consumes, [architecture.md](architecture.md).

## The model: delta-cycle, event-driven

A design lowers (in `siox-ir`) to two kinds of process, kept strictly apart:

- **Combinational `Driver`s** — a continuous assignment (`count = value;`), a
  wire that always equals its expression.
- **Sequential `EventBlock`s** — `if clk.rising() { … }`, updated only on the
  trigger, with next-state semantics.

A **settle** evaluates one delta cycle over the signal state (`cur`, `old`, and
per-signal `event` flags):

1. Mark `event[i]` for every signal whose stimulus changed it (`cur != old`).
2. Evaluate combinational drivers to a **fixpoint** (re-run until nothing
   changes; a non-converging loop is caught and warned, not hung).
3. Fire event blocks — each computes its next state from the **pre-commit**
   values (so `x::old` and same-cycle reads see the value before the edge).
4. Commit those next-state writes, then re-settle combinational logic.
5. Roll `old <- cur` and clear the event flags.

This is exact simulation: the value, the delta-cycle order, and every observed
event are the semantic contract. Nothing about *how* a value is stored may
change what is observed.

## Engines

The simulation model is emitted once by `siox-ir`; the **LLVM backend**
(`siox-llvm`) turns it into machine code, two ways:

- **JIT (default).** `sioxc test` and `sioxc sim --wave` compile the design
  in-process and run it. This is the engine.
- **Native AOT.** `sioxc <file>` compiles the `#[top]` design to a native
  object; `sioxc test --no-run -o <bin>` links a standalone native test binary
  (its own C testbench harness, no runtime dependency).

Both share the delta-cycle emitter, so JIT and native agree bit-for-bit. The
engine works in 64-bit words: a signal wider than 64 bits is rejected rather
than truncated. Building `--no-default-features` yields the frontend only (no
LLVM toolchain, no engine) — useful for working on the parser/checker, but
`sioxc test` then has nothing to run.

## Simulation time and the event wheel

The runtime has real **simulation time** (`time_fs`, femtoseconds internally;
`ns`/`ps`/… on the surface), not just delta cycles. An **event wheel** holds the
earliest pending event and advances to it:

- **Background clocks.** `clk = not clk after 5ns;` registers a free-running
  clock with a `5ns` half-period — the canonical clock generator. Multiple
  clocks interleave on the one wheel with real timestamps.
- **One-shot delays.** Any other `x = v after d;` schedules a write at
  `now + d`.
- **`await`** is the single timing primitive in a testbench, in three forms:

  ```siox
  await 10ns;          // advance simulation time
  await clk.rising();  // wait for an edge   (also .falling(), ::event)
  await count == 7;    // wait until a condition holds
  ```

  Each yields to the scheduler until its trigger fires, and may appear inside
  `for`/`if`. (`wait`/`tick` were removed — both now error and point at
  `await`.) The wheel lives in the runner and, identically, in the emitted C of
  the native binary. Design-note for the forward-looking scheduler/cocotb ABI:
  [proposals/timing-and-await.md](proposals/timing-and-await.md).

## Waveforms

`sioxc` records traces as [VCD](https://en.wikipedia.org/wiki/Value_change_dump)
(Value Change Dump) — the format every digital waveform viewer reads. siox does
not ship a viewer; it writes a VCD you open elsewhere.

```bash
sioxc sim counter.siox --wave counter.vcd
```

This elaborates the design, runs the first `#[test]` entity, and writes every
signal's value changes with real timestamps.

**How siox values appear:**

- **Buses** (`uint[8]`, `int[16]`) are binary vectors.
- **Four-value logic** (`Logic`, `Bit`) dumps as native VCD scalar states, so
  high-impedance shows as `z` and unknown as `x`, not a number.
- **Named enums** — an FSM `State`, `Bool` — dump as VCD `string` variables, so
  the viewer shows `Idle`/`Run`/`Done`/`true`/`false` instead of a raw
  discriminant (the de-facto VCD string extension Surfer and GTKWave both read).
- **Struct and array signals** flatten to one trace per leaf (`p.valid`,
  `regs[2]`).

**Viewing:** any VCD viewer — [Surfer](https://surfer-project.org/) (modern,
Rust, native or in-browser) or [GTKWave](https://gtkwave.sourceforge.net/) (the
long-standing workhorse).

**Notes:** the timescale is `1fs`, so a `10ns` period shows as `#10000000`
between edges; only signals that actually change are re-emitted, keeping traces
compact; FST (compressed) output for very large designs is a future addition.
