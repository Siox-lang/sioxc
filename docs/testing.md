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
    let dut: Counter = { .clk = clk, .rst = rst, .count = count };

    clock: process {
        clk = not clk after 5ns;     // free-running clock, 10 ns period
    }
    stimulus: process {
        await 10ns;                  // hold reset for one edge
        rst = '0';
        for i in 0..9 { await clk.rising(); }
        assert!(count == 10, "counter should reach 10");
    }
}
```

Testbench processes are concurrent, while statements inside one process run in
order. `await` advances simulation time (see
[simulation.md](simulation.md)), and a process-local `let` is a mutable local
with ordinary sequential assignment. The fixed native scheduler runs several
independently suspending foreground processes and background clocks. Each
process yields at `await` or a settling boundary; its own continuation resumes
when ready. These processes cooperate on one host thread, not OS threads.
Test processes receive stable process/block/local and
value IDs plus validated branch, match, loop, suspension, and termination CFGs
inside `Design::process_ir`; hardware processes and test stimulus execute from
those CFGs through the same LLVM/runtime path. There is no second test-only
program, generated source translation, or runner.
Method calls on the DUT or on struct-typed locals work in
stimulus, so a testbench can drive a design through a method result. Strings
retain their array semantics here: locals can be
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
than one ABI word preserve every word, while a narrower field still wraps at
its own boundary.
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
- `stop()` / `finish()` — end the run. They are ordinary functions, not
  macros: `stop()` halts the test, which passes so far, and `finish()` ends
  the simulation cleanly.

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
- **Debugging:** `sioxc --test -g` emits direct LLVM DWARF with optimization
  disabled. Use GDB source-line breakpoints, `next`, and source variable names;
  see the debugger checks and inspection conventions below. Ordinary runtime
  failures still carry Siox source locations without `-g`.
- **Failures name their source.** A failing `assert!`, a ranged signal leaving
  its domain, and a failing `read<T>` all print `--> file:line:col` beside the
  message, followed by the source line and a caret, so a CI log points at the
  line without a debugger. An assertion names its own statement; a range
  violation names the *assignment* that left the domain, not the declaration
  that set it, falling back to the declaration only for a signal written by a
  driver the compiler synthesized (a port connection).
- **Waveforms:** `-o trace.fst` writes compressed FST; `-o trace.vcd` writes
  portable text. The path's extension picks the format, so FST is the default
  without naming it. `--output=<path>` and `-o<path>` are equivalent, and
  passing `-o` twice with one of each extension writes both.
  Either option may precede or follow the test filter, and both may be requested
  together with different paths; using the same path is rejected. They share
  the same 1 fs scheduler samples and place multiple tests consecutively on one
  monotonic timeline.
  A filter matching no tests leaves waveform paths untouched. A selected test
  with no observable signals can still write an empty VCD, but requesting FST
  fails before creating/truncating that FST path: libfst requires at least one
  variable. Tests with signals may produce valid FST at time zero without an
  `await`.
- **A directory:** corpus orchestration belongs to the build/test tooling, not
  the compiler. `scripts/test-corpus.sh` compiles and runs each `.siox` file.
- **Native binary:** `sioxc --test <file> -o <bin>` builds a standalone test
  executable that exits 0 on pass.

A file with no `#[test]` entity reports zero tests rather than erroring.

## Native debugger smoke test

GDB can debug the native runtime, or step Siox source and inspect named state
when compiled with `--debug`. The reproducible check uses the existing FIFO and
128-bit corpus designs in both ordinary and debug builds:

```bash
bash scripts/guarded-run.sh bash scripts/test-gdb.sh /home/max/siox-tests
```

It verifies timed-wait and process-entry breakpoints, scheduler backtraces,
instruction stepping, signal reads through the waveform metadata and
`sx_read_word`, and byte-identical VCD output versus normal execution. The wide
case checks both 64-bit words of a 128-bit carry, source breakpoints/stepping,
and named wide structs/arrays. Ordinary builds still omit source metadata.
This requires GDB with Python support and permission to launch a traced child;
it adds no DAP or editor integration. Waveform tables are not a stable
source-debugger API.

```bash
sioxc --std std --test --debug /home/max/siox-tests/fifo_test.siox -o fifo-tests
gdb ./fifo-tests
# (gdb) break fifo_test.siox:26
# (gdb) run
# (gdb) print FifoTest::d::count
# (gdb) next
```

