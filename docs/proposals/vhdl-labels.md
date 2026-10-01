# VHDL-style labels

Status: **proposal**. Nothing here is implemented. It replaces the current
`process <name> { … }` spelling with a VHDL-style label, `<label>: process
{ … }`, and extends the same label to the structural `for` and `if`
constructs of an entity implementation and to any assignment, so that the
architecture a design elaborates into can be navigated by name.

A label is **for the user, not for the compiler**. Nothing ever requires one,
and adding or removing one never changes what a design does.

## Decision

A **label** is an identifier and a colon written before a process, a
structural `for` or `if`, or an assignment:

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
    } else {
        let tap: Stub = { .i = w[3] };
    }

    sum: y = a + b;                      // a concurrent assignment

    count: process {
        if rst == '1' { n = 0; }         // `for`/`if` inside a process take no label
        step: n = n + 1;                 // a sequential assignment
    }
}
```

- Four constructs take a label: `process`; the structural `for` and `if` that
  generate hardware outside any process; and any assignment, concurrent or
  inside a process (including indexed, field, and `after`-delayed targets).
  `let` declarations already carry their own name and take no label.
- **A label is never required.** Unlabelled `process { … }`, `for`, `if` and
  assignments stay valid and keep their current behaviour.
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
does today. A duplicate is `E-P002`. Labels inside one process share that
process's namespace.

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

**Labelled `if`.** The label covers the whole `if`/`else` statement and names
one scope, filled by whichever branch the condition selects: `c.debug_tap.tap`
is a `Probe` when `DEBUG` holds and a `Stub` otherwise. The path is the same
either way, so a waveform view or a debugger script keeps working when the
condition changes.

**Labelled assignment.** The label names that one assignment. It creates no
scope, and the signal keeps its name. It identifies the assignment in the IR
and in diagnostics about it: a conflicting-driver error, a range violation, a
possible-latch warning, or an override inside a process can say "assignment
`sum`" instead of pointing only at a line.

**Iterations are indexed with square brackets.** A labelled `for` produces an
array of scopes, so its children are spelled like array elements everywhere:
`stages[0]`, `stages[1]`, in the tree, the waveform and the debugger.

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
                      | "if" expr block ( "else" … )?
                      | assignment )
             | …
statement   := label? assignment | …          (inside a process)
label       := IDENT ":"
```

Nothing that can begin an impl item today starts with `IDENT ":"`.
Declarations begin with `let`, struct literals use `.field`, and paths use the
separate `::` token. One token of lookahead after the identifier therefore
decides it. The existing single-`:` lookaheads in the parser are elsewhere:
inside `<…>` generic binders, and the deprecated `struct B : A` form at module
level.

`for` and `if` take a label only at impl-item level. Inside a process they are
sequential control flow, not structure, and siox has no `break`/`next` a label
could target. Assignments take one in both places. An assignment's target is
itself a name, so `sum: y = …` is decided the same way: an identifier followed
by a single `:`.

## Implementation sketch

- **Parser.** At impl-item level, look for `IDENT ":"` before `process`,
  `for` and `if`. `process IDENT {` becomes an error with the replacement text,
  and parsing continues so later diagnostics still appear.
- **AST.** `ProcessDecl.name` becomes `label`. The structural `for` and `if`
  items and the assignment statement gain an optional label.
- **Resolve.** Labels enter the entity member namespace; duplicates are
  `E-P002`, as today.
- **Elaboration.** A labelled generate pushes a path segment (`stages[k]` or
  `debug_tap`) onto the instance paths it creates. Everything downstream
  (IR names, waveforms, diagnostics, the debugger table) already consumes
  those paths.
- **Printer.** Prints `label: process {`, `label: for …`, `label: if …` and
  `label: target = …;`.
  `sioxc --emit source` becomes the migration tool.
- **Docs.** `language.md` §3.11 and the structural-generate text; the
  `siox-lsp` outline can list labels as document symbols.

## Migration

1. Accept labels on `process`, turn the old `process name {` into an error
   with the replacement, and rewrite the corpus with `--emit source`
   (29 named processes in 10 files).
2. Accept labels on structural `for` and `if`, with hierarchy scopes.
3. Independently of both, and first, diagnose the generate-name collision.

## Decided

- **No label is ever required**, including on generates that create
  instances. Labels exist for the reader and the tools the reader uses.
- **Iteration scopes use square brackets** (`stages[0]`): a labelled `for` is
  an array of scopes and is spelled like one.
- **One label covers a whole `if`/`else`**, and the scope is filled by
  whichever branch is taken.
- **Any assignment can be labelled**, concurrent or inside a process.

## Open questions

- **Waveform viewers and brackets.** VCD scope names accept `[` and `]`, but
  some viewers read a trailing `[n]` on a *variable* as a bit select. Scope
  names are not variables, so this is expected to be safe; confirm it in
  Surfer and GTKWave before implementation.

## Non-goals

- Labels on `for`/`if` inside a process (assignments there can be labelled).
- Hierarchical references that read another instance's internals.
- Closing labels.
