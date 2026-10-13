# VPI and VHPI: the simulator's procedural interfaces

Status: **proposal**. Nothing here is implemented.

## Problem

A siox test executable can only be driven by its own `#[test]` processes.
The verification tools hardware teams already use reach a simulator through
its standard C interface instead:

- **VPI** (IEEE 1364 / IEEE 1800 §36–38): the Verilog/SystemVerilog interface,
  used by cocotb's Verilog path, Icarus/Verilator-style plugins and most
  custom C models;
- **VHPI** (IEEE 1076-2019 §17–23): the VHDL interface, used by cocotb's VHDL
  path (nvc, GHDL, Riviera, Questa) and by VHDL foreign models.

Both expose the same things: an object model of the elaborated design
(instances, signals, ports, names, types), reading and writing values, and
callbacks on simulation events and phases. Supporting them lets cocotb
testbenches, Python models and existing C plugins drive siox designs
unchanged, and it is the base for the nvc co-simulation in
[interoperability.md](https://github.com/Siox-lang/siox-paper/blob/main/docs/interoperability.md#working-with-nvc)
(items 2 and 4 there).

## Decision

Implement both as **thin front-ends over one simulator-interface core** in the
fixed C runtime (`runtime/`). The core owns the object model, value access,
callbacks and the scheduler hooks; `runtime/vpi.c` and `runtime/vhpi.c` only
translate handles, property codes and value formats. Nothing is generated per
design: the core reads the immutable tables the design object already exports
for waveforms, plus a few new ones.

Build **VPI first**. It is the smaller standard, cocotb's best-trodden path,
and its 4-state values are a strict subset of siox's (interoperability rule 1
gives the mapping). VHPI follows on the same core and adds the exact nine-value
`Logic`.

No language change, no compiler-owned runner, no new dependency (`dlopen` is
in libc).

## What exists to build on

| Existing piece | Role for VPI/VHPI |
| --- | --- |
| `sx_wave_scope_*`, `sx_wave_signal_*` tables | Instance tree, signal names, scopes, widths, kinds, metavalue companions: the read side of the object model |
| `sx_wave_symbol_*` tables | Enum symbols (`Logic`, user enums): value strings and enum values |
| `sx_read_word(signal, word)` | Reading a signal's value plane (and companion) |
| `sx_process_commit` / `sx_process_changed(signal)` | Which signals changed in a delta: value-change callbacks |
| The `sx_runtime_run_test` loop | Phase boundaries: processes ran, events applied, commit, settled point, time advance |
| `sx_insert_event` | Timed events: `cbAfterDelay` / `vhpiCbAfterDelay` |
| `runtime/main.c` | Executable options: plugin loading joins `-o` and the test filter |

## The core (`runtime/sim.c`, `runtime/sim.h`)

**Object model.** Built once at startup from the tables:

- a root region for each elaborated root, child regions for instances
  (`sx_wave_scope_parents`), and a region for each struct/tuple/view value
  (a VPI `vpiStructVar`-like object, a VHPI record);
- a signal object for each storage leaf, with its scope, name, width, kind
  (`Bit`, `Logic`, packed vector, integer, real, enum) and direction for
  ports;
- arrays as indexable objects over their element leaves.

Full names use the standard separators at the front-end (`top.u0.q` for VPI,
`:top:u0:q` for VHPI); the core stores the scope chain. Names are reported as
written; siox is case-sensitive, which VPI expects and VHPI's `vhpiCaseNameP`
carries (`vhpiNameP` is the upper-cased VHDL form).

**Values.** One internal form: value words plus, for metavalue-carrying
signals, the discriminant companion. Front-ends convert:

