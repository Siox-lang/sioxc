# Scoped blocks in implementations

Status: **proposal**. Nothing here is implemented.

## Problem

State that lasts across clock cycles is a signal declared at implementation
scope (language §3.11). A `let` inside a process is a temporary: a hardware
process has no `await`, so its body runs from the top on every activation and
the local's initializer runs every time. siox never infers a register from a
local, which removes VHDL's accidental-register and Verilog's
blocking/non-blocking traps.

The cost is locality. A register that only one process uses must still be
declared beside every other member of the implementation, and a large entity
with many processes ends up with one crowded namespace and no way to say which
state belongs to which logic. VHDL answers this with process variables, which
makes state implicit again. This proposal keeps state explicit and makes it
scoped instead.

## Proposal

A `{ … }` block at implementation scope groups members under their own scope.
The label is optional.

```siox
impl Cpu {
    counter: {
        let n: unsigned[8] = 0;          // a register, private to the block
        pub let wrapped: Bit = '0';      // visible to the whole implementation

        step: process {
            if clk.rising() {
                n = n + 1;
                wrapped = if n == 255 { '1' } else { '0' };
            }
        }

        count = n;                       // outer names are visible inside
    }

    overflow = counter.wrapped;          // a pub member, through the label
    // `n` is not in scope here; tools still see it as `counter.n`
}
```

### Where blocks may appear

At the root of an implementation, inside a generate `for`/`if`, and inside
another block. Inside a process or function `{ … }` remains ordinary control
flow and is unaffected.

### What a block may contain

What an implementation's root layer accepts for structure and behaviour:
`let` declarations (signals and entity instances), processes, concurrent
assignments, generate `for`/`if`, nested blocks and `attr` bindings. Type-level
members (`fn`, `const`, mode fields) stay at the implementation root, because
all split `impl` blocks of one entity share that member namespace (§3.1.1).

Entity instances inside a block are structural, so the placement rule widens:
an instance may be declared at the root layer, inside a generate `for`/`if`, or
inside a block (`E-P020` otherwise, unchanged).

### Scoping

Block scoping follows Rust. A name declared in a block is visible in that block
and the blocks nested in it, and nowhere else. Names from enclosing scopes are
visible inside. A name declared inside may not repeat a name visible from an
enclosing scope (`E-P002`): an assignment in a block must never silently
retarget an inner signal that happens to share an outer name.

Referring to a private block member from outside is an error:

- a bare name (`n`) is `E-P001`, with a help line naming the block that
  declares it and suggesting `pub let`;
- a path through the label (`counter.n`) is `E-P024`, the existing private
  member error.

### `pub let`

`pub` on a `let` in a block exports it to the enclosing scope. Visibility
follows the owning container (§3.1.1): a member's visibility cannot exceed its
owner, so a `pub let` is public to the implementation and never part of the
entity's interface. `instance.counter.wrapped` from another entity is
`E-P024`, exactly like any other implementation state; ports remain the only
structural interface.

- In a labelled block, a `pub` member is named through the label:
  `counter.wrapped`. The label is the namespace, as with generate labels
  (`tap.t`).
- In an unlabelled block, a `pub` member joins the enclosing scope directly
  (`wrapped`); a collision there is `E-P002`.
- A `pub` member is an ordinary signal outside the block: readable, and
  assignable under the usual driver rules (a second driver of an unresolved
  signal is `E-P014`). A `pub` instance exposes its ports
  (`counter.uart.tx`).
- `pub` exports one level. A `pub` member of a nested block is visible in its
  parent block, not beyond; reaching further needs a forwarding signal in the
  parent.
- `pub let` at the implementation root is rejected as redundant, like `pub` on
  a port: root state is already visible throughout the implementation and can
  never be more.

### Hierarchy and tools

A labelled block is a scope in the elaborated hierarchy, exactly like a
generate label: `counter.n` is the path in `--emit tree`, IR signal names,
VCD/FST scopes and the debugger, and the label shares the parent's member
namespace (`E-P002` on a duplicate). Inside a labelled generate loop the paths
compose: `stages[1].stage.r`.

An unlabelled block adds no hierarchy level. Its members are named in the
parent's namespace for tools, and a collision there, including two private
`n`s in sibling unlabelled blocks, is `E-P002`; a label avoids it. This is the
existing rule for unlabelled generate loops.

`attr` bindings follow scoping: inside the block, `attr keep for n = true;`
binds the block's `n`; from outside, only a `pub` member can be bound, through
its path (`attr keep for counter.wrapped = true;`).

### Semantics

None new. A block is purely structural: elaboration flattens it into the
implementation it sits in, with its members renamed by their paths. Initial
values, next-state semantics, driver rules, resolution, sensitivity inference
and process scheduling are unchanged, and the IR, LLVM backend and runtime see
nothing they do not see today. A process-local `let` stays a per-activation
temporary; blocks give state a place to live, never a way to be inferred.

## Prior art

- VHDL's `block` statement (`label: block … begin … end block;`) scopes local
  signal declarations to a group of concurrent statements; its label is
  mandatory.
- Verilog-2005 named `begin … end` generate blocks give local declarations and
  hierarchical names.
- Rust block expressions: lexical scoping, `pub` for export, labels optional.

## Implementation sketch

- `syntax`: an implementation member `Block { label: Option<Ident>, members,
  span }`; parse `{` and `label: {` at member position (unambiguous: no other
  member starts with `{`, and a label is otherwise followed by `process`,
  `for`, `if` or an assignment); pretty-printer and formatter support; `pub`
  accepted on `let` inside a block, rejected at the root.
- `resolve`: one scope per block, export of `pub` members by label path or
  directly, the shadowing check, and the `E-P001`/`E-P024` help lines.
- `types`: walk block members like root members.
- `elab`: a labelled block is a hierarchy scope (the generate-label path
  machinery, as a labelled `if true`); an unlabelled block is transparent, with
  the collision check.
- `ir`, `llvm`, `runtime`: no change.
- Documentation: language §3.11 and the generate-label rules; the paper's
  structure chapter.
- Corpus: labelled and unlabelled blocks, `pub` access through a label and
  directly, private-access errors, nesting, blocks inside a generate loop, an
  instance in a block, and the VCD scope names.

## Open questions

1. Shadowing. Proposed: an inner name may not repeat an outer one (`E-P002`).
   Rust allows shadowing; relaxing later is compatible, tightening is not.
2. Multi-level export. Proposed: `pub` exports one level. A `pub` on the block
   label (`pub inner: { … }`) could re-export a nested block's `pub` members
   instead of forwarding signals.
3. Unlabelled-block tool names. Proposed: `E-P002` on a collision, matching
   unlabelled generate loops. The alternative is synthesized names
   (`block_0.n`).
4. Scoped helpers. Whether a block may also hold `fn`/`const` visible only
   inside it.
