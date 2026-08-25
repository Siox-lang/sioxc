# Language design review

This document is a candid assessment of the current siox language rather than
a specification. The authoritative syntax and semantics remain in
[`language.md`](language.md); implementation gaps remain in
[`TODO.md`](../TODO.md). This review asks a different question: **which choices
form a coherent language, and which choices are likely to confuse users or
constrain later synthesis work?**

The assessment is based on the Phase 1 compiler, standard library, runnable
corpus, and current Phase 2/3 roadmap as of August 2026. “Does not make sense”
does not always mean “remove it.” Some choices are sound but need a sharper
rule, a less ambiguous spelling, or a clear boundary before they become part of
a stable language.

## Evaluation criteria

A language choice is strongest when it satisfies all of these:

- **hardware truth:** the syntax does not hide wires, state, scheduling, or
  width;
- **one concept, one rule:** similar constructs behave alike without special
  cases based on a familiar type name;
- **local readability:** a reader can understand an expression without knowing
  which unrelated modules were loaded;
- **tool independence:** the core language does not encode Vivado, Quartus,
  VHDL, Verilog, or a particular simulator;
- **diagnosability:** invalid hardware fails at the source construct that made
  it invalid;
- **future compatibility:** simulation conveniences do not quietly acquire a
  different meaning when synthesis arrives.

## What makes sense

### 1. Entities describe interfaces; `impl` describes behavior

Keeping ports in the entity header and state, instances, helper functions, and
drivers in `impl` gives hardware a visible structural boundary. It is clearer
than treating an entity as a software object whose fields happen to be wires.
Entity ports being inherently visible also follows naturally: a hidden port is
not part of a usable hardware interface.

The visibility model reinforces this separation well:

- module declarations are private unless exported;
- struct fields are private representation unless exported;
- entity ports are interface endpoints and need no `pub`;
- entity implementation state cannot be reached through an instance;
- a no-`self` entity function is only a namespaced function, not a hidden
  cross-hierarchy operation.

### 2. Structs carry layout while views carry protocol direction

A bus has one physical layout but multiple roles. Describing the fields once in
a struct and projecting `Source`, `Sink`, `Master`, or `Slave` views avoids
duplicating types and keeps direction out of stored data. Letting a view expose
private backing fields is also correct: a protocol endpoint deliberately
exposes those wires without making the raw struct representation public in
ordinary value code.

Views over a backing struct are preferable to standalone views. A direction
without a field type or reusable layout has too little information and would
create a second, weaker record system.

### 3. Traits are static contracts, not runtime objects

Hardware benefits from monomorphized behavior and compile-time checking. No
vtables, runtime type tests, or implicit dynamic dispatch keeps the generated
structure knowable during elaboration. Traits also give views reusable behavior
without introducing class inheritance.

The restriction that foreign modules cannot add inherent members is similarly
sound: extension uses a trait, while the nominal type keeps one coherent
private implementation domain.

### 4. Nominal derivation reuses representation without structural extension

`struct Meter(real);` and `enum Logic(ULogic);` create new identities while
reusing representation. Refusing to add fields or variants through derivation
avoids two incompatible meanings of inheritance and keeps layout exact.
Composition remains the explicit way to build a larger record.

This is especially useful for hardware units and interpretations: `time`,
`frequency`, `unsigned`, and `signed` can share a representation with another
type without becoming interchangeable with it.

### 5. Explicit conversion is the default

Using `T(value)` for visible conversion, with automatic conversion only along a
representation-identical derivation chain, prevents width and domain changes
from disappearing inside assignments. Hardware code benefits more from visible
boundaries than from convenient but lossy coercion.

Contextual integer and character literals are a reasonable exception: the
literal has no storage until its context selects one.

### 6. The compiler names mechanisms; std owns domain behavior

Keeping `Bit`, `Logic`, `Bool`, numeric vector families, time units, resolution
tables, and most operators in ordinary siox source is one of the language's
best architectural choices. It makes the contracts inspectable and allows user
types to use the same mechanisms as standard types.

The principle also gives future foreign libraries a clean boundary: the
compiler should understand a type and its ABI/layout facts, not bake a vendor's
package semantics into keywords.

### 7. One operator contract is easier to extend than one trait per symbol

