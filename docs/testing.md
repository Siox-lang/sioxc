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
    let count: unsigned[8];
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
a method result. Strings retain their array semantics here: locals can be
initialized or assigned from another same-length string, and equality compares
their characters (including the zero-character empty-string case).
Unconnected `Char` locals, string elements, and `Char` fields retain Unicode
character context for initialization, assignment, and comparison just like
DUT-connected character signals.

## Reporting

- `assert!(cond, "msg")` — fail the test if `cond` is false.
- `warn!(…)` / `print!(…)` — diagnostics and logging; enum and logic values
  render symbolically (`Idle`, `'Z'`), `Char` and string values render as
  Unicode, and arbitrary-width numeric values retain every decimal digit.
- `stop!` / `finish!` — end the run.

## Running

`sioxc --test` finds every `#[test]` entity and compiles a native test
executable. `sioxc` is only the compiler; run the executable to execute or
filter tests:

```console
$ sioxc --test counter.siox -o counter-tests
$ ./counter-tests
running 1 test
test counter::CounterTest ... ok

test result: ok. 1 passed; 0 failed
```

- **Filter by qualified name:** `./counter-tests counter::CounterTest` runs the
  matching subset. Partial names also work as filters.
- **A directory:** corpus orchestration belongs to the build/test tooling, not
  the compiler. `scripts/test-corpus.sh` compiles and runs each `.siox` file.
- **Native binary:** `sioxc --test <file> -o <bin>` builds a standalone test
  executable that exits 0 on pass.

A file with no `#[test]` entity reports zero tests rather than erroring.

## How the compiler is tested

- **Unit and integration tests** across the package (`cargo test`).
- **Native backend tests** compile and link focused designs, then assert values
  through the exported word ABI, including multi-word values.
- **Conformance corpus.** The runnable `.siox` programs (counters, FSMs, a FIFO,
  SPI, RISC-V fragments, …) live in the
  [Siox-lang/siox-tests](https://github.com/Siox-lang/siox-tests) repo. CI checks
  out the corpus and compiles every program through the freshly built compiler,
  so a regression there fails the build.
- **CI** installs an LLVM toolchain, builds, and runs the full test suite plus
  the corpus through the freshly-built compiler.
