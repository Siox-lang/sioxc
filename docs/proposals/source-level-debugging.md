# Source-level debugging

Status: **proposal**. Waveform output is implemented; nothing else here is.

A `#[test]` build is a native executable compiled through Clang, so a debugger
already attaches to it. What is missing is the mapping back to `.siox`: a
failure says *what* went wrong but not *where*, and a debugger sees generated C
rather than source. This proposes closing that gap in three tiers, the first of
which needs no debugger at all.

## Current state

Verified against a small FIFO and counter.

**Waveforms — implemented.** `--vcd` and `--fst` both work, and the dump
includes internal state, not only ports:

```
$scope module FifoTest $end
 $scope module f $end
  $var wire 8 v8  mem[0] $end
  $var wire 4 v12 head   $end
  $var wire 4 v14 used   $end
```

This is the standard HDL debugging loop and it is available now through
GTKWave, Surfer, or a vendor waveform viewer.

**Debugger attach — partial.** `gdb ./test.bin` breaks and produces a
backtrace:

```
Breakpoint 1, sx_settle ()
#0  sx_settle ()
#1  sx_run_settle ()
#2  test_r1 ()
```

The binary is not stripped, but it carries **no DWARF** — `readelf -S` reports
zero debug sections — so there are no source lines and no variable names.

**Runtime failures — no source location.** The two classes differ:

| failure           | reported today                                              |
| ----------------- | ----------------------------------------------------------- |
| `assert!`         | the message only                                             |
| range violation   | signal path, declared range, offending value — not the write |

```
test fail::FailTest ... FAILED
    the counter should be 99 here
```

Nothing names `fail.siox:18`, so a failing assertion in a large testbench has
to be found by reading.

## Goals

1. Every runtime failure names a source location.
2. A debugger maps machine code to `.siox` lines.
3. Values are inspectable by their siox name.

## Tier A — failures carry their source location

The highest value per unit of work, and it needs no debugger, no DWARF, and no
change to how anything is compiled. It also pays off in CI logs, where no
debugger is present.

Every runtime failure should render like a compile diagnostic:

```
test fail::FailTest ... FAILED
  --> fail.siox:18:5
   |
18 |     assert!(y == 99, "the counter should be 99 here");
   |     ^ the counter should be 99 here
```

Cases to cover:

- **Assertions.** Carry the `assert!` span into the generated C and print it on
  failure.
- **Range violations.** The signal is already named; add the span of the
  assignment that wrote the out-of-range value, which is the thing the author
  needs to look at.
- **Oscillation / delta cap.** Name the drivers that kept changing.
- **Runtime file I/O.** Already reports a source-relative path; align its
  rendering with the above.

Most of the spans already exist. `Signal` carries the declaration span of its
owning port or `let`, and IR diagnostics are anchored. What is missing is
per-*assignment* spans reaching the generated C, and a small runtime formatter
shared by every failure path.

## Tier B — DWARF through `#line`

Emit `#line N "<absolute path>.siox"` into the generated C and pass `-g` to
Clang. Clang then produces DWARF that points at `.siox` files directly, which
gives, with no debugger-specific work:

- `break fifo.siox:34`
- stepping through siox source in gdb, lldb, or any DWARF-aware IDE
- source shown at a crash or breakpoint

Two things need deciding.

**Many-to-one mapping.** One C statement can come from several siox lines —
an inlined function body, an unrolled generate loop, a substituted parameter.
`#line` is a single position, so the rule should be to attribute to the
*innermost* source span, which is what a reader wants when stopped there.
Inlining means a stepping session may appear to jump between files; that is
normal for inlined code and preferable to no mapping.

**Optimization.** Stepping degrades under `-O`. This suggests a debug build
mode — `-g -O0` — selected explicitly, with the optimized path unchanged, since
simulation throughput matters for long runs.

## Tier C — values by siox name

Two different problems.

**Testbench locals** are already C locals with injective mangled names, so
DWARF from Tier B gives them for free once the names are recoverable.

**Hardware signals** are not variables. They live in a flat array indexed by
`SignalId`, so DWARF has nothing natural to describe. Rather than synthesize
DWARF for them, the cheaper route is a shipped gdb/lldb helper script offering

```
siox print FifoTest.f.used
```

which resolves the path to an index and reads the array. The name-to-index
table already exists in the binary — the VCD/FST writer builds one — so the
helper can read the same table instead of duplicating it.

## Non-goals

- **Interactive control** — pause, poke a signal, resume. That is the cocotb
  path (drive the compiled design over VPI/GPI), which is a much larger piece
  and already tracked separately.
- **Vivado and other vendor tools.** Their *waveform viewers* read the VCD and
  FST output today. Importing a siox design for synthesis is the elaborated RTL
  artifact, which is Phase 3.

## Suggested order

Tier A first: it is self-contained, needs no toolchain flags, and fixes the
complaint that motivates all of this — a failure that does not say where it
happened. Tier B is a small emitter change with a large capability gain. Tier C
is worth doing only after B, and its hardware half is a helper script rather
than a compiler feature.