`Operator<"symbol", Input, Output>` represents mixed operands and non-self
result types without a growing compiler trait catalogue. A single `apply`
contract also makes operator capabilities easy to express as generic bounds.

Deriving six comparisons from one `<=>` implementation is economical for
types with a genuine total order. Custom operators being normal trait impls is
also a useful extension point.

### 8. Boolean and bit-vector operations use one type-directed family

Having `and`, `or`, and `not` mean boolean operations on `Bool` and per-element
logic on bit families matches established HDL practice. It avoids the software
language split between logical and bitwise token families, which is less useful
when the operand type already says whether a value is a truth value or a wire
vector.

Keeping nine-value `Logic` out of condition position is a particularly good
safety rule. `if logic` has no honest answer for `U`, `X`, or `Z`; requiring an
explicit comparison forces the author to choose one.

### 9. Range direction is part of the type

Preserving `7..0` versus `0..7`, keeping written labels, and making `'left`,
`'right`, and `'range` available reflects real HDL interfaces. Compact storage
does not need to erase source labels. Partial ranges derived from the indexed
object's bounds are convenient for generic bus code.

Treating ranges as values for custom indexing is also coherent: intrinsic
arrays and overloaded containers use the same surface syntax.

### 10. Sequential and combinational assignment rules are explicit

Next-state updates in edge-controlled blocks, delta-cycle settling, and
source-order override within one driver context give assignments deterministic
meaning. Reset being normal conditional logic rather than a separate magic
construct keeps synchronous and asynchronous reset shapes expressible without
adding special state semantics.

### 11. Resolved and unresolved logic are distinct types

`ULogic` rejecting parallel drivers while `Logic` opts into a `Resolve`
contract captures a valuable hardware distinction. Multiple drivers are not
silently accepted merely because a value happens to be logic-shaped, and a
user-defined resolved type can state its own rule.

### 12. `sioxc` is a compiler, not a project manager

The rustc-shaped boundary is sensible. One compiler invocation should compile
one selected design or test artifact. Dependency graphs, repeated test runs,
foreign library discovery, caching, and vendor flows belong in a future
Cargo-like tool. That keeps both the command line and embedding API usable by
other tools.

### 13. Testbenches compile into ordinary executables

`#[test]` entities, native filtering, assertions, deterministic randomness,
timing, and direct VCD/FST output make simulation easy to automate without
hiding it inside the compiler process. A generated executable is also a good
debugger and CI boundary.

### 14. Nominal identities include their modules

Two modules may define the same leaf name without sharing fields, variants,
methods, views, traits, operators, constants, or output scopes. This is basic
language hygiene, but it matters unusually much in an HDL where vendor and
protocol libraries commonly reuse names such as `Cell`, `State`, `Source`, and
`Word`.

## What does not yet make sense

The entries below are ordered roughly by how important they are to settle
before declaring the surface stable.

### 1. Operator authority is split ambiguously between the kernel and std

The intended principle says operator behavior belongs in `std`, but several
documents and implementations describe `and`/`or`/`not`, `Bit`, vector
arithmetic, comparisons, and conditions as both intrinsic and supplied by
traits. For example, std contains explicit `Operator` impls for `Bit` and
`Bool`, while parts of the language reference say no per-type impl is needed.

Some bootstrap fallback is practical, but the stable rule needs an exact
matrix:

- the parser owns tokens and fixed precedence for core symbols;
- kernel `integer`, `real`, and `Char` may own irreducible primitives;
- std declarations own public contracts and non-kernel behavior;
- fallback behavior must either be observably identical to std or rejected
  once std is loaded.

Until that matrix is written down, users cannot tell whether removing an impl,
shadowing a hook name, or defining a new bit family changes semantics.

### 2. An applied view is written as two adjacent type names

`Stream<T> StreamSource` is compact, but it asks the reader and parser to infer
that the second name is a role rather than another type, direction, or missing
punctuation. The declaration reads `view StreamSource for Stream<T>`, while the
use reverses that relationship. Trait impls become especially dense:

```siox
impl Writable<T> for Stream<T> StreamSource { ... }
```

The current backing-first order is internally consistent with a direction
suffix, but the relationship is under-signaled. Before syntax freeze, compare
it against an explicit form such as `Stream<T> as StreamSource` or a qualified
type form. If adjacency stays, the formatter and diagnostics must make the
pair visually unmistakable everywhere.

