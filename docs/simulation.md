# Simulation

How siox runs a design: the delta-cycle model, native execution, simulation
time, and waveform output. For the language semantics these implement see
[language.md](language.md); for the compiler pipeline that produces the IR a
simulation consumes, [architecture.md](architecture.md).

## The model: delta-cycle, event-driven

A design lowers (in `ir`) to two kinds of process, kept strictly apart:

- **Combinational `Driver`s** — a continuous assignment (`count = value;`), a
  wire that always equals its expression.
- **Sequential `EventBlock`s** — `if clk.rising() { … }`, updated only on the
  trigger, with next-state semantics.

A **settle** evaluates one delta cycle over the signal state (`cur`, `old`, and
per-signal `event` flags):

1. Evaluate combinational drivers to a **fixpoint** (re-run until nothing
   changes; a non-converging loop is caught and warned, not hung).
2. Snapshot the settled values and mark `event[i]` where `cur != old`.
3. Fire event blocks—each computes its next state from the **pre-commit**
   values (so `x'old` and same-cycle reads see the value before the edge).
4. Commit next-state writes, advance `old` to the snapshot, and start another
   delta if the commit or a derived connection changed anything.

This is exact simulation: the value, the delta-cycle order, and every observed
event are the semantic contract. Nothing about *how* a value is stored may
change what is observed.

## Native execution

The `ir` layer emits the simulation model and the **LLVM backend** (`siox::llvm`)
compiles it ahead of time to native machine code. `sioxc <file>` emits the
`#[top]` design as an object. `sioxc --test` generates a native testbench
harness and links it with that object. The compiler stops after producing the
executable; running and filtering it are separate operations.

Signal values cross the harness ABI in low-word-first 64-bit words, so values
such as `unsigned[128]` retain their per-type width. LLVM is the permanent
backend, so building siox needs an LLVM toolchain.

## Simulation time and the event wheel

The runtime has real **simulation time** (`time_fs`, femtoseconds internally;
`ns`/`ps`/… on the surface), not just delta cycles. An **event wheel** holds the
earliest pending event and advances to it:

- **Background clocks.** `clk = not clk after 5ns;` registers a free-running
  clock with a `5ns` half-period — the canonical clock generator. Multiple
  clocks interleave on the one wheel with real timestamps.
- **Delayed assignments.** The native Phase 1 harness currently accepts the
  canonical self-toggle above. Other `x = v after d;` forms receive a build
  error until one-shot writes are represented on the event wheel.
- **`await`** is the single timing primitive in a testbench, in three forms:

  ```siox
  await 10ns;          // advance simulation time
  await clk.rising();  // wait for an edge   (also .falling(), 'event)
  await count == 7;    // wait until a condition holds
  ```

  Each yields to the scheduler until its trigger fires, and may appear inside
  `for`/`if`. (`wait`/`tick` were removed — both now error and point at
  `await`.) The wheel lives in the runner and, identically, in the emitted C of
  the native binary. Design-note for the forward-looking scheduler/cocotb ABI:
  [proposals/timing-and-await.md](proposals/timing-and-await.md).

Native time is an unsigned 64-bit femtosecond count. A literal whose unit
conversion cannot fit that timeline is a build error. Runtime additions
saturate at `18446744073709551615fs` instead of wrapping to time zero; the
event-wheel sentinel and multi-test waveform timestamps use the same rule.

## Waveforms

Waveform output supports text
[VCD](https://en.wikipedia.org/wiki/Value_change_dump) (Value Change Dump) and
compressed FST. The generated native test executable owns the files: while
running its scheduler it writes hierarchy declarations and timestamped value
changes directly. `sioxc` does not receive samples back, and the compiler
library does not retain a trace or waveform writer.

```bash
sioxc --test counter_test.siox -o counter-tests
./counter-tests --vcd counter.vcd
./counter-tests --fst counter.fst
./counter-tests --vcd counter.vcd --fst counter.fst
```

`--vcd=<path>` and `--fst=<path>` are equivalent spellings. A test-name filter
may appear before or after either option. VCD and FST can be emitted together
to different paths and receive the exact same scheduler-side change points.
When several tests run, their traces are placed consecutively on one monotonic
timeline. siox does not ship a viewer; the resulting file is opened in an
external waveform application.

**How siox values appear:**

- **Buses** (`unsigned[8]`, `signed[16]`) are binary vectors.
- **Nine-value `Logic`** retains the IEEE discriminant internally. Waveforms map
  high-impedance states to `z`, unknown-like metavalues to `x`, and ordinary
  binary elements to `0`/`1`; `Bit` remains two-value.
- **Named enums** — an FSM `State`, `Bool` — dump as string variables, so
  the viewer shows `Idle`/`Run`/`Done`/`true`/`false` instead of a raw
  discriminant (the de-facto VCD string extension Surfer and GTKWave both read).
- **Struct and array signals** flatten to one trace per leaf (`p.valid`,
  `regs[2]`).

**Viewing:** [Surfer](https://surfer-project.org/) is a modern native/browser
viewer; [GTKWave](https://gtkwave.sourceforge.net/) is the long-standing
workhorse and provides the reference FST implementation.

**Notes:** the timescale is `1fs`, so a `10ns` period shows as `#10000000`
between edges; only signals that actually change are re-emitted, keeping traces
compact. FST is written through the pinned MIT-licensed libfst sources embedded
in `sioxc`; the resulting test executable needs ordinary zlib but does not need
GTKWave, `vcd2fst`, or a separately installed libfst.
