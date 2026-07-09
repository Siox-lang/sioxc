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
- **Rust-style operator traits** — operator overloading now uses named traits
  with Rust's `std::ops` names (`impl Add for Complex { fn add(...) }`,
  `BitAnd`/`bitand` for `and`, `Not` for unary `not`); one `impl Ord`
  (`fn cmp -> Ordering`) derives all six comparisons (replaces `impl "<=>"`).
  Mixed operands use the trait's type argument, exactly Rust's spelling:
  `impl Add<integer> for Complex`. Quoted operator-trait names
  (`impl "+" for T`) are removed — a targeted parse error points at the
  Rust-style name. Future custom operators are reserved as
  `impl ops::custom<"sym", Rhs> for T`.
- **uint/int operators moved to std** — only the kernel types (`integer`,
  `real`) keep built-in operators; `uint[N]`/`int[N]` arithmetic and shifts
  are now `impl Add for uint` etc. in `std/bits.siox`, and `int` gains
  **signed comparison** via a sign-aware `impl Ord for int` (library source,
  not compiler code; `self::width` is available inside operator impls).
  Overload selection tightened: exact rhs match, then integer-literal
  coercion — a sole candidate is never taken on a known mismatch.
- **Python-style array iteration + testbench locals** — `for x in xs`
  iterates any array (`for i in 0..n` now binds `i` too); `xs::len` joins the
  `::` metadata attributes; testbench `let`s run in statement order and
  unconnected names are real locals. All three paths (interp, JIT, native
  binary — arrays via generated id tables).
- **Rust-style `if` expressions** — `y = if sel { a } else { b };` with
  required `else` and `else if` chains; lowers to a select everywhere
  (hardware drivers, operator-impl bodies via the inliner, testbench
  evaluation, and the native binary as a C conditional). There is still no
  `?:` — the spec freezes if/else as the only conditional form.
- **VHDL-style delayed assignment** — `clk = not clk after 5ns;` is the
  canonical clock generator (self-toggle registers on the event wheel), and
  `rst = '0' after 12ns;` is a one-shot delayed write (value evaluated at
  schedule time). `after` is positional, not a reserved word; testbench-only
  (hardware rejects it); works on interp, JIT, and the native binary (clock
  idiom; one-shots error cleanly there for now).
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
- Examples: `hierarchy_test`, `multiclock_test`, `instances_test` (two
  instances of one entity on different clocks), `await_test`, `top_counter`.

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
- **Split the simulation kernel from the interpreter.** The test runner —
  `Engine` trait, `#[test]` discovery, stimulus, `await`/`clock` scheduler,
  time, waveform recording — moved to a new **`siox-run`** crate (engine-agnostic,
  always compiled). `siox-sim` is now *only* the delta-cycle interpreter (one
  `Engine`), pulled in via `--features interp` as the differential oracle — the
  rustc/Miri split at the crate level.
- **Compiler renamed `siox` → `sioxc`** (crate + binary) — the rustc side of the
  planned rustc/cargo split (the cargo-like `pcb`/`circuit` is a future repo).
- `test` reports in **libtest style** (`running N tests` … `test result: …`).

- **Tops-only lowering.** Only `#[top]`/`#[test]` roots lower; sub-entities and
  a testbench's DUTs lower recursively per-instance (`CounterTest.dut.*`), so
  two instances of one entity in a testbench no longer share state.
- **The native test binary got a real event wheel** — generated C tracks
  simulation time and per-clock next-edge state, so multiple clocks of
  different periods interleave correctly (previously all clocks toggled in
  lockstep) and `await <duration>` advances real time.

- **`wait` and `tick()` removed — `await` is the one timing primitive.**
  `wait` is a parse error (recovering as `await`); `tick()` fails with a
  pointer to the replacements: a manual pulse is plain code
  (`clk = '1'; await 5ns; clk = '0';`) and edge-driven tests use a generator
  (`clk = not clk after 5ns;`) with `await clk::rising`. `tick()` returns
  later as a std function. All examples converted.

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