DWARF namespaces follow elaborated instance paths: `FifoTest::d::head` names
internal FIFO state, while `FifoTest::dout` names persistent test storage.
Process locals use their lexical names; shadowed locals regain their outer
binding after the inner block exits, including across `await`. Select the Siox
caller frame (`up`) if stopped inside an unannotated native runtime helper.
Debug artifacts advertise the C++ debugger expression grammar because Siox has
no native debugger language integration; code generation remains direct LLVM.

Integers, reals, std/user enum discriminants, multiword vectors, structs and
fixed arrays retain their source types. Packed aggregates that cannot be read
faithfully as native bitfields use debug-only byte-aligned inspection buffers,
refreshed at Siox source boundaries. Do not write these buffers to mutate the
simulation or expect them to update while stopped inside runtime helpers.
Ascending arrays preserve their lower bound (`words[-1]`); descending arrays
are labelled records rather than silently reversed C++ arrays. GDB Python can
inspect their labels with `gdb.parse_and_eval('T::rows')['[3]']`. Flattened
hardware leaves containing brackets need quoted symbol names, for example
`print 'FifoTest::d::mem[0]'`.

Packed vectors retain their numeric view and additionally expose a read-only
`$logic` snapshot (`print T::bus$logic` or `print local_bus$logic`). Its elements
are the source enum variants, not raw metadata nibbles: X/Z, weak values and
user enum symbols come from elaborated type metadata. For structs and arrays
containing packed vectors, the companion preserves the recursive shape and
ordinary scalar fields. Apply the same ascending-array/descending-record
indexing conventions described above. These views refresh at source boundaries
alongside the other inspection buffers; they do not change simulation storage.
Process frames and function breakpoints are instance-qualified, for example
`break T::stimulus`.

Procedures and value-returning functions expanded into CFGs retain source
inline frames and call sites. Use `up`/`down` to select the caller or callee;
equal-spelled locals remain distinct at repeated calls (even on one source
line), in nested calls and after `await`. Lexical shadowing stays within its
own call instance rather than merging scopes with identical definition spans.
`info args` shows formal parameters under their signature names (including
`self`). Captured place arguments retain their language aliasing: after a
callee writes through an argument, inspection reports its current value, not
an entry-time copy. Structs, indexed places, wide/real projections and runtime
strings use the same read-only inspection conventions as locals. Return types
are also retained in the source subroutine metadata.
Literal string arguments use that same `length`/`data`/`byte_length`/`utf8`
view, backed by immutable character/byte arrays rather than runtime handles.
Empty literals remain visible formal arguments: both lengths are zero and
both content pointers are null, even though the language value has no storage.

Runtime-owned `string`/`Char[]` values expose a read-only view under their source
name: `text.length` counts Unicode code points, `text.data[i]` reads one code
point, `text.byte_length` counts UTF-8 bytes and `text.utf8[i]` reads a byte.
For example, `print *text.data@text.length` displays the entire character array.
Use the explicit byte length for embedded NULs; normal C-string printing stops
at the first NUL. Content pointers borrow runtime storage until test cleanup;
do not mutate either the view or its pointed-to content. Uninitialized/invalid
handles and views from inactive tests are empty, without raising runtime errors.
Snapshots refresh only at Siox source boundaries, as described above.

Remaining inspection work includes additional runtime-sized array representations
and expression-only inline frames. Debug mode
is deliberately unoptimized; it is not a simulation performance mode. The
same option applies to native objects and `--emit llvm-ir`.

`tests/debug_info.rs` verifies metadata/object output, native execution,
source/function breakpoints and steps, persistent/shadowed locals, loop indices,
3-bit signed and 65/81/127/129-bit signed/unsigned storage/ports/locals,
all nine std logic states across waits, recursive companion views, nonstandard
enums, real/wide packed structs, labelled arrays, dynamic Unicode/UTF-8 strings
(including empty/embedded-NUL values, updates and cleanup between two tests),
and runtime string locals through inlined length/index/equality expressions in default and
`bitpack` builds. A second regression checks repeated/same-line/nested CFG call
frames, call-site lines, shadowed locals and suspension for procedures and
value-returning functions, plus formal arguments, mutable/indexed aliases,
struct receivers, wide/real fields and Unicode/embedded-NUL string arguments.
The argument checks cover both runtime strings and literals, including empty
literals; ordinary/debug executables must agree on character-count attributes.
To run the existing native corpus with DWARF enabled:

```bash
bash scripts/guarded-run.sh env SIOXC_DEBUG=1 bash scripts/test-corpus.sh /home/max/siox-tests
```

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
