# siox

**siox** ("silicon oxide") lets you describe digital hardware in a modern,
Rust-flavoured language and simulate it right away — write a circuit, drive it
with a testbench, and watch it run, with assertions and waveforms.

It's early but real: the compiler and simulator work end to end. There's no
synthesis or analogue layer yet — this is the simulation-first phase, so expect
some sharp edges.

## Get the compiler

Build `sioxc` from source. The compiler needs:

- [Rust](https://rustup.rs) 1.90 or newer;
- LLVM 22 development libraries (the version selected in `Cargo.toml`);
- Clang and zlib for native `#[test]` executables. Clang links the LLVM-emitted
  design with a fixed C scheduler, command-line, and waveform runtime; those
  design-independent runtime objects are normally precompiled with `sioxc`.

```bash
git clone --recursive https://github.com/Siox-lang/sioxc
cd sioxc
cargo build --release
```

The FST waveform writer is vendored by reference as a git submodule, so
`--recursive` matters. An existing clone catches up with
`git submodule update --init --recursive`; the build stops with that command
named if it is missing. See [`third_party/`](third_party/README.md).

That produces `target/release/sioxc` — the compiler. Put it on your `PATH` or
call it by path. siox compiles designs through LLVM, which is the permanent
backend. Frontend-only library consumers such as editors can instead disable
default features and do not need LLVM; see the
[embedding API documentation](docs/interoperability.md#compiler-embedding-api).

Native execution has one path: canonical Process IR is emitted through LLVM
and linked with the fixed runtime; no design-specific C is generated. Typed
source/test processes lower directly under `ir/lower` into that product. One
input-side migration remains: normalized `Driver`/`EventBlock` hardware is
still imported into Process IR until those scheduler forms are derived from it.
See the [architecture status](docs/architecture.md#current-process-ir-ingress-boundary).

## Write your first circuit

Save this as `counter.siox` — an 8-bit counter that ticks up on each clock edge,
plus a testbench that drives it:

```siox
module counter;

using std::bits::unsigned;
using std::logic::{Bit, Logic};

entity Counter {
    clk: Bit in,
    rst: Logic in,
    count: unsigned[8] out,
}

impl Counter {
    let value: unsigned[8] = 0;

    update: process {
        if clk.rising() {            // runs only on a rising clock edge
            if rst == '1' { value = 0; }
            else { value = value + 1; }
        }
    }

    count = value;                   // a wire: always equal to `value`
}

#[test]
entity CounterTest {}

impl CounterTest {
    let clk: Bit = '0';
    let rst: Logic = '1';
    let count: unsigned[8];
    let dut: Counter = { .clk = clk, .rst = rst, .count = count };

    clock: process {
        clk = not clk after 5ns;     // free-running clock, 10 ns period
    }

    stimulus: process {
        await 10ns;                  // hold reset for one edge
        rst = '0';
        for i in 0..9 { await clk.rising(); } // let ten more edges pass
        assert!(count == 10, "counter should reach 10");
    }
}
```

Two kinds of logic sit side by side: a concurrent **wire**
(`count = value;` is always equal to `value`) and an ordered clocked
**process** (`update: process { if clk.rising() { … } }`). The `update:` label
is optional and VHDL-style: it names the process in diagnostics and tools.

## Run it

The `#[test]` entity is a testbench. Run every testbench in a file with:

```console
$ sioxc --test counter.siox -o counter-tests
$ ./counter-tests

running 1 test
test counter::CounterTest ... ok

test result: ok. 1 passed; 0 failed; 0 filtered out
```

It works like `rustc --test`: `sioxc --test` compiles each `#[test]` into a
native test executable. Run that executable normally, or pass a qualified name
to select a subset: `./counter-tests counter::CounterTest`.

## See the waveforms

The compiled test executable—not `sioxc`—writes the requested waveform while
it runs. Use portable text VCD or compressed FST for larger traces:

```bash
./counter-tests -o counter.fst
./counter-tests -o counter.vcd
# Both may be written in one run:
./counter-tests -o counter.vcd -o counter.fst
```

The files can then be opened in a waveform viewer such as
[GTKWave](https://gtkwave.sourceforge.net/) or
[Surfer](https://surfer-project.org/). Both formats contain the same hierarchy
and scheduler change points; multiple selected tests occupy one monotonic
timeline.

## The commands you'll use

| Command | What it does |
| --- | --- |
| `sioxc file.siox --emit metadata` | type-check and elaborate without code generation |
| `sioxc --test file.siox -o tests` | compile the `#[test]` test executable |
| `./tests -o out.vcd [filter]` | run generated tests and write text VCD |
| `./tests -o out.fst [filter]` | run generated tests and write compressed FST (any non-`.vcd` path) |
| `sioxc file.siox` | compile the sole structural root to a native object (`--top` when ambiguous) |
| `sioxc -D warnings file.siox` | treat every warning as an error; `-A`/`-W`/`-D`/`-F <lint>` set one lint's level, as in rustc |

The standard library loads from `./std` by default; add `--std <dir>` if it
 lives elsewhere. Peeking under the hood? `sioxc file.siox --emit
ast|ir|tree` prints the
parse tree, lowered IR, and instance hierarchy.

## Editor support

[`siox-lsp`](https://github.com/Siox-lang/siox-lsp) is maintained in its own
repository and depends on this compiler through Cargo Git. It provides live diagnostics,
go-to-definition, hover, completion, rename, and more:

```bash
git clone git@github.com:Siox-lang/siox-lsp.git
cargo build --manifest-path siox-lsp/Cargo.toml
```

Full capability list and setup notes:
[docs/interoperability.md](docs/interoperability.md).

## Learn more

- **[Examples](https://github.com/Siox-lang/siox-tests)** — a repo of runnable
  `.siox` programs: counters, FSMs, a FIFO, SPI, RISC-V fragments, tristate
  buses, and more.
- **[Language specification](docs/language.md)** — the full syntax and
  semantics (with an at-a-glance tour up front).
- **[docs/](docs/README.md)** — compiler architecture, simulation, testing, the
  standard-library reference, and interoperability.
- **[CHANGELOG](CHANGELOG.md)** — what's changed.
- **[House rules](HOUSERULES.md)** — conventions for contributors: the design
  principle, pipeline layering, diagnostics, and the testing gate.

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE),
at your option.
