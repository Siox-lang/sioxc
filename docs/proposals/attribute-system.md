# Declarative attributes

Status: **proposal**. Nothing here is implemented. It replaces the metadata
half of `#[...]` with a declaration-and-binding pair, and leaves `#[...]` to
the compiler directives described in
[compiler-directives.md](compiler-directives.md).

Siox has two mechanisms that both get called "attributes". `#[test]`,
`#[precedence = 40]` and `#[keep]` attach metadata to a declaration (3.5/3.6);
`sig'event` and `x'length` query a value (3.9). Nothing about them overlaps —
one writes, the other reads — but they are spelled with a Rust sigil and a VHDL
sigil respectively, in a language that borrows from both, so the pair reads as
duplication.

That would be cosmetic on its own. The substantive problem is that the split
falls in the wrong place. `#[...]` today mixes inert annotation with real
compiler directives, and there is no way to tell them apart.

## Decision

Metadata becomes a declaration and a binding, both spelled with the existing
`attr` keyword:

```siox
attr <name>: <Type> for <targets> = <default>;   // declaration
attr <name> for <object> = <value>;              // binding, named
attr <name> = <value>;                           // binding, enclosing item
<object>'<name>                                  // read
```

`#[...]` keeps only what changes compilation. For now that is `#[test]` alone.

## Why this shape

**`for` is already the language's word for "targets X".** Three of its four
current uses mean exactly that — `impl Trait for Type`, `view Source for
Stream<T>`, `attr keep: Bool for let, port` — so a binding reads in vocabulary
that already exists. VHDL spells the same thing `of`; siox does not need to
borrow a fourth preposition.

**The `attr` keyword marks the statement as compile time, lexically.** An
earlier form under review, `p'keep = true;`, was rejected precisely because `=`
is the signal assignment operator: a metadata binding would have had the same
shape as a driver, which is the one distinction a hardware language cannot
afford to blur. `attr keep for p = true;` cannot be mistaken for hardware.

**Objectless and named bindings compose as *this* versus *that*.** Inside an
impl, `attr precedence = 40;` binds the enclosing implementation while
`attr keep for acc = true;` binds a named declaration — the same distinction as
`self` versus an explicit name. The declaration stays unambiguous because only
it carries `: Type`.

**Defaults are what make reading safe.** Neither VHDL nor siox has them. In
VHDL, `sig'foo` is an error unless someone specified `foo` on `sig` somewhere
else in the file, which is why user attributes are so easy to lose track of.
With a declared default every object has every attribute, so a read always
answers:

```siox
attr external_clock: Bool for Pll = false;
...
if p'external_clock { ... }        // total, never "no such attribute"
```

Defaults also retire the `#[test]` == `#[test = true]` shorthand, which exists
only because there is no default today.

**Targets stay in the declaration.** VHDL puts the entity class in each
specification, so nothing can be checked until a tool looks at it. Siox already
fixes legal targets once, which is what makes `E-P006` a compile error rather
than a vendor-specific shrug. That property is retained, and it additionally
lets an objectless binding resolve its target: `attr test = true;` written in
an impl attaches to the entity when `test` is declared `for entity`, and to the
implementation when `precedence` is declared `for impl`.

## Example

```siox
attr test: Bool for entity = false;
attr keep: Bool for let, port = false;
attr precedence: integer for impl = 0;
attr external_clock: Bool for Pll = false;

impl Operator<"nand", Logic, Logic> for Logic {
    attr precedence = 40;              // enclosing implementation
    fn apply(self, rhs: Logic) -> Logic { ... }
}

impl Top {
    attr keep for probe = true;        // a named declaration
    attr external_clock for p = true;

    let probe: unsigned[8];
    let p: Pll = { .clk = clk, .locked = l };
}
```

## What this costs

These are the reasons the proposal is written down rather than applied, and
they should be answered before any of it is built.

- **Locality.** A binding no longer sits on the declaration it describes, so a
  reader cannot see a `let`'s attributes by looking at the `let`. This is the
  one property `#[...]` has and VHDL lacks, and it is the main argument for
  leaving metadata where it is.
- **Forward references.** May a binding precede its target? Cross a module
  boundary? VHDL answers yes within a declarative region; siox must decide, and
  must say what happens when two bindings name one object (an error, unlike
  drivers, where a later write overrides under 3.14).
- **Entity bodies are interface-only (3.1).** An entity-targeted binding cannot
  live inside the entity, so it must be resolved from the impl through the
  declaration's `for` clause. That is the mechanism above, but it means the
  binding sits somewhere other than the thing it describes.
- **`for` reaches five uses.** Reusing the word is coherent; it is also a lot
  of weight on one keyword.

## Migration

Each step leaves the compiler usable.

1. **Add defaults to `attr` declarations.** Additive: `= <default>` is optional
   and changes nothing where it is absent.
2. **Add readback through `'`.** Elaboration already retains what this needs —
   `Instance::attrs` is a `Vec<(String, Option<String>)>` of name and
   pretty-printed value, kept for external tools — so a read with default
   fallback is a lookup rather than a new mechanism. Note the tick's set is
   currently closed and compiler-known; this is the first user-extensible entry
   and needs the resolver to consult the attribute namespace.
3. **Add the binding statement**, both forms, accepted alongside `#[...]`.
4. **Migrate std and the corpus**, leaving `#[test]` alone: `precedence` (25
   uses), `keep`, `library`, `name`, and user attributes such as
   `external_clock`.
5. **Reject `#[...]` for anything but directives.** At this point `#[test]` is
   the only remaining use, and `#[...]` means "changes compilation".

Steps 1 and 2 are worth doing regardless of whether the rest is ever adopted:
they are additive, need no syntax decision, and readback is a capability the
language does not currently have at all.

## Why `#[test]` stays

`#[test]` is not metadata. It registers an entity with the native test harness
and changes what `sioxc --test` emits, which is a directive in exactly the Rust
sense. It is also the dominant use (about 188 in the corpus against 25 for
`precedence` and one user attribute), it sits on the entity's own line where it
is easy to scan, and an entity body cannot hold a binding anyway. Moving it
would cost visibility on the common case and buy nothing.

## Non-goals

- Reading attributes at run time. Bindings are elaboration-time constants, like
  VHDL's, and are folded before validation.
- `all`/`others` bulk binding. VHDL has it; it is not proposed here, and its
  absence should be a deliberate later decision rather than an oversight.
- Giving `top` compiler semantics. It stays vendor metadata (3.5).
- Changing the tick's existing closed set of value and shape queries.
