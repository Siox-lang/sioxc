# Macros

Status: **proposal**. Nothing here is implemented.

This document defines a compile-time macro system for Siox. It is separate from both declarative attributes and compiler directives:

- `attr ...` stores compile-time metadata.
- `#[...]` directs compiler behavior.
- `macro` generates Siox syntax.

Macros exist for cases where an abstraction needs to create or transform program structure rather than compute a value.

## Decision

Macros are declared as ordinary module items:

```siox
pub macro <name>(<pattern>) {
    <expansion>
}
```

and invoked with `!`:

```siox
name!(...);
```

For example:

```siox
pub macro twice($x: expr) {
    $x + $x
}

let y = twice!(a);
```

expands before semantic analysis to:

```siox
let y = a + a;
```

The `!` is deliberate. A macro invocation must remain visibly distinct from a function call:

```siox
foo(...)      // runtime/elaboration function call
foo!(...)     // compile-time syntax expansion
```

Macros operate on syntax, not values.

## Why macros exist

Functions abstract behavior:

```siox
fn parity(x: UInt[8]) -> Bit {
    ...
}
```

A function receives inputs and produces a result.

That is insufficient when an abstraction needs to generate declarations, implementations, entities, statements, or other syntax.

For example:

```siox
pipeline!(stages = 4, width = 32);
```

may generate four registers and their connections.

A function cannot introduce those declarations into its caller.

The distinction is therefore:

```text
fn
    values and behavior

macro
    syntax and structure
```

This distinction is particularly important for hardware description, where structural repetition is common and often must be resolved before elaboration or synthesis.

## Declaration

A macro is declared with the `macro` keyword:

```siox
macro twice($x: expr) {
    $x + $x
}
```

Visibility follows ordinary module rules:

```siox
pub macro twice($x: expr) {
    $x + $x
}
```

Macros are normal named items and participate in normal import and module resolution.

```siox
using std::assert;

assert!(ready);
```

There is no `macro_rules!`-style secondary namespace or export mechanism.

## Parameters

Macro parameters bind syntax fragments rather than typed values.

```siox
pub macro example(
    $value: expr,
    $name: ident,
    $ty: type,
    $stmt: stmt,
    $item: item,
) {
    ...
}
```

The initial fragment kinds are:

| Fragment | Meaning |
| --- | --- |
| `expr` | expression |
| `ident` | identifier |
| `type` | type |
| `stmt` | statement |
| `item` | declaration/item |
| `path` | resolved or unresolved path |
| `tokens` | unrestricted token sequence |

The set should remain small. New fragment kinds should only be added when they provide materially better parsing or diagnostics.

A parameter is always prefixed with `$`:

```siox
$x: expr
$name: ident
```

This distinguishes macro substitution from ordinary identifiers.

## Expansion

A macro body describes the syntax emitted at its invocation site.

```siox
pub macro twice($x: expr) {
    ($x + $x)
}
```

Invocation:

```siox
let result = twice!(foo);
```

Expansion:

```siox
let result = (foo + foo);
```

Macro expansion occurs after parsing enough syntax to recognize invocations, but before name resolution and ordinary semantic analysis of the expanded program.

Conceptually:

```text
source
  ↓
parse
  ↓
macro resolution
  ↓
macro expansion
  ↓
expanded syntax tree
  ↓
name resolution
  ↓
type checking
  ↓
elaboration
  ↓
IR
```

Generated syntax is therefore checked exactly as if the programmer had written it directly.

A macro cannot bypass type checking, visibility rules, coherence rules, clock-domain validation, or any other semantic rule.

## Structural macros

Macros may emit declarations and other items:

```siox
pub macro debug_signal($name: ident, $ty: type) {
    #[allow(unused_signal)]
    let $name: $ty;
}
```

Usage:

```siox
impl Cpu {
    debug_signal!(probe, UInt[32]);
}
```

Conceptually expands to:

```siox
impl Cpu {
    #[allow(unused_signal)]
    let probe: UInt[32];
}
```

This is the primary capability that distinguishes macros from functions.

A hardware-oriented macro may therefore generate structures such as:

```siox
register_bank!(32, UInt[64]);

pipeline!(stages = 5, value = result);

interface_adapter!(Axi4, Wishbone);

fsm! {
    Idle => start ? Busy : Idle,
    Busy => done  ? Idle : Busy,
}
```

Whether any particular abstraction belongs in `std` is a separate decision.

## Hygiene

Macros are hygienic.

