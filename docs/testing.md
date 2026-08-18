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
Unconnected `real` locals and fields likewise use floating-point semantics for
arithmetic, comparisons, conditionals, negation, and formatted output; their
native storage remains the same f64 bit representation used by real signals.
Calls to declared `extern "C"` functions are also valid in native testbench
expressions; parameter and return conversion follows the declaration (`real`
crosses as C `double`, `integer` as the signed ABI word, and packed scalars as
an unsigned ABI word).
Native kernel-`integer` locals and loop counters retain signed comparison,
division, arithmetic-right-shift, and formatting semantics. Each `for` body has
its own value/type scope: a range binds an `integer`, collection iteration binds
the element type, nested shadowing restores the enclosing loop metadata, and
leaving the loop restores any same-named outer local.
The same signed behavior applies to module constants, function/method results,
struct fields, plain connected integers, and width-constrained integer signals;
constrained values are sign-extended from their stored width before use.
Named `real` constants and real-typed parameters/returns of ordinary functions
and methods retain that same representation while native code inlines them.
Struct-local numeric leaves use their declared width as well: fields wider
than one ABI word preserve every word, while a field narrower than the
harness-wide value still wraps at its own boundary.
Unconnected scalar/vector arrays are materialized one typed element at a time,
so literals, indexing, element mutation, and same-shaped array copies preserve
arbitrary-width elements too.
Materialization is recursive for nested arrays, arrays of structs, and arrays
of fixed-size strings; composite copies match scalar leaf paths rather than
collapsing an aggregate into one machine word.
Unconstrained string/array locals acquire the initializer's concrete native
storage shape, and later assignments must match it. Struct locals accept the
same named, positional, typed-positional, and spread-update forms during
reassignment as during initialization; every form writes the flattened fields
in declaration order. Recursive initialization retains nested struct literals,
whole-struct copies, nested spreads, arrays of structs, and fixed-size string
fields rather than defaulting their descendant leaves.
Explicit local ranges retain their declared logical indices and direction.
This applies to numeric vectors and arrays alike: indexing, iteration, string
literal initialization of logic arrays, and the `'left`, `'right`, `'high`,
`'low`, `'length`, and `'ascending` attributes all observe the declared range.
Named `range` constants can be used as local type indices, and signed bounds
remain addressable. Integer constants can likewise supply local widths; based
and `_`-separated literal spellings retain the same width and value through
analysis, elaboration, IR, and native test generation, including direct real
reassignment and comparison.

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
- **Waveforms:** `-o trace.fst` writes compressed FST; `-o trace.vcd` writes
  portable text. The path's extension picks the format, so FST is the default
  without naming it. `--output=<path>` and `-o<path>` are equivalent, and
  passing `-o` twice with one of each extension writes both.
  Either option may precede or follow the test filter, and both may be requested
  together with different paths; using the same path is rejected. They share
  the same 1 fs scheduler samples and place multiple tests consecutively on one
  monotonic timeline.
- **A directory:** corpus orchestration belongs to the build/test tooling, not
  the compiler. `scripts/test-corpus.sh` compiles and runs each `.siox` file.
- **Native binary:** `sioxc --test <file> -o <bin>` builds a standalone test
  executable that exits 0 on pass.

A file with no `#[test]` entity reports zero tests rather than erroring.

## How the compiler is tested

- **Unit and integration tests** across the package (`cargo test`).
- **Native backend tests** compile and link focused designs, then assert values
  through the exported word ABI, including multi-word values.
- **Waveform interoperability tests** emit VCD and FST together, decode FST
  with the pinned libfst reader, and compare hierarchy, values, and timestamps.
- **Conformance corpus.** The runnable `.siox` programs (counters, FSMs, a FIFO,
  SPI, RISC-V fragments, …) live in the
  [Siox-lang/siox-tests](https://github.com/Siox-lang/siox-tests) repo. CI checks
  out the corpus and compiles every program through the freshly built compiler,
  so a regression there fails the build.
- **CI** installs an LLVM toolchain, builds, and runs the full test suite plus
  the corpus through the freshly-built compiler.
