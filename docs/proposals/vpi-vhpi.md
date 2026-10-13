# Co-simulation with VHDL and Verilog, and the simulator interfaces

Status: **proposal**. Nothing here is implemented.

## Problem

A siox design can only run in its own executable, driven by its own `#[test]`
processes. Two things a hardware team needs are missing:

1. **Mixing languages.** Putting a siox block into a VHDL or Verilog design
   (or a VHDL/Verilog block into a siox design's verification) and simulating
   them together. Today the only foreign-HDL feature is the `extern entity`
   black box, which simulates nothing.
2. **Verification tools.** cocotb, Python models and existing C plugins drive
   a simulator through its standard C interface, which siox does not offer.

Both go through the same two IEEE interfaces, used in opposite directions:

- **VPI** (IEEE 1364 / IEEE 1800 §36–38), the Verilog/SystemVerilog interface;
- **VHPI** (IEEE 1076-2019 §17–23), the VHDL interface.

For (1) siox is a *client*: a siox block runs as a plugin inside a VHDL or
Verilog simulator and talks to the rest of the design through that
simulator's VPI or VHPI. For (2) siox is a *server*: its own executable
implements VPI and VHPI so tools can attach to it.

## Decision

One runtime refactor serves both, then the client direction first:

1. **A steppable runtime.** Split the test runner's loop into calls an outside
   scheduler can drive (start, apply inputs, settle, advance time, read
   outputs). The `--test` executable becomes one driver of it.
2. **Co-simulation (siox as client).** `sioxc` builds a siox top as a shared
   library plus a wrapper module/entity in Verilog and VHDL. The host
   simulator instantiates the wrapper like any other block; the library,
   loaded as the host's VPI or VHPI plugin, steps the siox runtime from the
   host's callbacks. VHDL first on **nvc**, Verilog second on **Icarus**.
3. **Simulator interfaces (siox as server).** The siox executable implements
   VPI, then VHPI, over the same runtime hooks; cocotb is the first client.

The interoperability boundary rules
([interoperability.md](https://github.com/Siox-lang/siox-paper/blob/main/docs/interoperability.md#foreign-hdls))
govern every value crossing: rule 1 for logic values (exact through VHPI,
`To_X01Z` into 4-state VPI), rule 11 for names.

No language change, no new Rust dependency, nothing generated per design
beyond tables and the wrapper text; `dlopen` and the plugin entry points are
libc and C.

## Hosts

What the host simulators offer decides the shape of the client side.

| Host | Interface | What it offers (checked) | Consequence |
| --- | --- | --- | --- |
| nvc 1.23 | VHPI | value-change, end-of-processes, next-time-step, after-delay and end-of-elaboration callbacks; `vhpi_put_value` deposit/force with propagate; port/signal iteration. **No** foreign architectures, **no** `vhpi_schedule_transaction` (source, `src/vhpi/vhpi-model.c`) | the siox block is an empty VHDL shell whose outputs the plugin deposits: no `inout` resolution, outputs one host delta after inputs |
| nvc 1.23 | VPI | system tasks only: no `vpi_register_cb`, `vpi_get_time` or `vpi_control`, `vpi_handle_by_name` missing (`src/vpi/vpi-model.c`) | not a Verilog host |
| Icarus Verilog | VPI | the standard callbacks and `vpi_put_value` cocotb relies on (to be confirmed in step 1) | the Verilog host |
| Questa, Riviera-PRO, Xcelium, VCS | VHPI/VPI | full standards, VHPI foreign architectures (Questa, likely Riviera) | later; with a foreign architecture siox outputs become real drivers |
| Verilator | VPI (limited) | cycle-based, own eval loop, VPI on `public` signals | separate integration, later |

nvc is the reference simulator already and runs VHDL with Verilog mixed in,
so an nvc design can contain VHDL, Verilog and, through this, siox.

## Part 1: the steppable runtime (shared)

Today `sx_runtime_run_test` owns time and the whole loop. Split it into a
small C API in `runtime/process.c`, with the test runner rewritten on top and
no change in behavior:

```c
int      sx_sim_start(uint32_t root);           /* reset, initialize, settle at t=0 */
int      sx_sim_settle(void);                   /* deltas at the current time to a fixed point */
int      sx_sim_advance(uint64_t time_fs);      /* apply events due up to time_fs, set now */
int      sx_sim_next_due(uint64_t *time_fs);    /* earliest pending event, if any */
void     sx_sim_finish(void);
uint32_t sx_sim_port_count(void);               /* the root's ports, from a new table */
const sx_port *sx_sim_port(uint32_t index);     /* name, direction, width, kind, signal */
void     sx_sim_read(uint32_t signal, uint64_t *words, uint64_t *meta);
void     sx_sim_write(uint32_t signal, const uint64_t *words, const uint64_t *meta);
```

`sx_sim_write` needs one new design-object entry, `sx_deposit(signal, words,
meta)`, which stores a value into a signal and marks it changed so the next
commit propagates it as any write; it bumps the runtime ABI. The root's ports
need one new table (name, direction, signal, width, kind); the waveform tables
already carry names, scopes, widths, kinds, enum symbols and metavalue
companions.

**One run per library.** Emitted design state is object-owned globals
([multiprocess.md](multiprocess.md), "Ownership and ABI migration"), so one
loaded library holds one siox instance. The first version rejects a second
instance of the same wrapper with a clear error; several instances follow when
per-run design frames exist.

## Part 2: co-simulation (siox inside a host)

**Build.** `sioxc --cosim --top Top -o top` (flag names open) writes:

- `top.so`: the design object, the runtime and both bridges, exporting
  `vlog_startup_routines` (VPI) and a `vhpi_startup_routines`/bootstrap
  function (VHPI);
- `top.vhd`: `entity Top is port (..); end; architecture siox of Top is begin
  end;`, an empty shell the plugin fills;
- `top.v`: `module Top(..); endmodule`, outputs declared `reg` so a deposit
  holds.

**Wrapper ports.** Flattened by rule 11 (`bus.valid` → `bus_valid`,
`down[2].ready` → `down_2_ready`, tuples by position: `pair_0`), with types:

| siox | VHDL | Verilog |
| --- | --- | --- |
| `Logic` / `Bit` | `std_logic` / `bit` | `wire` / `reg` 1 bit |
| `Logic[N]`, `unsigned[N]`, `signed[N]` | `std_logic_vector`, `unsigned`, `signed` `(N-1 downto 0)` | `[N-1:0]` (`signed` for `signed`) |
| enum | its VHDL enumeration (generated in a package) | `[W-1:0]` of its discriminant |
| `integer` | `integer` when its range fits, else `signed(63 downto 0)` | `[63:0]` signed |
| `real` | `real` | `real` |

**The bridge**, per host:

1. At start of simulation (nvc: end of elaboration) find the wrapper
   instance, resolve each port to a host handle, `sx_sim_start`, deposit the
   initial outputs, register value-change callbacks on the inputs and a timer
   at `sx_sim_next_due`.
2. An input change marks the block dirty and registers one read/write-sync
   callback (`cbReadWriteSynch`, `vhpiCbEndOfProcesses`) for that host delta,
   so several inputs changing together are evaluated once.
3. At that callback or at the timer: `sx_sim_advance(host now)`, write every
   input (converted by rule 1), `sx_sim_settle`, read the outputs and deposit
   the changed ones (`vpi_put_value(.., vpiNoDelay)`,
   `vhpi_put_value(.., vhpiDepositPropagate)`), re-arm the timer.

The siox block thus behaves like any process in the host: inputs settle, the
block evaluates all its own deltas, outputs appear in the next host delta.
Clocks generated inside siox, `after` delays and `await` in siox code all
become host timers. Host time units convert to femtoseconds; a siox event
between two host ticks is reported once and rounded up.

**Not in the first version:** `inout` ports (a deposit is not a driver, and
nvc rejects `inout` across its own mixed-language boundary too), several
instances of one wrapper, and VHDL foreign architectures (not in nvc).

**The other way round** — a VHDL or Verilog block inside a siox design's own
testbench — is the same co-simulation with the host on top: the siox
testbench is ported to a host-language top level that instantiates both, or
the siox side is exported to HDL (interoperability item 3). It needs no
mechanism beyond this one.

## Part 3: the simulator interfaces (siox as server)

The siox executable implements VPI and then VHPI for tools that drive a
design (cocotb first).

- **Object model** from the waveform tables: regions per instance and per
  struct/tuple/view value, a signal object per storage leaf, arrays as
  indexable objects. Names as written; VPI full names `top.u0.q`, VHPI
  `:top:u0:q`.
- **Values** through the same conversions as Part 2: 4-state VPI by rule 1,
  exact nine-value `Logic` through VHPI (a runtime table maps siox's
  discriminants to VHPI's order, generated from std's `ULogic` declaration).
- **Writes**: deposit through `sx_deposit`; force/release through a force
  table the commit consults; delayed puts as scheduled events.
- **Callbacks** at the loop boundaries Part 1 exposes:

  | Callback | VPI | VHPI | Where |
  | --- | --- | --- | --- |
  | start / end of simulation | `cbStartOfSimulation`, `cbEndOfSimulation` | `vhpiCbStartOfSimulation`, `vhpiCbEndOfSimulation` | before reset; after the run |
  | value change | `cbValueChange` | `vhpiCbValueChange` | after a commit, per changed signal |
  | read/write sync | `cbReadWriteSynch` | `vhpiCbEndOfProcesses` | after processes, before the commit |
  | read-only sync | `cbReadOnlySynch` | `vhpiCbLastKnownDeltaCycle` | at the settled point |
  | next time step | `cbNextSimTime` | `vhpiCbNextTimeStep` | when time advances |
  | timer | `cbAfterDelay` | `vhpiCbAfterDelay` | a new event kind |

- **Calls**: the subsets cocotb's `VpiImpl`/`VhpiImpl` use (handles by name and
  index, iteration, `get`/`get_str`, values, callbacks, time, control, error
  checks, `vpi_get_vlog_info` reporting "siox"); confirm against cocotb's
  sources first, grow on demand; unsupported calls set the standard error
  record.
- **Running**: `--vpi <lib.so>` and `--vhpi <lib.so>[:entry]` load plugins
  before reset; a design-only build (`sioxc --sim --top Top`, name open) runs
  a top with no `#[test]` until the plugin finishes it. cocotb then needs a
  small simulator recipe, upstreamed once this works.

## Headers

Plugins and hosts use the standard headers, so every constant (object types,
properties, value formats, callback reasons) must have the standard's value.
Use the IEEE-published `vpi_user.h`/`sv_vpi_user.h` and `vhpi_user.h` if their
licence allows redistribution in an MIT/Apache project; otherwise write our
own from the standards' tables. nvc's copies are GPL-3 and are not used.

## Delivery order

1. **Facts.** Header licence settled; Icarus's VPI checked for the callbacks
   and puts the bridge uses; cocotb's call lists recorded; two minimal C test
   plugins written (one VPI, one VHPI) for later steps.
2. **Steppable runtime.** `sx_sim_*` with the test runner on top. Gate: full
   CI with identical results and waveforms.
3. **Ports and deposit.** The root port table, `sx_deposit` (ABI bump),
   `sx_sim_read`/`sx_sim_write`; unit-tested from C.
4. **VHDL co-simulation on nvc.** VHDL wrapper, VHPI bridge, `sioxc --cosim`.
   Milestone: a siox counter inside a VHDL testbench on nvc, its results
   equal to the same siox design's own test.
5. **Verilog co-simulation on Icarus.** Verilog wrapper, VPI bridge.
   Milestone: the same design inside a Verilog testbench.
6. **VPI server and cocotb.** Plugin loading, design-only build, the VPI
   object model and callbacks. Milestone: a corpus design driven by a cocotb
   test.
7. **VHPI server.** The same core with exact `Logic`; cocotb's VHPI path.
8. **Later:** several instances (per-run frames), `inout`, foreign
   architectures on hosts that have them, Verilator, commercial hosts.

Steps 2–4 are the first usable result: siox and VHDL in one simulation.

## Acceptance gates

- Steps 2–3 change nothing for existing executables: full CI identical,
  waveforms byte for byte, no measurable cost when nothing is attached.
- Co-simulation: each corpus design used in a co-simulation test gives the
  same port values, cycle for cycle, as its own siox test; run in default and
  `bitpack` builds; CI installs nvc and Icarus.
- Server: the C test plugins check hierarchy, names, values (every `Logic`
  symbol through VHPI, X/Z through VPI), each callback reason, deposit, force
  and release; compared with nvc and Icarus on an equivalent HDL design where
  one exists.
- cocotb: at least one corpus design under a cocotb test in CI.

## Open questions

1. **Headers.** Redistribute the IEEE headers, or write our own from the
   standards' constant tables?
2. **Flag names.** `--cosim`, `--sim`, `--vpi`/`--vhpi` (or one `--load` as
   nvc).
3. **Several instances.** Wait for per-run design frames, or load one copy of
   the library per instance (`dlmopen`) in the meantime?
4. **Wrapper types.** Enums as generated VHDL enumerations or as plain
   vectors; `integer` as VHDL `integer` (32-bit in most tools) or always
   64-bit `signed`.
5. **VHPI name case** (server): report `vhpiNameP` upper-cased as VHDL tools
   do, with the exact case in `vhpiCaseNameP`?
