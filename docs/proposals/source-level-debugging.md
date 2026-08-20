# Source-level debugging

Status: **implemented**. All three tiers are in the compiler. One item in the
original plan was re-scoped after checking what it actually described, and one
refinement is deliberately left; both are recorded below rather than quietly
dropped.

A `#[test]` build is a native executable compiled through Clang, so a debugger
always attached to it. What was missing was the mapping back to `.siox`: a
failure said *what* went wrong but not *where*, and a debugger saw generated C
rather than source. This record describes closing that gap in three tiers, the
first of which needs no debugger at all.

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

**Runtime failures — implemented.** Assertions, range violations and file
reads all render a location and a source snippet beside the message:

```
test fail::FailTest ... FAILED
    the counter should be 99 here
  --> fail.siox:18:5
   |
18 |     assert!(y == 99, "the counter should be 99 here");
   |     ^
```

An assertion points at its own statement, a range violation at the assignment
that left the domain, and a failing `read<T>` at the `let` that asked for the
file. The location is per-failure and cleared at the start of each test, so a
passing run reports none.

The range case is the one that needed machinery. The check is a post-settle
test on the signal's value, so nothing about the value says which of the
statements writing that signal produced it. `Driver` and `NextUpdate` therefore
carry the assignment they were lowered from, `Design::range_sites` numbers the
distinct spans that can be blamed, and the engine latches that index next to
the error code and the offending value — all three under one predicate, so a
later violation cannot repoint an already-reported failure at its own line. The
harness renders the table at compile time and prefers it over the declaration:

```
test m::T ... FAILED
    `T.e.level` left its range 0..100 (it was 104)
  --> ramp.siox:8:9
   |
 8 |         level = step + 30;
   |         ^
```

Anything reaching a ranged signal through no assignment in the design latches
site 0 and keeps the declaration. In practice that is a value written from
outside — the testbench pushing a computed value into a port — which is why the
fallback survives:

```
test m::T ... FAILED
    `T.e.a` left its range 0..10 (it was 12)
  --> extwrite.siox:2:12
   |
 2 | entity E { a: integer<0..10> in, y: integer<0..10> out }
   |            ^
```

Drivers the lowering synthesizes (a port connection, a metavalue companion)
also carry no span, since the passes that create them run after all body
lowering has returned. Those rarely surface: a connected signal is downstream
of the one the entity drives itself, which is checked first and does have a
line.

A generic body lowers once per instance, so one statement produces a driver for
each; the table folds them to one site, because the line is a source fact and
the *signal path* is what says which instance went wrong. Both engines number
the table by calling the same function, so the index the hardware latches and
the string the harness prints cannot drift apart.

Anchoring the report also surfaced a defect it did not cause. The check ran once
per driver, including drivers a later unconditional one replaces, so a value the
signal never held could fail a test — `t = a + 5; t = 2;` reported `t` leaving
its domain while `t` held 2. That was survivable while the message pointed at
the declaration; pointing at the line `W-P014` had just called dead made it
plainly wrong. Both engines now skip writes a later unconditional write to the
same target subsumes, on the combinational path and within a clocked block.

## Goals

1. Every runtime failure names a source location.
2. A debugger maps machine code to `.siox` lines.
3. Values are inspectable by their siox name.

## Tier A — failures carry their source location

The highest value per unit of work, and it needs no debugger, no DWARF, and no
change to how anything is compiled. It also pays off in CI logs, where no
debugger is present.

Every runtime failure renders the way a compile diagnostic does — the message,
the location, then the line with a caret under the column:

```
test fail::FailTest ... FAILED
    the counter should be 99 here
  --> fail.siox:18:5
   |
18 |     assert!(y == 99, "the counter should be 99 here");
   |     ^
```

Cases:

- **Assertions — done.** The `assert!` span reaches the generated C and prints
  on failure.
- **Range violations — done, at the declaration.** The signal's
  `declaration_span` is used, so the message names the signal, its domain, the
  offending value, and where the domain was declared. Pointing at the
  *assignment* instead would be better, but the check is a post-settle test on
  the signal's value and no single driver is known at that point; it would need
  a per-driver site id latched beside the error code. Left as a refinement.
- **Runtime file I/O — done.** A failing `read<T>` names the `let` that asked
  for the file.
- **Oscillation — already covered, re-scoped.** The plan said a runtime
  oscillation failure should name its drivers. There is no such failure: a
  non-converging combinational loop is diagnosed at *compile* time as `W-P010`,
  which already carries a source location, and the delta cap then bounds the
  loop silently. Nothing to add at runtime. Worth noting separately that such a
  design still *passes* its test with a meaningless value, because W-P010 is a
  warning; whether it should be an error is a language question, not a
  debugging one.
- **Caret rendering — done, in the shared renderer.** The compiler's own
  diagnostics did not draw carets either, so adding them to runtime failures
  alone would have made the two diverge. `SourceMap::snippet` renders the row
  now, and both `Compilation::render_diagnostics` and the native emitter use
  it. The runtime snippet is embedded at compile time, so the executable never
  reads the source and stays right even if the tree moves on.

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

## Tier C — values by siox name — implemented

**Testbench locals** come free with Tier B: they are C locals, and DWARF names
them.

**Hardware signals** are not variables — they live behind the `sx_read`
accessor, indexed by `SignalId` — so DWARF has nothing natural to describe. A
debug build therefore carries `sx_signal_names`, every signal's hierarchical
path in `SignalId` order, and `scripts/siox-gdb.py` turns that into commands:

```
(gdb) source scripts/siox-gdb.py
(gdb) siox print f.used
FifoTest.f.used = 3
(gdb) siox list f.mem
FifoTest.f.mem[0] = 11
FifoTest.f.mem[1] = 22
FifoTest.f.mem[2] = 33
FifoTest.f.mem[3] = 0
```

An exact path wins; otherwise a unique trailing match resolves, so the root
need not be typed. The value comes from calling `sx_read` in the running
program rather than from decoding memory, so it needs no knowledge of the
storage layout. The table is debug-only, and the commands say so against an
ordinary build instead of printing something misleading.

## Non-goals

- **Interactive control** — pause, poke a signal, resume. That is the cocotb
  path (drive the compiled design over VPI/GPI), which is a much larger piece
  and already tracked separately.
- **Vivado and other vendor tools.** Their *waveform viewers* read the VCD and
  FST output today. Importing a siox design for synthesis is the elaborated RTL
  artifact, which is Phase 3.

## What remains

Nothing in this proposal. All three tiers are implemented, including the range
anchor that was once deferred here as too wide a change to the core IR structs
to make on its own.