### 3. `..` is inclusive despite the language's Rust-shaped surface

Inclusive ranges are natural for hardware (`7..0` contains both endpoints),
and this project has deliberately chosen them. The problem is not the
semantics; it is the expectation created by Rust-like `fn`, `impl`, traits,
generics, and attributes, where `..` normally excludes the right endpoint.

`Bit[4]` meaning labels `0..3` while `Bit[0..3]` names the same four elements
adds another rule to remember. If inclusive `..` stays, it should be called out
in every introductory range example and diagnostics should explicitly say
“inclusive.” Supporting `..=` as a second spelling would make the model worse;
there should remain exactly one range operator.

### 4. One `<=>` contract conflates equality with total ordering

Deriving all comparisons from one implementation is excellent for integers,
versions, and other totally ordered values. It is too strong for types that
have meaningful equality but no honest total order. Requiring an arbitrary
ordering merely to write `a == b` encourages meaningless contracts.

Enums currently receive intrinsic equality, which already demonstrates that
equality is conceptually separable. The language should decide whether to add
an equality-only contract or explicitly restrict custom equality to totally
ordered types.

### 5. `Vector` looks behavioral but changes physical representation

Traits are documented as compile-time behavioral contracts, yet implementing
the canonical `Vector` trait opts a newtype into packed numeric storage and
backend behavior. That is a representation decision disguised as an empty
behavioral trait.

Either document `Vector` as a compiler-known representation marker—not an
ordinary trait—or replace it eventually with explicit derivation/layout
metadata. Users need to know that this impl changes more than which methods are
available.

### 6. Standard logic still depends on a discriminant convention

The standard library says it owns logic values and truth tables, but the
metavalue plane currently relies on `'0'` and `'1'` occupying discriminants 0
and 1 and treats higher discriminants as metavalues. Comments in `std::logic`
also say its order must match compiler/backend shims. That is the reverse of
the desired dependency direction.

This convention is efficient, but it should become explicit compiler-consumed
metadata derived from the enum declaration rather than an ordering promise in
a comment. Otherwise reordering a perfectly ordinary std enum can silently
change storage semantics.

### 7. Simulation-only operations can appear in hardware expressions

`extern "C"` calls are currently accepted in combinational and clocked design
logic, and `read<T>` means compile-time ROM input in hardware but runtime file
I/O in a testbench. These rules are usable in a simulation-first compiler, but
their synthesis meaning is absent or context-dependent.

Before synthesis output exists, every operation should acquire an explicit
classification such as synthesizable, elaboration-only, or simulation-only.
The compiler can then reject a design for the intended target instead of
letting a future backend reinterpret existing source.

### 8. Runtime out-of-range indexing fails soft

An out-of-range packed read returns zero and an out-of-range write is a no-op.
That is deterministic, but it hides address bugs in a language that otherwise
prioritizes strict widths, ranges, and diagnostics. It can turn a broken index
into plausible hardware behavior.

A bounds failure, assertion, or explicitly selected wrapping/saturating policy
would be easier to trust. If zero/no-op remains for synthesis reasons, native
tests should at least have an opt-in strict mode or a warning that identifies
the index site.

### 9. Default construction and hardware initialization are easy to confuse

`T::new()`, `T()`, and an omitted initializer all produce deterministic
simulation state, while reset remains ordinary explicit logic. The semantic
distinction is sound, but the surface makes a simulation power-on default look
like a hardware construction guarantee.

The synthesis-facing model must say whether an initializer requires
initializable storage, becomes an `initial`/configuration value, or is ignored
with a diagnostic. This cannot be left entirely to vendor adapters.

### 10. Custom operator precedence is attached to an implementation

Precedence affects parsing globally, but `#[precedence = N]` lives on a
type-specific impl. Several impls of the same operator must therefore agree on
a grammar fact that does not belong to any one operand type. Imports also have
to be discovered before parsing so the grammar can be assembled.

The current compiler detects conflicts, which makes the design workable. A
separate operator declaration would nevertheless be conceptually cleaner:
declare the symbol and precedence once, then provide any number of `Operator`
impls. This becomes more important for package-scale tooling and incremental
parsing.

### 11. The type-construction surface is overloaded

These forms are individually defensible but collectively dense:

