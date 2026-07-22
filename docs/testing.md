# Testing

siox testbenches, how to run them, and how the compiler itself is tested.

## `#[test]` entities are testbenches

A testbench is an entity marked `#[test]`. It instantiates a design-under-test,
drives its inputs over time, and asserts on its outputs — the HDL equivalent of
a `#[test]` function:

```siox
#[test]
entity CounterTest {}

impl CounterTest {
    let clk: Bit = '0';
    let rst: Logic = '1';
    let count: uint[8];
    let dut: Counter = { clk, rst, count };

    clk = not clk after 5ns;         // free-running clock, 10 ns period
    await 10ns;                      // hold reset for one edge
    rst = '0';
    for i in 0..9 { await clk.rising(); }
    assert!(count == 10, "counter should reach 10");
}
```

Testbench bodies are sequential: statements run in order, `await` advances
simulation time (see [simulation.md](simulation.md)), and a testbench `let` is a
mutable local with ordinary sequential assignment. Method calls on the DUT or on
struct-typed locals work in stimulus, so a testbench can drive a design through
a method result.

## Reporting

- `assert!(cond, "msg")` — fail the test if `cond` is false.
- `warn!(…)` / `print!(…)` — diagnostics and logging; enum and logic values
  render symbolically (`Idle`, `'Z'`), not as raw codes.
- `stop!` / `finish!` — end the run.

## Running

`sioxc test` finds every `#[test]` entity, simulates it on the JIT, and reports
pass/fail in libtest style — like `cargo test`:

```console
$ sioxc test counter.siox
running 1 test
test CounterTest ... ok

test result: ok. 1 passed; 0 failed
```

- **Filter by name:** `sioxc test counter.siox Counter` runs the matching
  subset.
- **A directory:** `sioxc test <dir>` runs every `.siox` file under it as its
  own module, then an aggregate result.
- **Native binary:** `sioxc test <file> --no-run -o <bin>` builds a standalone
  native test binary (the compiled testbench harness) that exits 0 on pass — for
  CI without the toolchain in the loop, or handing a design off to run
  elsewhere.

A file with no `#[test]` entity reports zero tests rather than erroring.

## How the compiler is tested

- **Per-crate unit tests** across the pipeline (`cargo test --workspace`).
- **JIT behaviour tests** (`tests/jit.rs`) drive the JIT across
  the whole expression surface — arithmetic, slices, concat, enum match,
  struct/array signals, clocked designs — and assert golden signal values. Those
  golden values were captured from the delta-cycle interpreter that used to be
  the differential oracle, before that engine was removed, so coverage is
  preserved exactly.
- **Conformance corpus.** The runnable `.siox` programs (counters, FSMs, a FIFO,
  SPI, RISC-V fragments, …) live in the
  [Siox-lang/siox-tests](https://github.com/Siox-lang/siox-tests) repo. CI checks
  out the corpus and compiles every program through the freshly built compiler,
  so a regression there fails the build.
- **CI** installs an LLVM toolchain and runs the real `--features llvm` suite
  plus the corpus, and separately builds `--no-default-features` to keep the
  frontend-only path honest.
