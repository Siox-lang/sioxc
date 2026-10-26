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

The [DWARF acceptance checklist](dwarf-audit.md) maps the source-debugging
contract to permanent checks and records the latest verification status.

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
When an operand has a known width but no recorded source range (for example a
contextual operator literal), its companion is a `bit positions` record with a
`bits` array. Read `frame.read_var('rhs$logic')['bits'][0]` for the physical
least-significant bit. These positions are not inferred source index labels.
Expression-call formals and copied locals also expose `$logic` in their owning
frame. For example, GDB Python can read `gdb.selected_frame().read_var('value$logic')`.
These snapshots become available only when ordinary emission has produced both
planes, or proven the value metadata-free. Inspection never evaluates a missing
plane or repeats a source call. Unconsumed planes are optimized out, not shown
as known binary values; source branches may leave a companion unavailable even
when its numeric view exists. Partial hardware aggregate companions preserve
available scalar fields while pruned packed members remain optimized out.
Process frames and function breakpoints are instance-qualified, for example
`break T::stimulus`.

Partially evaluated aggregate-return calls appear as source inline frames.
Their `$return` snapshot becomes readable after the retained field has been
evaluated; instruction stepping can inspect it before leaving the frame.
Read it through GDB Python with `gdb.selected_frame().read_var('$return')`.
Discarded fields remain optimized out. `finish` returns to the caller without
inventing a whole aggregate value from native leaf registers; inspect the
caller's bound variable there instead. This follows GDB's documented
[inline-return limitation](https://sourceware.org/gdb/current/onlinedocs/gdb.html/Inline-Functions.html).
Complete one-field wrappers still use their real native aggregate return ABI.

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
CFG-expanded packed formals expose `value$logic` (or `self$logic` for a receiver)
using the same recursive source-enum layout. These read the captured place's
current numeric/companion state before source stops, including indexed aliases
and parent frames across suspension and mutation. They are not entry-time
simulation copies. References are cleared before refresh and remain unavailable
if either required plane cannot be read. The suspended-call regression checks
all nine logic states, descending negative-label indexed aliases, 81-bit fields,
aggregate/receiver views and known binary literals in both packing modes, with
native assertions, DWARF verification and ordinary/debug VCD parity.
Literal string arguments use that same `length`/`data`/`byte_length`/`utf8`
view, backed by immutable character/byte arrays rather than runtime handles.
Empty literals remain visible formal arguments: both lengths are zero and
both content pointers are null, even though the language value has no storage.

Expression-shaped calls in procedural processes retain distinct source
frames for repeated, same-line and nested calls. Shared Process functions retain source metadata directly on their existing
native body. Inline-only calls compile once per consumer format as a native
function: conditional consumers reuse that body instead of recursively
duplicating it. These are
direct LLVM functions over the same Process IR, not generated C or a separate
frontend pipeline. Their scalar/aggregate parameters and
local aliases use read-only debug views populated from values the program
already evaluates, not by reevaluating expressions. Debug-only return views
keep return-line breakpoints observable when LLVM combines nested arithmetic
even at `-O0`; ordinary builds introduce no simulation storage/copies for these
boundaries. Unselected expression calls must not acquire runtime source stops.
Pure function-body `if`/`match` branches retain lexical local scopes, including
nested shadowing. Return breakpoints follow the selected return expression,
even when both arms return the same existing argument. Debug-only control-flow
boundaries finish a local's inspection view before a later source statement can
stop; LLVM instruction scheduling must not expose an uninitialized snapshot.
Metadata-only arguments do not enter the native call ABI: only the callee's
executable source operands may be evaluated, even if an unused binding remains
retained elsewhere in the IR.
Native argument formats come from the common emitter's actual width, signedness
and layout requests, not from debugger inspection types. A shared argument may
need more than one native format; evaluating it at its minimum width before
widening would change signed/contextual arithmetic. Entry inspection reuses
those native inputs for formals and same-value aliases without reevaluation.
Expression string formals and aliases use the same borrowed or immutable
content views as CFG parameters. Returning a runtime string preserves its
handle; downstream length, indexing and literal comparisons retain runtime
semantics. CFG-hoisted host operations are labelled with the caller's call
site, not the callee's return line, so return breakpoints stop in its frame.

Hardware expression calls use this same native-frame emitter. Source
bindings retain declaration-owned inspection layouts separately from executable
operand formats, so negative integer locals, wide signed/unsigned formals and
real values have faithful debugger types without introducing simulation casts.
Each source hardware process keeps its own frame inside bounded shared helpers;
generated scheduler instructions use line-zero native locations rather than a
borrowed source statement. Clocked calls retain their owning event process.
Hardware conditional returns carry selected-arm source spans and lexical scopes.
Field-backed aggregate parameters and local aliases use LLVM fragments of
already-evaluated leaf values. Nested structs, ascending arrays with negative
labels, descending arrays and non-byte-aligned wide signed/unsigned members
retain their declared shape. Inspection storage rounds leaves to whole bytes,
not ABI words: an 81-bit member occupies 11 bytes. Pruned members are unavailable
(`gdb.Value.is_optimized_out`), even when neighboring members can be inspected;
metadata must not evaluate an unused foreign-call field to fill a debug view.
Fully retained pure hardware aggregate returns share one native
call and exact-layout SSA result across selected field consumers. Tests require one
source stop when both fields, only the first, or only the second are consumed,
and none when neither is selected. Whole two-integer returns are also checked
with actual GDB `finish`, rather than inferred from parameter/local inspection.
Inactive effectful fields retain the ordinary demand behavior; debugger
grouping must not evaluate them to construct a complete packed return.
Nested if/match return branches also require one selected stop per input,
exact return/caller lines and the correct shadowed local. A branch-private
local must have no visible symbol on the fallback path. Independent field
conditions on one source line and aliased aggregate locals retain native
results and one source frame. GDB `finish` checks both returned integer fields
for every branch/input in ordinary, nested/match, independent and alias cases.
Complete arrays containing pure nested arithmetic calls are covered separately
with all fields consumed in opposite orders on selected branches. Grouping is
allowed only after a joint-demand proof; a unit check rejects an independent
field use even when another consumer demands the whole value. It also guards
dynamic whole-value selection, invalid checked indices, and signed/arithmetic
selectors that cannot safely be folded for this proof.

Runtime-owned `string`/`Char[]` values expose a read-only view under their source
name: `text.length` counts Unicode code points, `text.data[i]` reads one code
point, `text.byte_length` counts UTF-8 bytes and `text.utf8[i]` reads a byte.
For example, `print *text.data@text.length` displays the entire character array.
Use the explicit byte length for embedded NULs; normal C-string printing stops
at the first NUL. Content pointers borrow runtime storage until test cleanup;
do not mutate either the view or its pointed-to content. Uninitialized/invalid
handles and views from inactive tests are empty, without raising runtime errors.
Snapshots refresh only at Siox source boundaries, as described above.
Literal/empty string return calls also retain source stops when a length
consumer folds the value, including nested argument calls. Tests require the
actual caller/assignment line, constant parameter/local views and correct
argument order. A shared return stops once for both/first-only/second-only
consumers and not at all when neither is selected. Direct folded lengths in
untaken branches must not create a stop. Formatted print/warning operands use
the same path; successful assertions and suppressed warnings do not execute
their formatted string calls. Empty zero-storage specializations return void;
GDB `finish` returns to the caller without adding a value to its history.
This is distinct from a runtime string handle whose content happens to be empty.

Wide source-call views are checked at 81/127/128/129/256 bits for signed/unsigned
parameters and copied locals, including inferred generic declarations. Their
sizes remain their declared byte-rounded widths when a caller narrows to
8 bits or widens to 193 bits, and a callee returning just the low byte still
exposes the original literal
parameter. These literal cases do not justify padding missing nonconstant bits.
Ordinary and debug executables verify the returned values and waveform parity;
the low-byte return also passes native GDB `finish`.

The narrow-consumer regression uses changing, nonconstant 81-bit hardware
arguments with high bits set. A callee consuming only eight bits leaves its
full-width parameter/local unavailable rather than displaying a truncated value
as the complete source value. A second callee consumes both eight and 81 bits;
entry inspection uses its already-requested complete format, even when the
narrow consumer comes first. Both inline hints check actual native results,
ordinary/debug stdout and VCD parity, source binding sizes and DWARF validity.
Inspection requests no new executable operand formats.

On AMD64, GDB 18.1 cannot reliably decode arbitrary-width basic integer native
returns: it can report a false zero at 81 bits and hit an internal assertion at
129 bits, even with return printing disabled. Wide source calls now retain
their true signature on an inline frame over the existing native body.
Inspect the read-only `$return` snapshot after completion, then use `finish`;
it leaves that source frame without adding a fabricated native history value.
The snapshot remains unavailable when only a narrowed result was emitted and
is cleared at every entry. A declared byte result with a private 193-bit
consumer payload also uses this path; an ordinary byte return keeps native
`finish` decoding. No executable ABI or evaluation demand is changed.
This does not fix GDB's native history decoder or prove every return ABI.

Remaining inspection work includes supported companion argument/return-plane
and normalized conversion/signature transport audits. The focused regressions
below cover checked/effectful grouping, independent nested fragments, joint
partial returns and mixed wide-array/real fragments; they are not a general
ABI proof. Independent demand must retain independent frames, not be forced
into one whole-return boundary. Wide basic scalars use the truthful inline
snapshot path described above rather than GDB's unsupported native decoder.
Signed integer-derived Resolve and mixed integer/real struct-to-struct From
now have actual argument/return checks under both inline hints. Other supported
conversion specializations and packed companion transports remain under audit;
generic From constructor syntax rejected by the ordinary frontend is a separate
language boundary, not evidence of a DWARF regression.
The tested signatures do not prove every specialization or
every argument's availability at entry.
Runtime-sized native values currently support Char[]
handles only; general dynamic arrays are a separate host-service extension,
not an already-supported representation missing debugger metadata. Debug mode
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
A third regression checks procedural expression calls, scalar/aggregate bindings, nested
return-line breakpoints and selected versus unselected calls, with ordinary/debug
native execution and byte-identical VCDs in both packing modes. It also checks
direct packed-element locals and function arguments: a logic element keeps its
source discriminant instead of being read as a physical one-bit array slice.
Expression string coverage includes runtime/literal/empty formals and aliases,
borrowed-handle returns and caller locations, Unicode/embedded-NUL contents,
length/index/equality semantics, plus aggregate integer/real/wide field values.
The backend's inline-call regression checks that differently selected
consumers emit one function body per width format, without merging formats.
Native symbol counts also require repeated shared branch/alias calls to use
one body; GDB checks their selected return lines and lexical shadowing. Narrow
integer actuals retain 64-bit declared formal/local inspection types without
changing their executable consumer formats.
A fourth native/GDB regression checks nested shadowing, alias returns (including
identical operands in both branches), aggregate returns and function-body match
arms. It requires exact selected-return/caller lines with no extra untaken-arm
stops, verifies DWARF and compares ordinary/debug VCDs in both packing modes.
It counts actual callee breakpoints for a scalar local reused under distinct
predicates: both consumers select it, only the first selects it, or only the
second selects it. The call must execute once in each case; an outer untaken
call must not execute. All four inline/shared caller/callee combinations run in
both packing modes, and breakpoint callback failures are collected explicitly.
The cache regression also rejects branch-only readiness flags, keeps separate
width keys, and checks state invalidation rather than restoring stale results.
A fifth regression exercises nested scalar hardware calls, shared helpers and
multiple instances, selected returns with shadowed signed locals, 81-bit
signed/unsigned and real bindings, declared return types, and clocked calls.
Its declaration-owned formal checks cover integer/real suffixes, integer
conversion inputs, signed constrained/generic parameters and a custom packed
operator's `Self` rhs, checking both types and actual values. Operator formals
use contextual widths, not the minimum width of a literal's executable node.
GDB can canonicalize `unsigned[N]` to `unsigned int [N]` under the current C++
debug language; the regression also checks the integer type code and byte size
so a similarly spelled array cannot pass as the packed scalar.
The same fixture checks a custom enum Resolve's receiver, rhs and return type
for all input pairs, a one-field struct returned by `From<integer>`, and an
8-bit to 7-bit fixed-family conversion. Actual GDB `finish` values must match
the captured inputs. Function-name breakpoints inspect live packed operator
and fixed-family arguments after reference/native-input initialization;
inspection setup itself must not be advertised as a source stop. Ordinary and
debug native assertions and VCDs still agree in both packing modes.
The mixed-return fixture also checks contextual literal operator companions
at function entry: the `bits` array has the contextual width and decodes the
actual value using source enum symbols. Constant-inspection units cover full
64-bit unsigned values, multiword literals and negation without host truncation.
Its aggregate parameter/local probe includes one-field wrappers, nested structs,
sub-byte and signed wide leaves, arrays in both directions and negative labels. It checks field
values, inspection sizes and unavailable pruned members; IR/native unit checks
also prove that debug field metadata cannot retain or execute foreign effects.
The hardware aggregate-return probe counts actual source-call stops across
both/first-only/second-only/untaken consumers and checks both integer fields
after GDB `finish` for positive, negative and zero inputs. An aggregate field
containing `abort()` remains runtime-untaken in both ordinary and debug builds.
It checks each observed binding against its captured input, including transient
initial-delta defaults, and requires the settled values and both event inputs.
GDB callback failures are collected explicitly; a successful GDB exit alone is
not a pass. The native fixtures also cover packed wide-state access alignment:
loads/stores must not assume integer ABI alignment for packed members.
A sixth regression checks actual GDB `finish` values for real-returning
identity functions, rather than merely reading their formal arguments. Both
inline and shared bodies return positive and negative values and preserve the
sign of negative zero in both packing modes, with DWARF verification and
ordinary/debug native result and waveform parity. These native boundaries
return double; bitcasts retain the internal integer-bit value contract.
A seventh regression verifies wide call views at 81/127/128/129/256 bits with
both inline hint settings and both packing modes, including large positive/negative values,
zero, narrow/wider consumers and signed/unsigned generic specializations. It
checks source frames, complete declared argument/local values, byte sizes,
complete/unavailable return snapshots, repeated-entry reset, source-frame
`finish`, native results and VCD parity. These packed calls
do not prove shared Process-body eligibility. Automatic wide `finish` decoding
is deliberately not claimed, as explained above.
An eighth regression checks expression `$logic` snapshots for all nine std
states at 9 and 81 bits, known binary values, recursive struct fields, descending
arrays with negative labels and selected/untaken return branches. It also checks
that an unconsumed plane and pruned hardware members stay optimized out, with
native assertions, DWARF verification and ordinary/debug VCD parity in both
packing modes. The existing wide-return regression additionally guards native
formal availability before a narrow callee's source return stop.
A ninth regression checks the actual native GDB `finish` return value for
complete small SysV real/integer aggregates in both field orders, a nested
struct, a two-real array with distinct values and a one-real wrapper. It also
checks both 12-byte Char/real field orders, nested member alignment, a real/Char[2]
tail, 9-byte unsigned[8]/real and normalized unsigned[3]/real structs, and a
real/two-byte-field struct whose integer fields must share one register slot.
Sub-byte coverage includes unsigned[3]/signed[3] pairs, real tails, nested
structs and three-element unsigned[3] arrays containing nested arithmetic calls.
The array consumer visits elements in opposite orders on its selected branches.
It runs
with both inline hint settings and both packing modes, comparing ordinary/debug
native results and VCD bytes and verifying DWARF. Every hardware invocation is checked against
its captured formals, including intermediate delta-cycle inputs; all settled
positive, negative and signed-zero cases must also occur. These checks cover
complete standard-sized byte-normalized register leaves and naturally unaligned
memory-class returns, not general shared Process-body eligibility, odd-sized
basic integer layouts or partial returns.
A canonical-IR unit test separately checks the same boundary in actual shared
Process functions: mixed/reversed/all-real native signatures, canonical i128
call results, module verification and no additional source-call wrapper.
The unit also guards mixed i8/i16/i32/i48 register slots, exact i72/i80/i96/i112
canonical results, subword-slot coalescing, small unaligned void/sret signatures
and a normalized 9-byte/i67 memory carrier. Executable callers retain their
canonical payloads instead of consuming normalized inspection bytes.
Sub-byte units additionally check i16 and mixed double/i16 register signatures
with exact canonical i6/i70 caller results and LLVM module verification.
A tenth regression checks actual GDB `finish` for memory-class SysV source
returns: a 24-byte integer/real struct, a normalized 19-byte unsigned[81]/real
struct, and a 43-byte nested signed[81]/real struct with a descending real array.
The same fixture also checks a generic identity over a 31-byte struct with a
3-bit tag, real and descending signed[81] array. Concrete formal/result types,
all memory-return fields, changing inputs and signed zero are checked directly
in GDB. Procedural generic real identities check actual native history values
for positive/negative inputs and negative zero. Both hints and packing modes
run, with native assertions, ordinary/debug VCD parity and DWARF verification;
these specializations require no separate executable-ABI or signature fix.
Every invocation's returned fields are checked against its captured formals,
including intermediate delta inputs, and settled positive/negative/signed-zero
cases are mandatory. Negative signed data uses the full 81-bit literal to avoid
the separately tracked integer-widening defect. The test uses both inline hints
and packing modes, runs ordinary/debug native assertions, compares VCD bytes
and verifies DWARF. The shared-body unit additionally checks void/sret function
and call signatures, canonical i192/i145 values and entry-owned return storage.
Only the private canonical payload is used by executable callers; normalized
source bytes are debugger-facing. These checks do not prove partial-demand or
arbitrary-wide basic-integer return decoding.
An eleventh regression checks actual GDB `finish` for runtime and nonempty-literal
`Char[]` returns. It verifies all Unicode code points and UTF-8 bytes, including
an embedded NUL and one-character literals with i32 executable carriers,
reuses a returned runtime string after `await`, and runs two
test roots with distinct borrowed content. Exact source-call counts are checked;
empty literal specializations must remain void without a new debugger history
value. Both inline hints and packing modes run ordinary/debug native assertions,
VCD parity and DWARF verification. A boundary unit separately verifies the
32-byte source-view prefix, unchanged i32/i64/i193 executable payloads,
handle-only narrowing for metadata, caller decoding and LLVM module validity.
It guards explicitly declared `Char[1]` against acquiring a dynamic-string
return view. These checks do not prove string eligibility for shared Process
bodies or general dynamic-array support; borrowed content is inspected before
its owning test root is cleaned up.
A twelfth regression checks partial integer/integer and integer/real aggregate
returns. Both retained field positions and positive/negative inputs are tested
under all four caller/callee inline-hint combinations and both packing modes.
The discarded field calls `abort()`: ordinary/debug native assertions and
identical VCDs prove inspection does not force it. GDB checks the declared source
return type, initialized formals, inline frame identity, actual `$return`
fragments, unavailable discarded fields, caller bindings after `finish` and
exact source-call counts. `finish` must not manufacture a whole-result history
value. DWARF verification also runs. This covers faithful partial views, not
single-boundary grouping of independently demanded or effectful aggregate leaves.
A signature regression checks nominal `Number` Resolve inputs/results with both
signs, plus distinct `Source` and `Target` struct types and mixed integer/real
From values at function entry and after actual GDB `finish`. The packed-logic
fixture also checks a nested return: its numeric-only callee has unavailable
companions, while the caller sees all source states once it consumes metadata.
Source call counts and actual numeric/enum finish values are checked.

An odd-sized aggregate regression covers 17/33/65/81-bit unsigned fields with
an 8-bit sibling in procedural and hardware calls under both inline hints.
[GDB's AMD64 classifier](https://gnu.googlesource.com/binutils-gdb/+/refs/heads/master/gdb/amd64-tdep.c)
cannot classify these small structs' basic integer fields; `finish` on the old
native source boundary failed with an unknown-alignment error. The source call
now remains an inline frame over the same native body, retaining the real source
type. GDB Python `frame.read_var('$return')` exposes the already-computed complete
snapshot before leaving that frame. Tests reject stale availability at entry,
check snapshot values and subsequent caller bindings, and require `finish` not
to fabricate a native history value. Ordinary/debug native assertions, VCD
parity and DWARF verification still apply. This is not a fix for GDB's native
arbitrary-width integer classifier; large aggregate memory returns and basic
wide scalar inspection retain their separate tests and limitations.

The 81/127/128/129/256-bit return regression inspects the source-enum
`$return$logic` companion after its actual mirror stores, alongside the numeric
snapshot. Signed, unsigned and generic results cover negative values, zero,
large positives and repeated entries under both hints and packing modes.
The low-byte return retains its declared eight-bit companion even when the
private executable payload is wider. A narrowed consumer cannot fabricate a
complete companion, and a fresh source entry cannot reveal a previous call.
Instruction-step budgets scale with the existing per-bit enum mirror writes;
numeric availability does not imply the companion's stores have run yet.
The missing-plane regression returns an 81-bit hardware input containing X.
Native assertions and ordinary/debug VCD parity prove that simulation retains
the real X state. GDB can read the complete numeric snapshot but cannot read a
logic companion whose metadata was not consumed inside that source frame.
Both hints run; absence is intentional, not a request to evaluate another plane.

The ordered-aggregate regression checks checked/effectful return grouping under
both inline hints and packing modes. GDB observes complete read-only `$return`
snapshots on in-place forward/reverse foreign-call source frames, and actual
native `finish` values for checked array fields and total division (including
zero divisors). A `putchar` between field uses preserves separate partial
frames; their `$return` fragments expose
only already-evaluated fields. One-bit `and` leaves an untaken `abort()` field
unavailable. Exact calls and transient inputs are checked, not just settled final
values. Ordinary/debug stdout equality proves foreign-call order and counts;
native assertions, VCD equality and DWARF verification also run. A failing active
variant checks the first bounds diagnostic and native exit status against the
ordinary build. Changing inputs checks compatibility captures and direct
procedural execution without sharing a debug-only effect body. Untaken
aggregate-return bounds checks have an independent ordinary-lowering gap
recorded in the compiler TODO; this fixture
does not claim to fix it.

The reused-effect regression consumes the same foreign result at narrow and
wide formats across another foreign call, with changing input epochs. Ordinary
and debug stdout/VCD must match; a seeded random test also checks that host draws
are not repeated for new consumer formats. GDB inspects both cached aggregate
fragments, completed inline scalar snapshots and normalized pure-tail loop returns,
with exact stop counts under both hints and packing modes. An inline entry must
not expose an earlier invocation's return snapshot. Lazy arguments become
inspectable only as their actual values are consumed; the probe follows caller-
owned argument ranges as well as callee ranges. Normalized IR inspection and
instruction stepping confirm that the procedural pure-tail frame no longer
consumes its original `value` formal. It stays unavailable; its `saved` local
becomes visible after the existing captured result is read. Repeated loop
entries check those availability transitions and actual native finish values.
Other normalized signatures remain under audit; this does not imply every
source formal is available at every function-name stop.

The independently selected nested-partial regression exercises neither, left,
right and both consumers with changing inputs under both hints and packing
modes. Its third, unused return field calls `abort()` and must remain pruned.
Ordinary/debug stdout preserves the exact eight foreign evaluations, including
transient hardware inputs; numeric results and VCD also match. GDB inspects
each retained `$return` fragment and consumed formal, waits through lazy
caller-owned ranges, then observes the caller's own fragment store after
source finish. Entry references cannot expose an earlier call, and discarded
fields stay optimized out. The probe checks the first unused GDB history slot
directly after finish: captured finish text alone is not proof that no native
return was fabricated. This verifies partial frames, not eager whole-return
grouping for independently guarded fields.

The joint-partial regression consumes the first and last fields of a three-field
return whose middle field calls `abort()` and must stay pruned. It requires one
source frame per input epoch, a complete declared struct type with two readable
return fragments, and no fabricated native history slot on source finish.
Forward/reverse field use checks actual foreign-call order; pure/foreign nested
calls and selected/fallback return branches run under both hints and packing
modes, with native assertions, DWARF verification and stdout/VCD parity.
The carrier unit proves that two retained 64-bit fields occupy 128 private bits,
not the source struct's 192 bits, and rejects independent uses and overlapping
projections. The existing independently selected regression remains separate:
its guarded fields cannot justify eager joint grouping.

The mixed-partial regression returns a descending `signed[81][2..1]` array,
an unused middle `abort()` field and a real value through generic nested calls.
Its comparison consumers retain three independent source fragments. GDB checks
the full 38-byte declared `Mixed` type and each actual array/real fragment over
three input epochs, including negative zero; the abort field stays unavailable.
Both inline hints and packing modes run native assertions, ordinary/debug VCD
parity and DWARF verification. Metadata-only comparison dependencies do not
justify evaluating another numeric fragment or grouping these frames.

The numeric-family signature regression converts binary32 to binary16,
binary16 to signed fixed, and signed fixed to binary32. GDB verifies source
formal/return types, byte sizes and actual `finish` values separately for
procedural and hardware calls, with positive/negative input epochs under both
inline hints and packing modes. Qualified and local source type names are
matched as the same nominal family without changing compiler metadata.
Ordinary/debug native assertions and VCDs agree, and DWARF verification runs.
The regression caught an early shared-native entry stop caused by binary
inspection `select`s becoming control flow at `-O0`; the encoding-aware
branch-free mapping fixes that without adding evaluation demand. A unit checks
reversed binary codes, modular width wrapping and nonbinary metadata forwarding.
The fixture additionally builds binary16 and bfloat16 from the same binary32
input. Canonical IR must retain the explicit return ranges `5..-10` and
`8..-7`, with distinct used shared functions, plus binary32 `8..-23` and
sfixed8 `3..-4`. GDB must observe both actual 16-bit result encodings in both
caller contexts, not just a count of conversions. No extra executable width or
argument is introduced to repair metadata; omitted planes stay unavailable.

The restoring-root loop regression checks that synthesized pure selections
retain shared SSA instead of generating `debug.select.then` machine branches.
Its nested identity input is a caller-owned captured evaluation, not a fresh
callee effect. Ordinary/debug native assertions and VCDs match, source frames
remain present, and DWARF verification runs in both packing modes. The real
`float_sqrt_test` corpus compile additionally exercises the larger std loop;
the focused measured debug compiler child used 783612 KiB after the guard.
Existing selected-return and checked/effectful regressions protect the cases
that still require conditional source ranges. The custom-enum Resolve fixture
also checks byte-normalized sub-byte native returns: `finish` reads `High` as
one, not undefined high register bits.

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