| siox | VPI | VHPI |
| --- | --- | --- |
| `Logic` element | `vpi0/1/X/Z`, `'L'`→0, `'H'`→1, `'U'/'W'/'-'`→X (rule 1) | `vhpiU..vhpiDontCare`, exact (table maps siox discriminants `'0'=0,'1'=1,'Z'=2,'X'=3,'U'=4,..` to VHPI's `U=0,X=1,'0'=2,..`) |
| `Bit`, packed vector | `vpiScalarVal`, `vpiVectorVal`, `vpiIntVal`, binary/hex strings | `vhpiLogicVal`/`vhpiEnumVal`, `vhpiLogicVecVal`, `vhpiIntVal` |
| `integer`, `real` | `vpiIntVal`, `vpiRealVal` | `vhpiIntVal`/`vhpiLongIntVal`, `vhpiRealVal` |
| enum | `vpiIntVal`, `vpiStringVal` (symbol) | `vhpiEnumVal`, `vhpiStrVal` |

The discriminant mapping is a table in the runtime, generated from std's
`ULogic` declaration order rather than hard-coded twice.

**Writes.** A new design-object entry, `sx_deposit(signal, words, meta)`,
stores a value into a signal's storage and marks it changed, so the next
commit propagates it like any write. On top of it:

- *deposit* (`vpiNoDelay`/`vpiInertialDelay` with no driver, VHPI
  `vhpiDepositPropagate`): the value holds until the design drives the signal
  again;
- *force* / *release* (`vpiForceFlag`/`vpiReleaseFlag`, `vhpiForcePropagate`
  /`vhpiRelease`): a force table the commit consults before publishing a
  signal, so design drivers cannot overwrite a forced value until released;
- delayed puts (`vpiTransportDelay`, `vhpi_schedule_transaction` later):
  ordinary scheduled events.

A deposit is not a driver, so it does not take part in `Logic` resolution;
that is the standard behavior and the one nvc implements.

**Callbacks.** Each maps onto an existing loop boundary:

| Callback | VPI | VHPI | Where in the loop |
| --- | --- | --- | --- |
| start / end of simulation | `cbStartOfSimulation`, `cbEndOfSimulation` | `vhpiCbStartOfSimulation`, `vhpiCbEndOfSimulation` | before reset; after the last test |
| value change | `cbValueChange` | `vhpiCbValueChange` | after each commit, per changed signal |
| read/write sync | `cbReadWriteSynch` | `vhpiCbEndOfProcesses` | after processes ran, before the commit |
| read-only sync | `cbReadOnlySynch` | `vhpiCbLastKnownDeltaCycle` | at the settled point where waveforms sample |
| next time step | `cbNextSimTime` | `vhpiCbNextTimeStep` | when `sx_now` advances |
| timer | `cbAfterDelay` | `vhpiCbAfterDelay` | a new event kind in the event wheel |

A read-only callback may not write (the standard's rule; the core rejects
it). A write from a value-change or read/write callback is visible in the next
delta, as a process write would be.

**Time.** siox time is an unsigned 64-bit femtosecond count. VPI reports it as
`vpiSimTime` with the precision `vpiTimePrecision = -15`; VHPI as
`vhpiTimeT` in femtoseconds (`vhpiResolutionLimitP`).

**Control.** `vpi_control(vpiFinish/vpiStop)` and `vhpi_control(vhpiFinish
/vhpiStop)` end the run the way `finish`/`stop` do; `vhpiReset` is not
supported.

## Front-ends

`runtime/vpi.c` implements the VPI subset cocotb's `VpiImpl` uses:
`vpi_handle`, `vpi_handle_by_name`, `vpi_handle_by_index`, `vpi_iterate` /
`vpi_scan` (modules, nets, regs, ports, struct members), `vpi_get`
(`vpiType`, `vpiSize`, `vpiDirection`, ...), `vpi_get_str` (`vpiName`,
`vpiFullName`, `vpiType`), `vpi_get_value`, `vpi_put_value`,
`vpi_register_cb`, `vpi_remove_cb`, `vpi_get_time`, `vpi_control`,
`vpi_chk_error`, `vpi_release_handle`/`vpi_free_object`, `vpi_get_vlog_info`
(product "siox"), and `vpi_printf`.

`runtime/vhpi.c` implements the matching VHPI subset cocotb's `VhpiImpl`
uses: `vhpi_handle` (`vhpiRootInst`), `vhpi_handle_by_name`,
`vhpi_handle_by_index`, `vhpi_iterator` / `vhpi_scan` (internal regions,
signal/port/generic declarations, indexed and selected names), `vhpi_get`,
`vhpi_get_str`, `vhpi_get_phys`, `vhpi_get_value`, `vhpi_put_value`,
`vhpi_register_cb`, `vhpi_remove_cb`, `vhpi_get_cb_info`, `vhpi_get_time`,
`vhpi_control`, `vhpi_check_error`, `vhpi_release_handle`, `vhpi_printf`.

Both lists are what cocotb calls today; confirm them against cocotb's
`share/lib/gpi/VpiImpl.cpp`/`VhpiImpl.cpp` before starting, and add what a
second client needs only when it appears. Unsupported calls set the
standard's error record (`vpi_chk_error`, `vhpi_check_error`) instead of
crashing.

**Headers.** Plugins are compiled against the standard headers, so every
constant (object types, properties, value formats, callback reasons) must
have the standard's value. Use the IEEE-published machine-readable
`vpi_user.h`/`sv_vpi_user.h` and `vhpi_user.h` if their licence allows
redistribution in an MIT/Apache project; otherwise write our own headers from
the standards' tables. Resolve this before any code (see open questions).

## Running a design under a plugin

Two things the executable cannot do yet:

1. **Load plugins.** New options: `--vpi <lib.so>` runs the library's
   `vlog_startup_routines`; `--vhpi <lib.so>[:<entry>]` runs its
   `vhpi_startup_routines` or the named bootstrap function, as nvc's
   `--load` and Questa's `-foreign` do. Loading happens before reset, so
   `cbStartOfSimulation` is seen.
2. **Run a design with no testbench.** cocotb supplies the stimulus, so the
   executable must elaborate a design top with no `#[test]` entity and run
   until the plugin finishes it or no event or callback remains. This is a
   build mode: `sioxc --sim --top Top -o top-sim` (name open), producing the
   same native executable with one root and no foreground process. `--test`
   keeps its meaning.

cocotb then needs a small simulator recipe (its runner/Makefile entry for
siox: build with `sioxc --sim`, run with `--vpi $(cocotb-config --lib-name-path
vpi siox)`); that belongs upstream in cocotb once the interface works.

## Fit with the rest of the runtime

- **Determinism.** Callbacks run on the coordinator at loop boundaries, in
  registration order; plugin writes enter the same commit path as process
  writes. Waveforms show deposited and forced values like any other.
- **Multiprocess.** [multiprocess.md](multiprocess.md) keeps one coordinator
  that owns time, commit and host effects; callbacks are host effects and run
  there, between epochs, never on a worker.
- **Debugging.** DWARF source debugging is unaffected; a plugin and gdb can
  attach to the same run.
- **The reverse direction** (siox as a VHPI plugin inside nvc) reuses the
  value mapping and naming of this core but not the object model, and is a
  separate step.

## Delivery order

1. **Settle the headers and the subset.** Licence question answered; the exact
   cocotb call lists recorded; a minimal C plugin (print the hierarchy, read a
   signal at each time step) written as the first acceptance test.
2. **Read-only core + VPI.** Object model, names, `vpi_get_value`,
   `vpi_get_time`, plugin loading, start/end-of-simulation callbacks. The
   C plugin walks a corpus design and reads values.
3. **Callbacks.** Value change, read/write and read-only sync, next time step,
   timers, removal; the C plugin drives a clock with `cbAfterDelay`.
4. **Writes.** `sx_deposit` in the design object (ABI bump), deposit, force,
   release; the C plugin toggles inputs and checks outputs.
5. **Design-only executable** (`sioxc --sim`) and **cocotb**: run cocotb's own
   simulator-agnostic tests that apply to a non-Verilog design, plus a siox
   corpus design driven by a cocotb test.
6. **VHPI front-end** over the same core, exact `Logic`; cocotb's VHPI path.
7. Later, when wanted: `vhpi_schedule_transaction` and drivers, VHPI foreign
   architectures, siox-in-nvc co-simulation.

Each step is usable on its own; steps 2–5 are the cocotb milestone.

## Acceptance gates

- A C plugin in the repository's tests, compiled with the system C compiler
  that already builds the runtime, run against corpus designs in default and
  `bitpack` builds: hierarchy and names, values (including X/Z and every
  `Logic` symbol through VHPI), each callback reason, deposit/force/release.
- The same plugin's output compared with nvc for VHPI (and Icarus for VPI)
  on an equivalent VHDL/Verilog design where one exists, as the logic tables
  are compared with nvc today.
- cocotb: at least one corpus design under a cocotb test, in CI once cocotb is
  available there.
- No change to `--test` executables that load no plugin: same results,
  same waveforms, no measurable overhead (callbacks cost nothing when none
  are registered).

## Open questions

1. **Headers.** Can the IEEE `vpi_user.h`/`vhpi_user.h` be redistributed
   under the project's licence, or do we write our own from the standards'
   constant tables?
2. **Flag names.** `--sim` for the design-only build, `--vpi`/`--vhpi` (or
   one `--load` as nvc) for plugins.
3. **Name case for VHPI.** Report `vhpiNameP` upper-cased as VHDL tools do
   (with the exact case in `vhpiCaseNameP`), or as written?
4. **Struct and tuple values through VPI.** Expose them as
   `vpiStructVar` members (SystemVerilog VPI) or only their leaves?
