# Runtime calls with frames, and recursion

Status: **proposal**. Nothing here is implemented.

## Problem

Every call in a testbench is expanded at its call site. A function whose body
is one value becomes part of the caller's value graph (or a shared value
function, `ProcessFunction`, when it is large). A function with control flow
is inlined into the caller's CFG (`cfg_calls::inline_call`). Neither form has
a frame, so a call can never reach itself:

```siox
fn fact(n: integer) -> integer {
    if n <= 1 { return 1; }
    return n * fact(n - 1);
}
```

Today this compiles, and the test then fails when it runs with *direct Process
IR lowering is incomplete for process 0 block 1*. That message names neither
the function nor the reason. TODO.md (LLVM) asks for runtime calls with
"explicit frame, ownership, conversion and suspension semantics", where
unsupported forms "fail before argument/callee effects or staged writes become
observable".

## Decision

Add **frame functions**: a function body lowered once into its own CFG and
emitted as a native LLVM function with a frame, called at run time. Inlining
stays the default for everything else. Only functions that need a frame get
one: in the first version, the members of a recursive cycle in the call graph.

Hardware is unaffected. A hardware call is a value or an unrolled structure
with no run-time stack, and recursion there stays an error.

## What a frame function may do (first version)

| | allowed | why |
| --- | --- | --- |
| parameters | by value, packed or kernel numbers (`integer`, `real`, `unsigned[N]`, `Bit`, structs of these) | the caller passes words; nothing aliases the caller |
| writing a parameter | no | a parameter that writes back aliases a caller place (`std::mem::swap`); that needs place passing (later) |
| `Logic` / X-Z values | no | metavalue companion planes would have to be passed and returned (later) |
| `await`, `stop()`, `finish()` | no | suspension would need a heap frame the runtime resumes (later) |
| reading signals | yes | values are stable within one process step |
| writing signals, storage | no | staged writes from inside a frame need their own ordering rules (later) |
| `print!`, `assert!`, `warn!` | yes | runtime instructions; a failure unwinds the frames (below) |
| calls | yes | to other frame functions, and inlined calls as today |

A recursive cycle that breaks a rule is a **compile-time error** naming the
cycle and the rule. It replaces the run-time "incomplete" failure.

## Shape

**Call graph.** After source lowering has seen every function, compute the
strongly connected components of the testbench call graph. A function in a
component with a cycle (or a self-call) is a frame function. Inlining treats
a call to a frame function as opaque.

**IR.** `ProcessIr` gains `frames: Vec<ProcessFrame>`. Each frame has an id,
a name and span, parameter locals with layouts, locals, blocks, an entry
block, and a result layout. It is lowered by the same statement lowering that
builds process CFGs, with `Return { value }` ending the function. A call is a
new value kind, `ProcessValueKind::FrameCall { frame, arguments }`. CFG
normalization treats it as it treats a host call: evaluated once, in source
order, and captured before later operands that have effects.

**LLVM.** One internal function per frame:
`i8 sx.frame.N(ptr result, <arguments as iN>)`. Locals are allocas, and the
blocks are emitted by the existing block emitter in a frame mode:
`Return` stores the result and returns *completed*, and suspension cannot
occur. The `i8` is the process status: a failed `assert!` in a frame returns
*stopped*, and every caller returns it in turn up to the process entry, which
already reports failures this way. Nothing staged by the process is lost,
because a frame cannot stage writes.

**Runtime.** Native recursion uses the C stack, so depth needs a bound before
the stack runs out: `sx_runtime_frame_enter()` and `sx_runtime_frame_leave()`
count depth. Past a limit (default 100 000, as an `sx_` option), the test
fails with *recursion deeper than N in `fact`* rather than crashing. Tests
run on the main thread today; the limit must fit its default 8 MiB stack, or
the runtime moves test execution to a thread with a stack sized for it.

**Debugging.** A frame is a real function, so a DWARF build gets a real
`DW_TAG_subprogram` and a call stack in GDB. The inline-scope machinery is not
needed for frames.

## Delivery

1. **Diagnose.** Call graph and cycle detection. A recursive call becomes a
   compile-time error naming the cycle: an improvement on its own.
2. **Frame IR** for cycles that meet the rules above: lowering, validation,
   `FrameCall` normalization.
3. **LLVM frames** with status propagation and the depth guard.
4. **Tests:** `fact`, `fib`, mutual recursion (`even`/`odd`), recursion
   through a struct argument, an `assert!` failing three frames deep (the
   source location and stop status survive), the depth limit, and every
   rejected rule with its diagnostic. Corpus programs in siox-tests.
5. **Later, each its own change:** place parameters (write-back),
   metavalue planes, signal writes from frames, suspension in frames (a heap
   frame the scheduler resumes), and frames for large non-recursive functions
   (code size, alongside `#[inline]`).

## Acceptance

- Every program in delivery step 4 gives the same result in default and
  `bitpack` builds, and in ordinary and `--debug` builds.
- Corpus and full CI are unchanged for programs without recursion; no
  existing call changes form.
- `scripts/bench.py` shows no change on the default program set.

## Open questions

1. **Depth limit:** a fixed default with a test-binary option, or a
   per-function `attr`?
2. **Opt-in frames:** should `#[inline(never)]` on a non-recursive CFG
   function also give it a frame (smaller code), or keep that out of the first
   version?
3. **Rules first or features first:** is the first version above the right
   cut, or is write-back through parameters (`swap`-style helpers that
   recurse) needed from the start?
