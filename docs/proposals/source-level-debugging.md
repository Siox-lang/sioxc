# Source-level debugging

Status: **partly implemented**. Waveform output, Tier A's two main failure
classes, and all of Tier B are in the compiler. What remains is listed under
each tier.

A `#[test]` build is a native executable compiled through Clang, so a debugger
already attaches to it. What is missing is the mapping back to `.siox`: a
failure says *what* went wrong but not *where*, and a debugger sees generated C
rather than source. This proposes closing that gap in three tiers, the first of
which needs no debugger at all.

## Current state

Verified against a small FIFO and counter.

**Waveforms — implemented.** `-o <path>` writes a waveform, the format chosen
by the path's extension, and the dump includes internal state, not only ports:

```
$scope module FifoTest $end
 $scope module f $end
  $var wire 8 v8  mem[0] $end
  $var wire 4 v12 head   $end
  $var wire 4 v14 used   $end
```

This is the standard HDL debugging loop and it is available now through
GTKWave, Surfer, or a vendor waveform viewer.

**Debugger attach — implemented.** Before this work, `gdb ./test.bin` broke and
backtraced but the binary carried no DWARF at all, so a stop showed
`sx_settle ()` and nothing about the source. With `-g` it now names `.siox`
files and lines; see Tier B.

**Runtime failures — implemented for assertions and range violations.** Both
now render a location beside the message:

```
test fail::FailTest ... FAILED
    the counter should be 99 here
  --> fail.siox:18:5
```

An assertion points at its own statement; a range violation points at the
ranged signal's declaration. The location is per-failure and cleared at the
start of each test, so a passing run reports none.

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

Cases:

- **Assertions — done.** The `assert!` span reaches the generated C and prints
  on failure.
- **Range violations — done, at the declaration.** The signal's
  `declaration_span` is used. Pointing instead at the *assignment* that wrote
  the out-of-range value would be better still, and needs per-assignment spans
  carried into the emitted check.
- **Oscillation / delta cap — not done.** Should name the drivers that kept
  changing.
- **Runtime file I/O — not done.** Already reports a source-relative path;
  its rendering is not yet aligned with the above.
- **Caret rendering — not done.** The location is `file:line:col`; showing the
  source line with a caret underneath needs the line text embedded at emit
  time.

The spans came from work already in place: `Signal` carries the declaration
span of its owning port or `let`, and IR diagnostics are anchored. One shared
`span_location` helper renders them, so every failure path names its source the
same way.

## Tier B — DWARF through `#line` — implemented

`sioxc --test -g` attributes every testbench statement to its `.siox` line with
a `#line` directive and compiles `-g -O0`. Clang produces DWARF naming the
`.siox` file, so with no debugger-specific work:

```
$ gdb -ex "break fifo.siox:92" -ex run ./fifo.bin
Breakpoint 1, test_r1 () at fifo.siox:92
92          assert!(level == 3, "three entries are held");
```

`list` shows surrounding siox source, `next` steps siox statements, and a
backtrace names siox files and lines. The default build is unchanged: no debug
sections, still `-O2`.

Two things were decided.

**Many-to-one mapping.** One C statement can come from several siox lines —
an inlined function body, an unrolled generate loop, a substituted parameter.
`#line` is a single position, so the rule should be to attribute to the
*innermost* source span, which is what a reader wants when stopped there.
Inlining means a stepping session may appear to jump between files; that is
normal for inlined code and preferable to no mapping.

**Optimization.** Stepping degrades under `-O`, so a debug build is `-O0`,
selected explicitly by `-g`/`--debug` and left off by default because
simulation throughput matters for long runs. The build also passes
`-grecord-command-line`, which puts the flags in DWARF so a binary can be asked
how it was built rather than taken on trust.

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

Tier A and Tier B are in. What remains, in the order worth doing it:

1. Tier A's remaining failure classes — oscillation and file I/O — plus caret
   rendering, and moving the range anchor from the declaration to the
   assignment.
2. Tier C, whose hardware half is a helper script rather than a compiler
   feature.
