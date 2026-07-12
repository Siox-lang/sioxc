# siox

**siox** ("silicon oxide") is a digital hardware description language and an
event-driven simulator for it, written as a Rust workspace. You write hardware
in a modern, Rust-flavoured syntax; `sioxc` type-checks it, elaborates the
instance hierarchy, lowers it to a digital IR, and simulates it — with a test
runner, assertions, and VCD waveform output.

It is **Phase 1: simulation-first**. There is no analogue, schematic, or
synthesis layer yet (those are Phases 2–3 — see [docs/roadmap.md](docs/roadmap.md)).

> **Status: early but real.** The full pipeline works — lexer, parser, name
> resolution, type/kind checking, elaboration, digital IR, and a delta-cycle
> simulator with `#[test]` discovery, `await`/clock timing, assertions, and VCD
> export. Two engines run every design: an LLVM JIT/AOT backend (default) and a
> delta-cycle interpreter that doubles as a differential oracle. The language
> surface is still moving; expect sharp edges.

## A first look

```siox
module counter;

using std::logic::{Logic, Clock};
using std::bits::uint;

entity Counter<W: integer> {
    in clk: Clock;
    in rst: Logic;
    in en: Logic;
    out count: uint[W];
}

impl Counter<W: integer> {
    let value: uint[W] = 0;

    if clk::rising {              // sequential: updates on the rising edge
        if rst == '1' { value = 0; }
        else if en == '1' { value = value + 1; }
    }

    count = value;               // combinational: continuous assignment
}

#[test]
entity CounterTest {}

impl CounterTest {
    let clk: Logic = '0';
    let rst: Logic = '1';
    let en: Logic = '1';
    let count: uint[8];
    let dut = Counter<W = 8> { .clk, .rst, .en, .count };

    clk = not clk after 5ns;     // free-running clock, 10ns period

    await 10ns;
    rst = '0';
    for i in 0..9 {              // inclusive range: ten rising edges
        await clk::rising;
    }
    assert!(count == 10, "counter should reach 10");
}
```

```console
$ sioxc test counter.siox

running 1 test
test CounterTest ... ok

test result: ok. 1 passed; 0 failed
```

## Building

Requires Rust (edition 2021, `rust-version = 1.90`).

The default backend is the **LLVM JIT/AOT** engine, which needs a matching local
LLVM toolchain (the `inkwell` version feature in `crates/siox-llvm/Cargo.toml`
pins the LLVM major version). If you don't have that LLVM installed, build the
**interpreter-only** path instead — it needs no external toolchain and is the
same engine used as the correctness oracle:

```bash
# Interpreter only (no LLVM toolchain needed):
cargo build --no-default-features --features interp
cargo test  --no-default-features --features interp

# Full build (needs a matching local LLVM):
cargo build
cargo test
```

CI runs the interpreter path, so it works on any machine; the LLVM backend is
validated locally.

## Using the compiler

```bash
sioxc <file.siox>            # compile to a native object (AOT)
sioxc check  <file.siox>     # parse, resolve, type-check, elaborate, lower
sioxc test   [filter]        # run #[test] entities (libtest-style output)
sioxc test   --no-run -o bin # link a native test binary
sioxc sim    <file> --wave out.vcd   # simulate and dump a waveform

# Debug views:
sioxc ast  <file>            # parse tree
sioxc ir   <file>            # lowered simulation IR
sioxc tree <file>            # elaborated instance hierarchy
```

By default the standard library is loaded from `./std`; pass `--std <dir>` to
point elsewhere. See [`examples/`](examples/) for runnable programs (counters,
FSMs, a FIFO, SPI, RISC-V ALU/decoder fragments, tristate buses, generate
loops, and more).

## What the language has

- **Entities and impls** with parameters (`Counter<W>`), an instance hierarchy,
  and port connections — including struct/bus bundles and `inout` tristate nets.
- **Combinational vs. sequential** logic kept distinct: continuous assignments
  vs. `if clk::rising { … }` event blocks with `::event`/`::old` edge queries.
- **A four-value logic type** (`Logic`: `'0'/'1'/'Z'/'X'`) with std_logic truth
  tables and parallel-driver resolution, plus a two-value `Bit`, `Clock`, and a
  library `uint[N]`/`int[N]` with operator-trait-driven signedness.
- **Generics, trait bounds, `where`**, Rust-style operator traits, derived
  nominal types, and `#[…]` attributes.
- **A test/stimulus layer**: `#[test]` entities, `await` (time / edge /
  condition), background `clock`s, `assert!`/`warn!`/`print!`, `extern "C"`,
  file IO, and VCD waveforms.
- **Diagnostics** with stable codes, plus lints (possible latch, unused import,
  unresolved multiple drivers, non-exhaustive/unreachable match).

## Documentation

The [`docs/`](docs/) folder is the source of truth — start at
[docs/README.md](docs/README.md):

- [docs/spec.md](docs/spec.md) — the language specification.
- [docs/architecture.md](docs/architecture.md) — the compiler pipeline and crate layout.
- [docs/implementation.md](docs/implementation.md) — stage-by-stage status.
- [docs/std.md](docs/std.md) — the standard-library reference.
- [CHANGELOG.md](CHANGELOG.md) — what has changed.

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE),
at your option.
