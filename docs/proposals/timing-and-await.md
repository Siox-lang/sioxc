# Timing, `await`, and cocotb (design note)

Status: **#34 (timed scheduler) and #35 (`await`) are implemented** — in the
runner *and* the native `--no-run` binary. `wait`/`tick` are removed; `await` is
the single timing primitive. What remains is **#36: a VPI/GPI-shaped ABI so
cocotb can drive a compiled siox design** — its own layer (`siox-vpi`), not core
compiler work. The back half of this note is the design for that ABI.

## What is implemented

The runtime has real **simulation time** and an **event wheel**, on both
engines the language ships:

- **Simulation clock.** `time_fs` (femtoseconds internally; `ns`/`ps`/… on the
  surface) advances as events fire — not a pure delta cycle.
- **Event wheel.** The earliest pending event (a background clock's next edge or
  a one-shot delayed write) is found and fired, then the design settles:
  - runner: `Runner::next_event` / `step_one_clock`, with `clocks: Vec<ClockGen>`
    (a free-running clock's half-period) and `oneshots: Vec<(fs, signal, value)>`
    (`src/run.rs`);
  - native binary: the generated C emits the same wheel — `sx_next_edge` /
    `sx_step_clock` over sim-time + per-clock next-edge arrays
    (`src/bin/sioxc/build.rs`).
- **Background clocks.** `clk = not clk after 5ns;` registers a free-running
  clock (half-period `5ns`); other `x = v after d;` schedules a one-shot write at
  `now + d`.

### `await` — the one timing primitive

`wait Nns` and `tick(clk)` are gone (both now hard-error, pointing here). Three
forms, each yielding the testbench to the scheduler until its trigger fires:

```siox
await 10ns;          // advance simulation time      (cocotb Timer)
await clk.rising();  // wait for an edge             (cocotb RisingEdge)
await count == 7;    // wait until a condition holds (cocotb value/edge)
```

(The condition form uses `==`, not `=`.) All three run identically on the
JIT (driven by the runner: `do_await` → `run_clocks_until` / `await_edge` /
`await_cond`) and in the native binary. `await` may appear inside `for`/`if`:

```siox
for i in 0..9 { await clk.rising(); }   // ten edges
```

The "coroutine execution" worry from the original proposal is handled by
emitting the testbench as one C function with native control flow — `await`
blocks on a step-clock loop, so the testbench runs on the C stack and loops /
branches around `await` just work. No generated state machine was needed.

## What remains: cocotb over a VPI/GPI-shaped ABI (#36)

cocotb doesn't simulate — it *drives* a simulator over VPI/VHPI (or its GPI
abstraction), embedding Python. Control inverts: **cocotb is the top, the siox
design is the DUT** (Verilator — compile-to-native like sioxc — proves a
compiled simulator can attach). Because `await`'s trigger model *is* cocotb's
trigger model, the scheduler already built for `await` is the surface a GPI
backend needs; it just isn't exposed as a foreign, callback-driven ABI yet.

Today's runtime ABI is flat and runner-driven:

```c
void sx_reset(void);
void sx_set(uint32_t sig, uint64_t val);
uint64_t sx_read(uint32_t sig);
void sx_settle(void);
```

That is enough for the runner to drive the design, but not for an *external*
top to drive it: there is no name→handle lookup and no way to register a
callback and hand control back. The design below adds exactly that, as a thin
layer over the existing scheduler.

### The `siox-vpi` ABI (proposed)

A minimal, GPI-shaped C ABI. cocotb's GPI wants three things — handles, value
access, and phase-aware callbacks — so the surface mirrors them directly.

**1. Handles (hierarchy lookup).** Map a hierarchical name to an opaque handle
over the flattened signal table the design already has (`Design::signals`, whose
`path` is the hierarchical name, e.g. `Counter.dut.count`).

```c
sx_handle sx_get_handle(const char *full_name);   // NULL if not found
uint32_t  sx_handle_width(sx_handle h);
const char *sx_handle_name(sx_handle h);
// iteration for discovery (cocotb walks the hierarchy):
sx_handle sx_iterate(sx_handle scope);   // children; NULL scope = top
sx_handle sx_next(sx_handle iter);
```

