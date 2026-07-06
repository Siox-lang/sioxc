# Changelog

All notable changes to `siox` are recorded here. The project is pre-release
(**Phase 1: simulation-first**), so everything lives under *Unreleased*; the
format loosely follows [Keep a Changelog](https://keepachangelog.com).

The core pipeline — lexer, parser, name resolution, type/kind checking,
elaboration, digital IR, and a delta-cycle simulator with `#[test]` discovery,
assertions, and VCD export — predates this changelog. See
[`docs/implementation.md`](docs/implementation.md) for per-stage status.

## [Unreleased]

### Added
- **`await` / `clock` timing primitives** — `await 10ns` (advance time),
  `await clk::rising` (edge; also `::falling`/`::event`), `await cond`
  (condition), and `clock(clk, period)` for a free-running background clock.
  Runs identically on the interpreter, the JIT, and the native test binary.
- **Hierarchical simulation** — an entity may instantiate sub-entities; each
  instance lowers into its own signal namespace (`Add2.s1.a`) and every port
  connection becomes a driver. Multiple instances of one entity take
  per-instance parameters (`Reg<8>` and `Reg<4>` in one parent size correctly).
- **Bare-file compile / `sioxc <file>`** — compiles the `#[top]` design to a
  native object (rustc-shaped); `--top` picks the top entity.
- **`sioxc test --no-run`** — links a standalone native test binary that runs
  every `#[test]` with libtest-style output and a name filter.
- **Compiled backend** (`siox-llvm`, inkwell): a **JIT** (`sioxc test`) and
  **AOT** native objects, both driving the shared test runner via an `Engine`
  trait.
- **Differential harness** — the JIT is verified bit-for-bit against the
  interpreter oracle across the expression surface (`--features interp`).
- Examples: `hierarchy_test`, `multiclock_test`, `await_test`, `top_counter`.

### Changed
- **LLVM is the default execution engine.** `sioxc test` JIT-runs and
  `sioxc sim --wave` JIT-traces; the default build needs an LLVM toolchain
  (`--no-default-features` for an LLVM-free build).
- **Interpreter feature-gated off** (`interp`, off by default). It stays in-tree
  as the differential oracle and the >64-bit fallback; the engine-generic test
  runner (`Testbench`, `await`/`clock`, `assert!`) is always compiled and the
  JIT drives it.
- **Simulation time moved to the runner/kernel.** The `Engine` trait is now
  purely combinational (`set`/`read`/`settle`); the runner owns `time_fs` and
  the event wheel — deliberately the factoring digital events, Phase-2 analogue
  timesteps, and cocotb will all share.
- **Compiler renamed `siox` → `sioxc`** (crate + binary) — the rustc side of the
  planned rustc/cargo split (the cargo-like `pcb`/`circuit` is a future repo).
- `test` reports in **libtest style** (`running N tests` … `test result: …`).

### Fixed
- **JIT-traced VCD timestamps** were frozen at `#0` (the JIT engine reported
  time 0). The runner now owns time, so waveforms carry real timestamps and
  multiple clocks interleave correctly on one event wheel.
- **Hierarchical designs** with submodules wired up wrong (lowering was flat,
  per entity type); now per-instance with connection drivers.
- Divide-by-zero yields `0` consistently on both engines; the IR validator
  rejects malformed IR before codegen.

### Deferred / by design
- **Signedness is not compiler-hardcoded.** `int[N]`/`uint[N]` operators — and
  signed compare/divide/arithmetic-shift — will live in `std` as operator-trait
  impls, deleting the last numeric shim (#37). The compiler already inlines such
  impls (`Complex` in `std/math.siox` is the proof).
- **cocotb** integration (VPI/GPI) is a later, separate layer (#36).
- Two instances of the *same* entity at the testbench top level currently share
  one DUT in the name map (#39).