Names introduced inside a macro do not accidentally capture names in the invocation scope, and identifiers used by the caller are resolved in the caller's context.

For example:

```siox
pub macro once($x: expr) {
    let value = $x;
    value
}
```

must not conflict with:

```siox
let value = 10;
let result = once!(foo());
```

The internal `value` introduced by the macro receives a compiler-generated identity distinct from the caller's `value`.

Conceptually:

```text
macro-local `value`
        ≠
caller `value`
```

Hygiene should be based on identifier identity rather than generated string names.

Macros should not require conventions such as:

```text
__macro_internal_123
```

to avoid collisions.

## Caller identifiers

A macro may intentionally place a caller-provided identifier into generated syntax:

```siox
pub macro declare($name: ident, $ty: type) {
    let $name: $ty;
}
```

Usage:

```siox
declare!(counter, UInt[32]);
```

expands to:

```siox
let counter: UInt[32];
```

Because `counter` originates from the caller, it belongs to the caller's lexical context.

This is distinct from identifiers written directly inside the macro body.

## Repetition

Structural generation requires repetition.

The macro language therefore needs a compile-time repetition construct.

Proposed syntax:

```siox
pub macro tuple!($($x: expr),*) {
    {
        $($x),*
    }
}
```

However, Siox should not copy `macro_rules!` syntax merely for compatibility.

A cleaner Siox-native form should be preferred if repetition is added, for example:

```siox
pub macro connect_all($items: expr...) {
    for macro $item in $items {
        connect($item);
    }
}
```

The exact repetition syntax is deliberately left open in this proposal.

The required semantic property is only that a macro can receive and expand repeated syntax fragments without exposing runtime iteration.

## Multiple forms

A macro may need more than one accepted invocation shape.

For example:

```siox
assert!(ready);
assert!(ready, "interface did not become ready");
```

There are two reasonable designs:

```siox
pub macro assert($cond: expr) { ... }

pub macro assert($cond: expr, $message: expr) { ... }
```

or a single declaration containing alternatives.

Siox should prefer ordinary overloading if macro signatures can participate cleanly in overload resolution.

That keeps macro declarations aligned with functions and avoids introducing a separate pattern-matching language solely for macros.

If macro overload resolution proves ambiguous in practice, alternatives can be added later.

## Macro resolution

Macros use the normal module system.

```siox
module debug {
    pub macro probe($x: expr) {
        ...
    }
}
```

may be invoked as:

```siox
debug::probe!(signal);
```

or imported:

```siox
using debug::probe;

probe!(signal);
```

Macro names do not require a separate global namespace.

Whether a function and macro may share the same textual name is syntactically unambiguous:

```siox
foo(...)
foo!(...)
```

so the language may permit:

```siox
fn foo(...) { ... }

macro foo(...) { ... }
```

The resolver may maintain distinct callable and macro namespaces internally.

## Macros and compiler directives

Macros and compiler directives are separate mechanisms.

```siox
#[test]
entity Foo {
    ...
}
```

does not invoke user code.

It instructs the compiler to change compilation behavior.

Likewise:

```siox
#[allow(unused_signal)]
```

is handled by the compiler's diagnostic system.

A macro invocation:

```siox
foo!(...)
```

instead expands syntax.

The distinction is:

```text
#[...]
    compiler instruction

foo!(...)
    user-defined syntax expansion
```

Macros may emit compiler directives:

```siox
pub macro test_entity($name: ident) {
    #[test]
    entity $name {
        ...
    }
}
```

but they do not define new `#[...]` directives.

User-defined attribute macros are not part of this proposal.

## Macros and declarative attributes

Declarative attributes remain data.

```siox
attr keep: Bool for let = false;
```

and:

```siox
attr keep for probe = true;
```

do not execute macro code.

A macro may emit an attribute binding:

```siox
pub macro kept_signal($name: ident, $ty: type) {
    let $name: $ty;
    attr keep for $name = true;
}
```

but this does not make `attr` itself a macro mechanism.

The division remains:

```text
attr
    metadata

#[...]
    compiler behavior

macro
    syntax generation
```

## Built-in macros

Some syntax-generating operations may require compiler support that cannot be expressed in ordinary Siox.

These may be exposed through `std` as macros backed by compiler builtins.

For example:

```siox
pub macro assert($cond: expr) {
    builtin # assert($cond)
}
```

or:

```siox
pub macro format($args: tokens) {
    builtin # format($args)
}
```