A handle is just a `SignalId` (plus a tag) — no new state, since the flattened
signal table already *is* the hierarchy.

**2. Value get/set/force/release.** `get`/`set` already exist as `sx_read`/
`sx_set`; add `force`/`release` (a forced signal ignores its drivers until
released — cocotb uses this constantly):

```c
uint64_t sx_get(sx_handle h);
void     sx_put(sx_handle h, uint64_t val);   // deposit (drivers may override)
void     sx_force(sx_handle h, uint64_t val); // hold until released
void     sx_release(sx_handle h);
```

`force`/`release` need one new per-signal bit in the engine's settle (a forced
signal is not overwritten by its combinational driver) — the only *engine*
change; everything else is a runtime-layer addition.

**3. Phase-aware callbacks.** This is the real addition: register a callback and
return control to the caller; the runtime invokes it when the trigger fires.
The five GPI callback kinds map onto the existing wheel:

```c
typedef void (*sx_cb)(void *user);
sx_cbh sx_cb_timed(uint64_t delay_fs, sx_cb f, void *user);      // Timer
sx_cbh sx_cb_value_change(sx_handle h, sx_cb f, void *user);     // Edge/ValueChange
sx_cbh sx_cb_readwrite(sx_cb f, void *user);   // end of settle, before commit
sx_cbh sx_cb_readonly(sx_cb f, void *user);    // values stable, no writes
sx_cbh sx_cb_nexttime(sx_cb f, void *user);    // start of next delta
void   sx_remove_cb(sx_cbh);
void   sx_run(void);   // drive the wheel until no events / callbacks remain
```

Mapping to what exists:

| GPI callback | Backed by |
| --- | --- |
| timed (`Timer`) | a one-shot entry on the wheel (`oneshots`), firing the cb instead of a write |
| value-change (`Edge`) | the `await_edge` watch generalised to a callback list keyed by signal |
| read-write | a hook at the end of `step_one_clock` before `settle`'s commit |
| read-only | a hook after settle reaches fixpoint (values stable) |
| next-timestep | a hook at the top of the next `step_one_clock` |

The runner's `next_event`/`step_one_clock` loop becomes `sx_run`'s core; instead
of the runner interpreting a testbench, the wheel fires *registered callbacks*.
The `await` lowering and a GPI backend are then two clients of the same wheel.

### Control inversion and where it lives

Under cocotb, cocotb's process is the top and calls `sx_run`; the siox runtime
is a shared library it loads. Two attachment options, both on this ABI:

1. **Minimal VPI shim** — expose a small VPI subset (`vpi_handle_by_name`,
   `vpi_get/put_value`, `vpi_register_cb`) that forwards to `sx_*`; cocotb's
   stock VPI backend then works unmodified.
2. **Custom cocotb GPI backend** — a small C++ GPI implementation calling `sx_*`
   directly (no VPI emulation). Cleaner, but tracks cocotb's GPI interface.

Either way this is a **separate layer** (`siox-vpi`, sibling to the future
`pcb`), depending on the core runtime but not part of the compiler. It also
wants a real cocotb dependency to test against, so it should not land in-tree
until there is a reason to pull that in.

### Incremental, testable path

Each step is self-contained and validated without cocotb:

1. **Handles + get/put** (`sx_get_handle`, `sx_iterate`/`sx_next`, `force`/
   `release`). Testable directly in Rust/C against a compiled design.
2. **The callback wheel** (`sx_cb_timed`, `sx_cb_value_change`, `sx_run`, the
   phase hooks) — refactor `step_one_clock` to fire callbacks; re-express the
   existing `await` forms as clients of it, so the current tests are the
   regression guard.
3. **A VPI/GPI shim** (`siox-vpi`) — only once cocotb is actually wired up.

## Order

#34 scheduler + real sim time → **done**. #35 `await` on that wheel → **done**.
#36 the callback ABI above, then a cocotb shim on it → the remaining work. Don't
harden more of the backend around settle-only or the flat `sx_set/read` ABI.
