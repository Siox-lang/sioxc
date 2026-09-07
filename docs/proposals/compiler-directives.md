# `#[...]` as compiler directives

Status: **proposal**. Nothing here is implemented. It is the other half of
[attribute-system.md](attribute-system.md): once declarative metadata moves to
`attr … for … = …;`, `#[...]` is left to mean one thing.

## Decision

**`#[...]` marks something that changes compilation.** If removing it would
change only what a downstream tool sees, it is metadata and belongs in an
`attr` binding. If removing it changes what the compiler emits, accepts, or
reports, it is a directive and belongs here.

That is a semantic split. The current one is historical — a Rust spelling for
declaration metadata, a VHDL spelling for value queries — and it groups
`#[test]`, which changes what is emitted, with `#[library = "x"]`, which does
not.

## What qualifies today

Exactly one thing: `#[test]`. It registers an entity with the native test
harness, so `sioxc --test` emits a descriptor and an entry point for it.
`#[test = false]` suppresses that. Nothing else in `std/attrs.siox` changes
compilation — `keep`, `library`, `name` are consumed by tools, and
`precedence` is read by the parser but describes a fixed property of an
implementation rather than directing the compiler to do something.

## What should qualify next: lint control

Siox emits fourteen warnings and has **no way to suppress any of them**:

```
POSSIBLE_LATCH   UNUSED_SIGNAL   UNUSED_PARAM   UNUSED_IMPORT
UNREACHABLE_MATCH_ARM   NON_EXHAUSTIVE_MATCH   SUSPICIOUS_LOGIC_COMPARE
SUSPICIOUS_RESET   COMBINATIONAL_LOOP   UNDRIVEN_OUTPUT   UNCONNECTED_INPUT
DEAD_ASSIGNMENT   UNIMPLEMENTED_ATTR   INCOMPLETE_STRUCT_LITERAL
```

Several have entirely legitimate deliberate cases: a latch that is meant,
a probe signal kept for waveforms, an unused output on a shared interface, a
match left open on purpose. `INCOMPLETE_STRUCT_LITERAL` is the sharpest — its
own rationale says the omitted fields are "usually intended", so it fires on
the intended case with nothing to say "yes, I meant it".

A warning that cannot be silenced is a warning that gets ignored wholesale, and
CI runs clippy with `-D warnings` for the compiler's own Rust for exactly this
reason: suppression is what makes a deny-by-default gate usable.

```siox
#[allow(possible_latch)]
impl Latch { ... }

#[allow(unused_signal)]
let probe: unsigned[8];
```

Scoped to the item it precedes, with the same nesting rule as Rust: the
innermost enclosing level wins. `#[deny(...)]` and `#[warn(...)]` follow the
same shape if a project wants to raise a lint rather than lower it.

This does not depend on the attribute migration. `#[allow]` can be added while
`#[...]` still carries metadata, exactly as Rust mixes inert `#[test]` with
generative `#[derive]` under one spelling. Adding it first also makes the
eventual split easier to argue, because `#[...]` will visibly carry directives
before anything is moved out of it.

## What might qualify later: `derive`

`#[derive(Ord)]` on a struct would generate the `Operator<"<=>">`
implementation that already drives all six comparisons, and `#[derive(Resolve)]`
would generate an element-wise fold. Both are code generation, so both are
directives.

This is speculative. It needs a decision about where generated implementations
live for coherence (3.26 restricts inherent implementations to the defining
module), and it should not be built before there is a second real user.

## What does not qualify: `cfg`

Conditional compilation is already decided against. The compile-time target
value is `std::target: std::Target`, and the unified pipeline plan is explicit
that it "is folded before reachability and target validation. It selects code
within the same pipeline; it does not select another frontend or IR."

A value that folds is strictly better than a directive that deletes: it
type-checks both branches, it cannot desynchronise two configurations, and it
keeps one IR. `#[cfg]` should not be added.

## Boundary

| | spelling | removing it changes |
| --- | --- | --- |
| metadata | `attr keep for sig = true;` | only what a tool sees |
| directive | `#[test]`, `#[allow(...)]` | what the compiler emits, accepts, or reports |

The test for a new attribute is that one question. `#[library = "work"]` names
a vendor library and compiles identically without it — metadata. `#[test]`
determines whether an entity appears in the test executable — directive.

## Open questions

- Do directives need declaring, as `attr` does? Rust's are compiler-known and
  ad hoc. Requiring declaration would keep the "no undeclared attribute" rule
  uniform, at the cost of std declaring lint names the compiler already knows.
- Should `#[allow]` accept the stable code (`W-P002`) as well as the name?
  Codes are stable and greppable; names are readable. Rust has only names.
- Does an unrecognised directive error, or warn as `UNIMPLEMENTED_ATTR` does
  today? Erroring is safer for a directive, since silently ignoring one changes
  the build.

## Non-goals

- A macro system. `#[...]` here directs a fixed set of compiler behaviors; it
  does not run user code over a token stream.
- Retaining `#[...]` for metadata once `attr` bindings exist.
- Giving `top` compiler semantics, in this or any other spelling.