`builtin # ...` is privileged implementation syntax available only to the standard library/compiler boundary.

The public interface remains an ordinary macro.

This keeps compiler primitives small while allowing `std` to expose them through normal language constructs.

The same principle applies to functions:

```text
compiler primitive
        ↓
std wrapper
        ↓
public language API
```

Compiler builtins should not be added merely because implementing a feature as an ordinary macro is inconvenient.

## Compile-time execution

This proposal does **not** define arbitrary compile-time execution.

A macro transforms syntax according to the macro language. It does not execute unrestricted Siox code at compile time.

In particular:

```siox
macro foo(...) {
    ...
}
```

does not imply that arbitrary functions may perform filesystem access, execute processes, inspect environment variables, or mutate compiler state.

Those capabilities would introduce reproducibility, sandboxing, caching, and build-system concerns that are outside the needs of structural HDL generation.

If Siox later gains general compile-time evaluation, its interaction with macros should be specified separately.

## Diagnostics

Errors inside an invocation should report both the generated location and the invocation responsible for it.

For example:

```siox
register_bank!(32, UnknownType);
```

should produce a diagnostic conceptually like:

```text
error: unknown type `UnknownType`
  --> cpu.siox:42:24
   |
42 | register_bank!(32, UnknownType);
   |                        ^^^^^^^^^

note: while expanding `register_bank!`
```

When the error originates from syntax written inside the macro definition, diagnostics should point to the definition as well.

Macro expansion must preserve source spans sufficiently for errors, IDE navigation, and debugging.

## Expansion visibility

Tools should be able to inspect macro expansion.

At minimum the compiler should eventually support an equivalent of:

```text
sioxc --emit-expanded
```

showing the Siox syntax after macro expansion.

This matters especially for hardware because generated structure directly affects elaboration and synthesis.

A macro system that cannot explain what hardware it generated will be difficult to debug.

The language server should likewise be able to expose generated declarations without presenting them as handwritten source.

## Recursion

Macro expansion may invoke another macro:

```siox
macro a(...) {
    b!(...)
}
```

Recursive expansion must be bounded.

The compiler should impose an expansion-depth limit and issue an error when it is exceeded.

For example:

```text
error: macro expansion limit exceeded
note: `a!` expanded `b!`
note: `b!` expanded `a!`
```

The exact default limit is an implementation detail.

## Ordering

Macros are expanded before ordinary semantic analysis, but their names must first be resolvable.

A simple compilation order is:

```text
1. Parse module structure and macro declarations
2. Resolve macro names
3. Expand macros
4. Repeat until no expandable invocations remain
5. Perform ordinary name resolution
6. Type-check
7. Elaborate
8. Lower to IR
```

Expansion may therefore introduce new ordinary declarations.

Whether expansion may introduce new macro declarations should initially be rejected. Allowing generated macros creates additional ordering and fixed-point complexity for little immediate benefit.

## Restrictions

The initial macro system should deliberately remain constrained.

A macro may:

- consume syntax fragments;
- emit valid Siox syntax;
- introduce declarations;
- emit statements and expressions;
- invoke other already-visible macros;
- emit declarative attributes;
- emit compiler directives.

A macro may not:

- mutate an already-parsed declaration in place;
- inspect arbitrary compiler semantic state;
- query inferred types during expansion;
- inspect generated RTL;
- perform filesystem or network I/O;
- define new compiler directives;
- suppress compiler errors except by emitting an existing directive such as `#[allow(...)]`.

These restrictions keep expansion deterministic and make macros understandable as source-to-source transformations.

## No procedural token-stream API initially

Rust procedural macros expose raw token streams to arbitrary compile-time Rust code.

Siox should not begin there.

A token-stream API is powerful, but it also means:

- a second compiler execution environment;
- arbitrary parser implementations inside libraries;
- weaker diagnostics;
- more difficult IDE support;
- substantially more difficult reproducible builds;
- macros that can transform almost anything into anything.

The first Siox macro system should instead be declarative and syntax-aware.

If real use cases later require arbitrary procedural transformation, that should be a separate proposal backed by concrete examples that cannot be expressed with declarative macros.

## No attribute macros initially

This proposal does not allow:

```siox
#[my_macro]
entity Foo { ... }
```

where `my_macro` is user-defined.

`#[...]` remains reserved for compiler directives.

If users need generated structure they write:

```siox
my_macro!(...);
```

This maintains the distinction the language draws for `#[...]` (language §3.5):

