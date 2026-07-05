# Timing, `await`, and cocotb (design note — proposal)

Status: **`await` + background clocks implemented** (#35 done; interp, JIT, and
native `--no-run` binary). What remains under #34 is a general event wheel with
real wall-clock time in the *compiled* runtime and the VPI/GPI-shaped ABI for
cocotb (#36, way later). Current scheduler is clock-driven stepping, not a full
time wheel. Tracks #34/#35/#36.

## The gap

The runtime has no real notion of **time**. The interpreter can `advance(t)`,
but the compiled path's `settle` is a pure delta-cycle: a native testbench's
`wait 10ns` just settles, and `tick(clk)` is the only way to make an edge.
There is no simulation clock, no future-scheduled wakeups, no value-change
callbacks. That blocks real timing *and* any external verification frontend.

## `await` — one timing primitive

Replace the ad-hoc `wait Nns` / `tick(clk)` with a single primitive that
mirrors cocotb's async triggers. Three forms:

```siox
await 10ns;          // advance simulation time      (cocotb Timer)
await clk::rising;   // wait for an edge             (cocotb RisingEdge)
await clk == '1';    // wait until a condition holds (cocotb value/edge)
```

Note the condition form uses `==` (equality), not `=` (assignment).
`tick(clk)` becomes sugar — `clk = '1'; await clk::rising; clk = '0';` — or is
kept as shorthand.

Each `await` **yields the testbench to the scheduler** until its trigger
fires. So the testbench is a coroutine, not straight-line code. This
generalizes past testbenches to imperative processes:

```siox
loop { await clk::rising; count = count + 1; }
```

which is how SystemVerilog/VHDL processes and cocotb coroutines already read.

## What it needs: a timed event scheduler (#34)

- A simulation clock (fs internally, `ns`/`ps`/… surface units).
- An event wheel: schedule a wakeup at `now + Δ` (`await 10ns`), and
  value-change callbacks on signals (`await clk::rising`, `await cond`).
- Exposed in the **compiled runtime C ABI**, not just the interpreter, since
  the native binary needs it too.
- **Coroutine execution of the testbench.** Straight-line testbench→C stops
  working once `await` can appear inside loops/branches; the generated code
  becomes a state machine (or the testbench runs on its own stack) driven by
  the scheduler. This is the main lift for the compiled backend.

Design the ABI as: hierarchy lookup (name → handle), get/set/force/release,
and register {timed, value-change, read-only, read-write, next-timestep}
callbacks. That surface is deliberately VPI/GPI-shaped (see below).

## Why this shape: cocotb (#36)

cocotb doesn't simulate — it *drives* a simulator over VPI/VHPI (or its GPI
abstraction), embedding Python. Inversion of control: **cocotb is the top,
the siox design is the DUT**. Verilator (compile-to-native, like sioxc) proves
a compiled simulator integrates with cocotb.

Because `await`'s trigger model *is* cocotb's trigger model, the scheduler +
callback ABI built for `await` is the same surface a VPI/GPI shim needs. Build
#34's ABI VPI-shaped now and cocotb attaches later without rework — either by
exposing a minimal VPI subset (cocotb's existing backend works) or a custom
cocotb GPI backend calling the siox runtime. Payoff: cocotb's whole ecosystem
(async testbenches, drivers/monitors/scoreboards, reuse of existing tests).
This belongs in its own layer (`siox-vpi`, near `pcb`), not the core compiler.

## Order

#34 scheduler + VPI-shaped ABI → #35 `await` lowers onto it → #36 cocotb shim
sits on the same ABL. Do not harden more of the backend around settle-only.
