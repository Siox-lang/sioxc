# VHDL-style labels

Status: **proposal**. Nothing here is implemented. It replaces the current
`process <name> { … }` spelling with a VHDL-style label, `<label>: process
{ … }`, and extends the same label to the structural `for` and `if`
constructs of an entity implementation, so that the architecture a design
elaborates into can be navigated by name.

## Decision

A **label** is an identifier and a colon written before a concurrent construct
at the top level of an entity implementation:

```siox
impl Pipeline {
    update: process {                    // was: process update { ... }
        if clk.rising() { q = d; }
    }

    stages: for k in 0..2 {              // three stages: ranges are inclusive
        let s: Stage = { .i = w[k], .o = w[k + 1] };
    }

    debug_tap: if DEBUG {
        let tap: Probe = { .i = w[3] };
    }
}
```

- Three constructs take a label: `process`, and the structural `for` and `if`
  that generate hardware outside any process.
- A label is optional everywhere. An unlabelled `process { … }` stays valid.
- `process update { … }` is removed. It becomes an error that names the
  replacement (`write \`update: process { … }\``), the same treatment `wait`
  received when `await` replaced it.
- A labelled `for` or `if` becomes a **scope** in the elaborated hierarchy.
  The instances and signals it creates are named through it (see
  [Semantics](#semantics)).

## Why this shape

**A process is labelled, not declared.** `fn name`, `entity Name` and
`struct Name` declare something you then refer to by name. A process is never
called or referenced; it runs. Its name is a tag on a statement that exists
anyway, and label-first syntax says exactly that. `process update` borrows the
declaration shape for something that isn't one.

**One spelling covers every concurrent construct.** `process update` has no
natural extension to the generate constructs: `for stages k in 0..2` and
`if debug_tap DEBUG` both read badly. `stages: for …` and `debug_tap: if …`
use the same form as `update: process`.

**It is what VHDL users already read.** `update: process` and
`stages: for i in 0 to 2 generate` are the VHDL forms, and VHDL tools (nvc,
GHDL, Vivado, Questa) name hierarchy through generate labels the same way. A
siox waveform then lines up with the mental model a VHDL engineer brings.

**Alternatives rejected.**

- Rust's `'label:` is unavailable: `'` is siox's attribute sigil (`sig'event`),
  and `'stages: for` would read as an attribute.
- `#[label = "stages"]` makes identity into metadata, and
  [compiler-directives.md](compiler-directives.md) reserves `#[…]` for
  directives that change compilation.
- A VHDL closing label (`end process update;`) has no place in brace syntax.

## Why generate scopes are needed now

Today a structural `for` or `if` leaves no trace in the architecture. For the
`Chain` below, `--emit tree` and the waveform both show a flat list:

```siox
impl Chain {
    for k in 0..2 { let s: Stage = { .i = w[k], .o = w[k + 1] }; }
    if 1 == 1 { let tap: Stage = { .i = w[3], .o = z }; }
}
```

```text
c: Chain
  s_0: Stage          <- from the loop, suffixed with the iteration
  s_1: Stage
  s_2: Stage
  tap: Stage          <- from the `if`, indistinguishable from a plain instance
```

Nothing records which instances a loop produced or which condition created
`tap`, and in a large design that is most of the structure. The `_k` suffix
also shares a namespace with ordinary names, and **a collision is not
diagnosed**. Adding a hand-written `let s_0: Stage = …;` beside that loop
compiles cleanly and produces two instances at the same path:

```text
IR:    signal T.c.s_0.i : 8        signal T.c.s_0.i : 8     (two signals, one name)
VCD:   $scope module s_0 $end      (one scope)
         $var wire 8 v7  i $end
         $var wire 8 v8  o $end
         $var wire 8 v13 i $end    (the second instance's ports, same names)
         $var wire 8 v14 o $end
```

Simulation values stay correct, since signals are distinct internally, but the
waveform folds two instances into one scope with duplicate variable names, and
a debugger path such as `sx_dbg_get("T.c.s_0.o")` is ambiguous. That is a bug
worth fixing on its own (a duplicate-name diagnostic) before this proposal
lands. Labelled generates remove the cause, because generated names live in
the generate's own scope and can no longer collide with the parent's.

## Semantics

**Namespace.** A label shares the entity implementation's member namespace
with `let` declarations, instances and functions, exactly as a process name
does today. A duplicate is `E-P002`.

**Processes.** A process label means what a process name means today. It is
the driver context's identity in the IR (`Design::process_labels`), and it
appears in the IR dump and in diagnostics such as the testbench's "merge
process `x` into the first stimulus process". It never affects scheduling.

**Labelled `for`.** The label names a scope with one child per iteration,
keyed by the loop value. Everything the body declares is named inside that
child:

```text
c: Chain
  stages: for
    [0]
      s: Stage
    [1]
      s: Stage
    [2]
      s: Stage
```

The instance paths become `c.stages[0].s`, `c.stages[1].s`, `c.stages[2].s`.
That spelling flows unchanged to `--emit tree`, VCD/FST scopes, IR signal
names, runtime diagnostics, and the `sx_dbg_get` debugger paths. A descending
or negative range keys its children by the actual loop values.

**Labelled `if`.** The label names one scope, present only when the condition
holds: `c.debug_tap.tap`. Whether an `else` branch shares the label's scope or
needs its own is listed under open questions.

**Unlabelled generates** keep today's flat naming, so existing designs are
unchanged. They still need the collision diagnostic described above.

**Access is unchanged.** A label is for naming and navigation. It does not
make an instance's internals reachable from outside; `c.stages[0].s.o` is a
path you see in the tree, the waveform and the debugger, not an expression a
testbench can read. Entity implementation state stays private.

**Attributes.** A label is the natural target for label-class attributes in
the declarative attribute design ([attribute-system.md](attribute-system.md)),
matching VHDL's `attribute keep of update : label is true`.

## Grammar

```text
impl-item   := label? ( "process" block
                      | "for" pattern "in" expr block
                      | "if" expr block ( "else" … )? )
             | …
label       := IDENT ":"
```

Nothing that can begin an impl item today starts with `IDENT ":"`.
Declarations begin with `let`, struct literals use `.field`, and paths use the
separate `::` token. One token of lookahead after the identifier therefore
decides it. The existing single-`:` lookaheads in the parser are elsewhere:
inside `<…>` generic binders, and the deprecated `struct B : A` form at module
level.

Labels are accepted only at impl-item level. `for` and `if` inside a process
are sequential control flow, not structure, and siox has no `break`/`next` a
label could target.

## Implementation sketch

- **Parser.** At impl-item level, look for `IDENT ":"` before `process`,
  `for` and `if`. `process IDENT {` becomes an error with the replacement text,
  and parsing continues so later diagnostics still appear.
- **AST.** `ProcessDecl.name` becomes `label`. The structural `for` and `if`
  items gain an optional label.
- **Resolve.** Labels enter the entity member namespace; duplicates are
  `E-P002`, as today.
- **Elaboration.** A labelled generate pushes a path segment (`stages[k]` or
  `debug_tap`) onto the instance paths it creates. Everything downstream
  (IR names, waveforms, diagnostics, the debugger table) already consumes
  those paths.
- **Printer.** Prints `label: process {`, `label: for …` and `label: if …`.
  `sioxc --emit source` becomes the migration tool.
- **Docs.** `language.md` §3.11 and the structural-generate text; the
  `siox-lsp` outline can list labels as document symbols.

## Migration

1. Accept labels on `process`, turn the old `process name {` into an error
   with the replacement, and rewrite the corpus with `--emit source`
   (29 named processes in 10 files).
2. Accept labels on structural `for` and `if`, with hierarchy scopes.
3. Independently of both, and first, diagnose the generate-name collision.

## Open questions

- **Should a generate that creates instances require a label?** VHDL makes
  generate labels mandatory. Requiring them would guarantee a navigable
  hierarchy, but it would break the 13 unlabelled structural `for` loops
  (in 5 files) the corpus has today.
- **Waveform spelling of an iteration scope:** `stages[0]` (siox index syntax),
  `stages(0)` (VHDL tools), or `stages_0`. VCD scope names accept brackets, but
  viewers treat them inconsistently.
- **`else` under a labelled `if`:** one scope for whichever branch was taken,
  or a separate label per branch as VHDL-2008's `case`/`if generate`
  alternatives allow.
- **Labels on concurrent assignments** (`sum: y = a + b;`), which VHDL also
  allows, would name the driver context of a bare assignment for diagnostics.
  Not proposed here.

## Non-goals

- Labels on sequential statements inside a process.
- Hierarchical references that read another instance's internals.
- Closing labels.