```text
#[...]
    fixed compiler behavior

macro!
    user-defined code generation
```

That separation also means a reader can determine whether arbitrary user expansion occurs merely by seeing the syntax.

## Example: repeated register declaration

A future repetition facility might allow:

```siox
pub macro registers($prefix: ident, $count: const, $ty: type) {
    for macro $i in 0..$count {
        let ${prefix}${i}: $ty;
    }
}
```

Usage:

```siox
registers!(stage, 4, UInt[32]);
```

could generate:

```siox
let stage0: UInt[32];
let stage1: UInt[32];
let stage2: UInt[32];
let stage3: UInt[32];
```

Identifier concatenation is intentionally not specified here; this example demonstrates the structural use case rather than committing to syntax.

In many cases indexed structures may be preferable to generated names, and the language should not encourage macros where ordinary arrays or generics express the hardware more clearly.

## Example: assertion

A macro can preserve source syntax that a function would lose:

```siox
pub macro assert($condition: expr) {
    builtin # assert($condition)
}
```

Given:

```siox
assert!(valid && ready);
```

the compiler can retain the original expression:

```text
assertion failed: valid && ready
```

A function:

```siox
fn assert(condition: Bool)
```

would receive only the evaluated condition and would no longer know how that condition was written.

## Example: structural helper

```siox
pub macro pipeline_stage($input: expr, $output: ident, $ty: type) {
    let $output: $ty;

    on clk.rising() {
        $output = $input;
    }
}
```

Usage:

```siox
pipeline_stage!(a + b, sum_q, UInt[32]);
```

expands into ordinary Siox structure.

The resulting declarations and assignments participate in the same CDC, driver, type, and elaboration checks as handwritten code.

## When not to use a macro

Macros should not replace functions, generics, arrays, loops, or ordinary language constructs.

Prefer:

```siox
fn add(a: UInt[32], b: UInt[32]) -> UInt[32]
```

over:

```siox
add!(a, b)
```

when both express the same abstraction.

Prefer a generic entity:

```siox
entity Fifo<const Depth: Integer, T> {
    ...
}
```

over:

```siox
fifo!(depth = 32, type = UInt[8]);
```

when parameterization alone is sufficient.

A macro is justified when the abstraction must manipulate syntax or generate structure that cannot otherwise be represented cleanly.

The intended hierarchy is:

```text
function
    reusable behavior

generic / const parameter
    reusable parameterized hardware

macro
    reusable syntax or structural generation

compiler builtin
    behavior impossible to express in Siox
```

## Open questions

- What exact syntax should repetition use?
- Should macros support overloads or explicit pattern alternatives?
- Is unrestricted `tokens` needed initially, or should all parameters have structured fragment kinds?
- Should item macros require a trailing semicolon?
- May expression macros expand to blocks containing declarations?
- How should macro-generated identifier construction work, if it is supported at all?
- Should macros be permitted inside entity interfaces, or only in implementation/declarative regions?
- Should expansion be represented directly in the AST or through a separate syntax tree before lowering?
- What stable representation should IDE tooling expose for generated declarations?
- Should the compiler provide a built-in source-location value to macros for diagnostics?
- How are macro versioning and exported macro ABI handled across compiled packages?

None of these require procedural macros or arbitrary compile-time execution.

## Migration

Macros are additive.

A possible implementation order is:

1. Add `macro` declarations and `name!(...)` parsing.
2. Add structured single-fragment parameters such as `expr`, `type`, and `ident`.
3. Implement hygienic substitution.
4. Allow expression and statement expansion.
5. Allow item/declaration expansion.
6. Add source-span propagation and expanded-source output.
7. Add repetition once a concrete standard-library use requires it.
8. Add compiler-backed `builtin # ...` macros where necessary.
9. Consider more powerful macro facilities only after declarative macros have demonstrated an actual limitation.

The first useful implementation therefore does not require a complete metaprogramming language.

## Non-goals

- Replacing functions or generics.
- User-defined `#[...]` directives.
- Rust-compatible `macro_rules!` syntax.
- Rust-compatible procedural macros.
- Arbitrary compile-time execution.
- Filesystem or network access during expansion.
- Conditional compilation.
- Operating on generated RTL or synthesis results.
- Allowing macros to bypass ordinary semantic validation.
- Guaranteeing that every repeated hardware pattern should be expressed as a macro.

The macro system exists to provide hygienic, explicit compile-time structural abstraction where ordinary Siox constructs are insufficient.