- `struct Packet { ... }` declares a record;
- `struct Meter(real);` declares a nominal newtype;
- `T { ... }` constructs a record;
- `T()` constructs a default;
- `T(value)` converts;
- `T[N]` may constrain a vector or array;
- `integer<left..right>` uses generic-looking brackets for a value range.

The compiler can distinguish them, but readers must learn several meanings of
the same punctuation. This needs a compact “construction and constraint forms”
table near the start of the language guide and should not acquire more cases.

### 12. Entity functions have a sharp associated/receiver cliff

`Entity::helper(...)` works when the function has no `self`, while a public
function with `self` and any `instance.method()` call are rejected. The rule is
correct today—an instance call requires real ports and scheduling—but it is
surprising if both are casually called “entity methods.”

Documentation and diagnostics should consistently say **associated function**
for the implemented form and **receiver method** for the proposed generated-port
form. When receiver methods land, generated ports must remain visible in tree,
metadata, waveforms, and synthesis output.

### 13. Clock inference is elegant but under-specified for tooling

Inferring an event-controlled block from `if clk.rising()` avoids another
process syntax and reads naturally. With no dedicated clock type, however, any
signal or user `ClockLike` implementation can become a clock. That complicates
CDC analysis, timing constraints, and synthesis diagnostics.

The language does not necessarily need a clock type, but elaborated metadata
does need an authoritative set of clock domains and derived-clock
relationships. A behavioral helper alone is not enough for Phase 3 tools.

### 14. `using` and future library discovery need one vocabulary

Today `using` imports declarations and creates type aliases. The roadmap also
proposes language-neutral external-library discovery, previously sketched as
`use <library>`. Two near-identical keywords with different project/compiler
ownership would be difficult to teach.

Foreign library selection belongs to the future project tool or a clearly
distinct source declaration. It should not look like an ordinary symbol import
unless it participates in exactly the same module graph.

### 15. The normative language document contains too much history

[`language.md`](language.md) is both the current specification and a record of
twelve historical implementation stages. That makes it hard to know whether a
later “work item” is normative, completed, or obsolete, and genuine
contradictions are harder to spot.

Move historical stages into a development-history document. Keep the language
reference organized by user concepts, with one current rule for each concept.
This is documentation structure, but documentation structure becomes language
design when it determines which rule implementers follow.

## Incomplete does not mean incoherent

These items are missing or partial, but their current direction makes sense and
does not need a syntax redesign merely because implementation remains:

- public entity receiver methods, provided they elaborate into inspectable
  ports and initially enforce one caller per method/instance;
- arbitrary one-shot delayed writes, once overwrite/cancellation semantics are
  defined;
- a Cargo-like project and test tool outside `sioxc`;
- a stable scheduler boundary and cocotb/VPI integration;
- foreign HDL libraries selected by project/backend metadata rather than HDL
  keywords;
- vendor-neutral elaborated RTL output before Vivado/Quartus adapters;
- reusable std models such as synchronizers, memories, streams, and FIFOs;
- comment-preserving formatting and incremental compiler queries;
- a separate analogue IR and solver rather than analogue behavior leaking into
  the digital IR.

## Decisions worth freezing next

Before adding more surface syntax, the highest-value decisions are:

1. Write the exact operator/intrinsic ownership matrix for compiler versus std.
2. Freeze or replace the adjacent applied-view spelling.
3. Decide whether equality can exist without a total `<=>` order.
4. Make simulation/elaboration/synthesis availability an explicit semantic
   property of functions and intrinsics.
5. Replace the standard-logic discriminant convention with compiler-consumed
   declaration metadata.
6. Choose a strict policy for runtime out-of-range indexing.
7. Specify clock-domain metadata independently of the convenient
   `clk.rising()` surface.

## Bottom line

The language has a coherent center: explicit hardware interfaces, nominal
types, role-based views, static contracts, visible conversion, deterministic
event semantics, and a std-owned domain library. It already feels more like a
purpose-built HDL than “Rust syntax translated into gates.”

The weak points are mostly boundaries where a convenient Phase 1 shortcut is
starting to look like a permanent semantic rule: compiler/std operator
fallbacks, representation marker traits, discriminant conventions,
simulation-only calls in hardware, and soft bounds behavior. Settling those
before synthesis and package tooling will prevent implementation details from
becoming accidental language design.
