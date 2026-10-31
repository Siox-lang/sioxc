# Implicit receivers, static functions and process-local state

Status: **accepted, not implemented** (owner, 2026-10-31). Every `self`
parameter goes, in trait declarations as well as implementations: `fn` has an
implicit receiver and `static fn` has none. `static let` is allowed only in
processes and functions, never in an entity body, where every `let` is
already a signal declared once; its exact semantics (proposed: a signal scoped
to its process, and one copy per call site in a function) are still being
decided. Written 2026-10-22 from the language design discussion. Examples below use proposed syntax; they are not claims
about what today's compiler accepts. The implemented language remains
[language.md](https://github.com/Siox-lang/siox-paper/blob/main/docs/language.md).

## Decision

Use the same function model for entities, structs, enums and applied views:

| Declaration inside an implementation | Receiver | Visibility |
| --- | --- | --- |
| `fn f(...)` | Implicit `self` | Private |
| `pub fn f(...)` | Implicit `self` | Public |
| `static fn f(...)` | None | Private |
| `pub static fn f(...)` | None | Public |

Remove `self` from source parameter lists. Keep `self` as an expression and
`self.member` as explicit access to the receiver. `Self` still names the
implementing type. `pub` controls visibility, never receiver presence.

Also allow `static let` directly inside a process to declare persistent,
process-private state initialized once per process instance. This use of
`static` concerns storage lifetime, not function dispatch.

## Why change this

Today a receiver parameter distinguishes a struct method from an associated
function. Entity helpers naturally refer to their ports and state without
writing a receiver parameter. Dropping `self` only for entities would make
`fn convert(...)` ambiguous: does it use an instance or only template values?

An explicit `static fn` answers that question without inspecting the body.
The distinction must not depend on `pub`, whether the body happens to read a
field, or whether optimization removes that read. The same source rules apply
to all implementation owners, rather than giving enums or entities exceptions.

`const fn` would answer a different question: whether a function is eligible
for compile-time evaluation. A static conversion may use a compile-time width
while operating on a changing signal. This proposal does not add `const fn`
or make static functions automatically pure or compile-time-only.

## Examples

### Structs

```siox
pub struct Counter {
    value: integer
}

impl Counter {
    pub fn increment() {
        value = value + 1;
    }

    pub fn set(value: integer) {
        self.value = value;
    }

    pub static fn from_count(value: integer) -> Counter {
        return Counter { .value = value };
    }
}
```

`counter.increment()` supplies the receiver implicitly. The parameter in
`set` shadows the field; `self.value` makes the destination explicit.
`Counter::from_count(3)` needs no existing counter. Receiver mutation, place
binding and copy-in/copy-out retain their existing semantics; omitting a
parameter does not introduce references, inheritance or a new ownership model.

### Enums

```siox
pub enum State { Idle, Busy }

impl State {
    pub fn is_idle() -> Bool {
        return self == Self::Idle;
    }

    pub static fn idle() -> State {
        return Self::Idle;
    }
}
```

An enum receiver is the enum value, not an implicit payload or discriminant.
`state.is_idle()` and `State::idle()` use the same distinction as struct calls.
Variants retain their existing `::` spelling. No payload-enum feature is added.

### Generic entities

```siox
pub entity Encoder<W: integer> {
    result: unsigned[W] out
}

impl<W: integer> Encoder<W> {
    pub static fn encode(value: integer) -> unsigned[W] {
        return unsigned[W](value);
    }

    pub fn clear() {
        result = 0;
    }
}
```

`Encoder<8>::encode(value)` binds `W` from the selected type specialization.
It does not instantiate an entity or create a connection to one. The value
argument need not be constant. Owner type arguments, including named arguments,
must survive resolution and substitution through the function body and result.

`clear` has an instance context. A public call such as `encoder.clear()` also
requires hardware boundary semantics; see [Public methods on entities](entity-methods.md).
This proposal settles its declaration syntax, not generated ports, arbitration
or scheduling. Existing unsupported cross-entity calls must stay diagnosed
until that separate work is implemented.

## Lookup, calls and access

- Inside an instance function, parameters and lexical locals take precedence
  over implicit receiver members; ordinary enclosing/module lookup follows.
  Shadowing must not fall back to a member just because the local is not
  callable or has the wrong type. Existing duplicate-name rules still apply.
- Unshadowed fields and entity ports/state may be written directly. A local
  instance-helper call `helper(x)` binds to `self.helper(x)` under the same
  lookup rules. Explicit `self.field` and `self.helper(...)` remain available.
- Static functions have no `self` or implicit member lookup. They may use
  template parameters, accessible type constants and ordinary module names.
  Private instance storage does not become accessible through a static function.
- Normal instance calls are `value.f(args)`; static calls are `Type::f(args)`.
  Inside either kind of function, `Self::f(args)` can select a static function.
  Calling a static function through a value, or an instance function as a
  receiver-free type function, is an error rather than a guessed conversion.
- `Trait::method(value, args)` remains explicit trait-qualified instance
  dispatch, where supported. Its first call argument supplies the receiver;
  it does not make the trait requirement static.
- Applied views retain their backing-type identity, direction restrictions and
  private-access rules. An implicit receiver grants no additional permissions.
- Private remains private to the existing implementation domain. A public
  method's effective visibility cannot exceed its owner or expose private
  signature types. Public entity methods do not export internal fields.

Module-level and foreign functions remain ordinary `fn` declarations with no
receiver. `static fn` is only meaningful as an implementation/trait member;
it is not a new foreign ABI or module-level storage mechanism.

## Traits, operators and constructors

Trait declarations explicitly state the same distinction, and implementations
must match it, including default bodies:

```siox
pub trait Readable<T> {
    fn read() -> T;
}

pub trait Factory<T> {
    static fn create(value: T) -> Self;
}
```

Trait implementation members still inherit trait visibility and therefore do
not add `pub`. A static/instance mismatch is a signature error, even when the
spelled argument counts otherwise match. Generic bounds and view capability
checks keep their existing meaning.

Operator methods lose only their written receiver parameter. For example,
`fn add(self, rhs: T) -> T` becomes `fn add(rhs: T) -> T`; the operator's left
operand still supplies the implicit `self`. Unary methods such as `not` have
no written value parameters but still have a receiver. Operator traits and
precedence rules do not change.

The separate [binary/unary operator proposal](binary-and-unary-operators.md)
would replace the custom binary contract and add custom prefix unary operators.
Its `apply(rhs)` and `apply()` examples use this same implicit receiver model;
neither is static. The two migrations must preserve operand count explicitly.

Associated constructors and conversions must instead become static explicitly:
`Default::default` requirements/implementations use `static fn default() -> Self`
(`New` was renamed `Default` in sioxc #304), and
`From` requirements/implementations use `static fn from(value: Source) -> Self`.
The existing `T(value)` conversion syntax and lang-item selection remain intact;
there is no implicit conversion or compiler-owned truth table added here.

## `static let` inside processes

```siox
impl CounterDevice {
    counting: process {
        static let count: unsigned[8] = 0;

        if clk.rising() {
            count = count + 1;
            q = count;
        }
    }
}
```

Proposed initial rules:

- A static local belongs to one declaration in one elaborated process instance.
  Entity instances and generated process instances get independent storage;
  generic specializations do not share a host-global variable.
- Initialize it once during that instance's initialization, before its first
  process activation. Retain it across later activations, loop iterations and
  `await`. A fresh simulation/test run initializes fresh state. A hardware reset
  after startup still needs an explicit reset branch in the design.
- Ordinary process `let` executes its initializer whenever control reaches its
  declaration. Its value already survives suspension within that execution;
  survival across `await` alone is not a reason to require `static`.
- Static-local writes are immediate, like other local writes. In the example,
  `q` receives the incremented count through the normal scheduled signal write.
  Hoisting the declaration into an ordinary impl signal must not change this
  ordering into next-state signal semantics.
- The local is private to the process. Other processes communicate through
  existing signals/ports, not by reaching into its storage. `pub static let`
  is rejected; no global shared mutable state or new locking mechanism is added.
- For the initial implementation, declare static locals directly in the process
  body, not in conditional blocks, loop bodies or helper functions. This avoids
  making initialization depend on first reach, call count or inlining policy.
- Initially require constant/elaboration-time initializers, allowing bound
  template values. An omitted initializer uses the existing type default when
  it is valid at initialization. Runtime inputs, file I/O and foreign calls
  in static-local initializers need a separate design rather than hidden lazy
  initialization.

These placement and initializer restrictions are proposed starting constraints,
not claims that broader cases are impossible. `static let` in a function,
including a `static fn`, is outside this proposal: per-function, per-receiver,
per-call-site and per-process lifetime are different choices.

Persistence denotes state, not a promise to create a flip-flop. Clocked use
can infer clocked storage; combinational use must not silently evade existing
latch, feedback, sensitivity or driver diagnostics. The acceptance work must
settle and test non-clocked retained-state cases before advertising them.

The [scoped-blocks proposal](scoped-blocks.md) remains complementary: it groups
ordinary scheduled signals and processes structurally. A process static local
is immediate, private variable state, not a scoped signal or exported member.

## Implementation and migration plan

1. **Syntax and diagnostics.** Add the declaration forms
   `[pub] static fn` and process `static let`, retaining spans for each marker.
   Record function kind and local lifetime explicitly; reject a written `self`
   parameter under the new rules with migration help. Decide whether `static`
   is reserved everywhere or only in declaration positions before landing.
2. **Resolution and typing.** Synthesize the implicit receiver binding for
   instance members and reuse the current receiver machinery. Resolve implicit
   member accesses to the same declaration identities as explicit `self` access.
   Check static isolation, specialization, trait-kind agreement, visibility,
   port directions and shadowing without inferring function kind from its body.
3. **Elaboration and canonical Process IR.** Keep one pipeline. Reuse current
   function/receiver binding and process-local allocation machinery; give static
   locals explicit per-process lifetime and once-only initializer CFGs. Do not
   let reactive-entry initialization or optimizer inlining reset/duplicate them.
   Preserve immediate local writes versus scheduled signal writes.
4. **Backend and tooling.** Preserve receiver/argument evaluation order, effects,
   captured places, suspension and debug bindings. Associate static storage with
   source process/declaration identity, independent of physical inlining. Carry
   its initialization and lifetime through native objects and metadata; do not
   implement it as a C/Rust global. Keep inspection names stable in dumps,
   waveforms where exposed, DWARF and editor API metadata.
5. **Atomic source migration.** Remove written receiver parameters from instance
   functions and mark every formerly receiver-free impl function `static`.
   Classify existing entity helpers by their actual instance dependence, not
   visibility. Migrate core traits/lang-item hooks, all std impls, actual corpus,
   embedded Rust fixtures, macros/examples, formatter and editor consumers.
   Update language.md only when the behavior lands, and reconcile the current
   entity-methods proposal's old `self`-based examples at that point.

Do not change current syntax or silently reinterpret old receiver-free helpers
while only part of this migration has landed. Explicit marker-based dispatch
must replace assumptions that parameter presence alone identifies a method.

## Acceptance checks

- Struct, enum, entity and applied-view instance/static functions; all public
  and private cases, including across modules and split implementations.
- Field/parameter/local collisions; explicit `self` access; enum `self` and
  `Self::Variant`; invalid static access to ports, state and receivers.
- Generic type/value parameters in associated calls, returns and widths,
  including named arguments and multiple concrete specializations.
- Trait defaults, receiver-kind mismatches, operator bodies, `Default`/`From`,
  qualified trait calls and standard conversion syntax after migration.
- Separate state for two entities of the same specialization, generated
  processes and successive test runs; repeated activation and `await` retention.
- Once-only initialization; static/local ordering; private static-local access;
  rejected conditional/function declarations and runtime initializers.
- Native ordinary/debug parity and default/bitpack corpus parity, including
  multiword values and exact X/Z metadata. Inspect both runtime behavior and
  generated storage ownership rather than merely accepting the syntax.

Full Rust suites, the actual sibling corpus and direct native executions must
pass before committing the implementation. This documentation change does not
implement any of these capabilities.

## Open decisions and non-goals

The common receiver/static/visibility model is the intended direction. Before
implementation, settle non-clocked persistent-state diagnostics, keyword
reservation and whether broader static-local placement/initializers are needed.
Do not let those questions silently change per-process ownership.

Cross-entity public instance calls still depend on the separate generated-port
and scheduling proposal. Function-local statics, host-global state, implicit
casts, `const fn`, new inheritance and new runtime dispatch are not included.
