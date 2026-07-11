# siox Phase 1 — Digital Language Specification and Implementation Plan

This document defines Phase 1 of siox: the digital HDL layer. Phase 1 should produce a usable digital language, a parser, a type checker, an elaborator, an event-driven simulator, a test runner, and waveform output. Analogue domains and schematic/design syntax are intentionally left for later phases.

The goal is not to finish the full language. The goal is to freeze and implement a coherent digital subset that is strong enough to write counters, FSMs, buses, ready/valid interfaces, small datapaths, test entities, assertions, and simulation traces.

---

## 1. Phase 1 goal

Phase 1 creates the digital core of siox.

It should support:

- Modules and imports.
- Public/private items.
- Type aliases.
- Structs.
- Enums.
- Entities.
- Implementations.
- Traits.
- Digital directions: `in`, `out`, `inout`.
- Parameterized entities and structs.
- Digital system attributes: `::event`, `::old`.
- Derived event helpers such as `::rising`, `::falling`, `::edge`.
- Digital simulation with delta cycles.
- Combinational assignments.
- Sequential/event-controlled assignments.
- Test entities.
- Assertions.
- Waveform output.

It should not support yet:

- Analogue `domain`.
- `across` / `through` quantities.
- `::ddt`.
- Analogue solvers.
- Mixed-signal bridges such as `sample`, `hold`, `cross`, `quantize`.
- Schematic/design language.
- Layout attributes such as `#[pos = ...]`.
- Synthesis backend.

Phase 1 is simulation-first.

---

## 2. Core principle

The digital language describes hardware behavior using a small number of core constructs.

```text
entity
    external hardware interface

impl
    behavior / implementation of an entity or type

struct
    ordinary grouped digital data shape

enum
    finite value domain

trait
    compile-time behavior/interface contract

using
    import or type alias

attr
    metadata attribute declaration

#[...]
    metadata attribute application

::
    language/system attributes and associated items

.
    member/field access

=
    declaration initialization, assignment, connection, or update depending on context
```

The language should avoid extra syntax where context is enough.

---

## 3. Phase 1 hard rules

These rules should be considered frozen for Phase 1.

### 3.1 Entity bodies are interface-only

An `entity` declares the external boundary of a component.

Allowed inside an entity body:

- Digital input ports.
- Digital output ports.
- Digital bidirectional ports.
- Bus/interface fields.
- Plain interface fields where direction is encoded in the type or mode.

Not allowed inside an entity body in Phase 1:

- `const` declarations.
- Internal signals/state.
- Behavior.
- Equations.
- Local helper variables.
- Instantiations.

Valid:

```siox
entity Counter<W: integer> {
    in clk: Clock;
    in rst: Logic;
    in en: Bit;

    out count: uint[W];
}
```

Invalid:

```siox
entity Counter {
    const W: integer;      // invalid in entity body
    let value: uint[8];  // invalid in entity body

    in clk: Clock;
    out count: uint[W];
}
```

Reason: an entity is the interface. Parameters that define the shape or behavior of an entity go in `<...>`, not inside the entity body.

---

### 3.2 Entity parameters are elaboration parameters

Entity parameters are written in `<...>`.

They must be known when the entity is specialized/instantiated.

Valid:

```siox
entity Counter<W: integer> {
    out count: uint[W];
}
```

Instantiation:

```siox
let c = Counter<W = 8> {
    .count = count8,
};
```

or positional:

```siox
let c = Counter<8> {
    .count = count8,
};
```

Recommended Phase 1 restriction: do not allow mixing named and positional parameters in the same specialization.

Valid:

```siox
Counter<8>
Counter<W = 8>
```

Invalid:

```siox
Counter<8, MODE = Fast>
```

---

### 3.3 `const` is not an entity-field feature

In Phase 1, `const` may exist at module scope or inside implementation/function contexts, but not as entity interface fields.

Valid:

```siox
const DEFAULT_WIDTH: integer = 8;

entity Counter<W: integer> {
    out count: uint[W];
}
```

Valid inside implementation if used as a local compile-time value:

```siox
impl Counter<W: integer> {
    const MAX: uint[W] = (1 << W) - 1;
}
```

Invalid:

```siox
entity Counter {
    const W: integer;
    out count: uint[W];
}
```

Reason: entity fields are externally connected ports/interface terminals, not hidden configuration.

---

### 3.4 `using` is only for imports and aliases

`using` should not create runtime/local objects.

Valid:

```siox
using std::logic::{Bit, Logic, Clock};
using Word = uint[32];
```

Invalid:

```siox
using path = a -> b; // invalid in digital Phase 1 and not an alias
```

In Phase 2 analogue, local paths should use `let`, not `using`.

---

### 3.5 `attr` declarations are required before `#[...]` use

Metadata attributes must be declared before use.

Example declarations:

```siox
module std::attrs;

pub attr top: Bool for entity;
pub attr test: Bool for entity;
pub attr keep: Bool for let, port;
pub attr library: string for entity;
pub attr name: string for entity;
```

Usage:

```siox
#[top]
entity Top {
    in clk: Clock;
}
```

Invalid if `top` was not declared/imported:

```siox
#[top]
entity Top { }
```

Invalid if type does not match:

```siox
#[top = "yes"]
entity Top { }
```

because `top` expects `Bool`.

Boolean shorthand:

```siox
#[top]
```

means:

```siox
#[top = true]
```

**Representation attributes.** `#[vector]` on an array-derived type
(`struct F : Logic[]`) tells the compiler to lay it out as one packed N-bit
signal with integer-valued arithmetic, rather than N separate `Logic`
sub-signals; `#[signed]` selects two's-complement interpretation. This is how
`uint`/`int` are defined — ordinary library `struct`s in `std/bits.siox`, not
compiler-magic names — and a user `#[vector] struct MyWord : Logic[];` gets
identical treatment. The std, not the compiler, says how and why.

**Type-targeted attributes.** A target may also be a *type name*, declaring
an attribute valid only on that entity/struct or on declarations/instances
of it — vendor metadata, like Vivado's `ASYNC_REG`/`DONT_TOUCH`:

```siox
pub attr external_clock: Bool for Pll;

#[external_clock = true]
let p = Pll { .clk, .locked };
```

Applying it to anything else is `E-P006`. Instance attributes are preserved
through elaboration (visible in `sioxc tree`) so synthesis/constraint
backends can export them to external tools.

---

### 3.6 Metadata attributes do not change core semantics by default

Metadata attributes guide tools, passes, external libraries, tests, or backends.

They should not silently change language semantics.

Examples:

```siox
#[top]
entity Top { ... }

#[test]
entity CounterTest { ... }

#[library = "work", name = "ExternalCounter"]
extern entity Counter { ... }
```

The compiler may have special passes that consume known attributes, but normal language expressions should not depend on arbitrary metadata.

---

### 3.7 `struct` is ordinary digital data

A `struct` groups fields. It does not define port directions.

Valid:

```siox
struct Packet<T> {
    valid: Bit,
    data: T,
}
```

Invalid:

```siox
struct Packet<T> {
    in valid: Bit,  // invalid: directions do not belong in normal struct fields
    out data: T,
}
```

Directions are applied at entity ports or directional bus-mode implementations.

---

### 3.8 Enums are finite digital states

Enums represent finite value domains.

Basic enum:

```siox
enum State {
    Idle,
    Start,
    Shift,
    Done,
}
```

Enum with representation:

```siox
enum State: uint[2] {
    Idle  = 0,
    Start = 1,
    Shift = 2,
    Done  = 3,
}
```

Phase 1 should avoid Rust-style payload enums. Keep enums simple.

---

### 3.9 Digital system attributes exist on every digital/discrete value

Every digital/discrete value has:

```siox
x::event
x::old
```

This includes:

- `Bit`.
- `Logic`.
- `Bool`.
- `uint[N]`.
- `int[N]`.
- Enums.
- Arrays of digital values.
- Structs whose fields are digital.

Meaning:

```text
x::event
    true if x changed value during the current simulation step/delta cycle

x::old
    value of x before the current update/event
```

Enum example:

```siox
if state::event {
    changed = '1';
}

if state::old == State::Idle and state == State::Start {
    started = '1';
}
```

Struct example:

```siox
struct Packet {
    valid: Bit,
    data: uint[32],
}

let p: Packet;

if p::event {
    packet_changed = '1';
}

if p::old.valid == '0' and p.valid == '1' {
    packet_became_valid = '1';
}
```

A struct value is built with a construction literal. The type name may be
omitted when it is fixed by context — the declared type of the assignment
target:

```siox
let p: Packet = Packet { .valid = '1', .data = 5 };  // explicit
let q: Packet = { .valid = '1', .data = 5 };         // type from `q: Packet`
```

Bits are concatenated with a brace list of positional values, most-significant
first (distinguished from a struct literal by the absence of leading `.`):

```siox
let byte: uint[8] = { hi_nibble, lo_nibble };  // hi occupies bits 7..4
```

For structs:

```text
p::event = any field changed
p::old   = previous full struct value
```

For arrays:

```text
array::event = any element changed
array::old   = previous full array value
```

---

### 3.10 Clock helpers are derived from `::event` and `::old`

`::rising`, `::falling`, and `::edge` are library-defined/system-recognized helpers for suitable clock-like types.

Example definition:

```siox
trait ClockLike {
    fn rising(self);
    fn falling(self);
    fn edge(self);
}

impl ClockLike for Logic {
    fn rising(self) {
        return self::event and self::old == '0' and self == '1';
    }

    fn falling(self) {
        return self::event and self::old == '1' and self == '0';
    }

    fn edge(self) {
        return self::event;
    }
}
```

Usage:

```siox
if clk::rising {
    q = d;
}
```

The compiler recognizes that `clk::rising` depends on `clk::event`, so the block is event-controlled.

---

### 3.11 Event-controlled blocks are inferred

No VHDL-style explicit sensitivity list is needed.

This:

```siox
if clk::rising {
    q = d;
}
```

is event-controlled because the condition depends on `clk::event`.

This:

```siox
if en {
    y = a;
}
```

is not event-controlled by itself. It is ordinary conditional logic.

Inside an event-controlled block:

```siox
if clk::rising {
    if en {
        q = d;
    }
}
```

`en` is sampled when the clock event occurs.

---

### 3.12 Assignment uses one operator

The language uses only `=`.

Its meaning depends on context.

Declaration initialization:

```siox
let value: uint[8] = 0;
```

Combinational assignment:

```siox
y = a and b;
```

Sequential/event-controlled update:

```siox
if clk::rising {
    q = d;
}
```

Instance connection:

```siox
let c = Counter<W = 8> {
    .clk = clk,
    .rst = rst,
    .count = count,
};
```

Shorthand connection:

```siox
let c = Counter<W = 8> {
    .clk,
    .rst,
    .count,
};
```

means:

```siox
let c = Counter<W = 8> {
    .clk = clk,
    .rst = rst,
    .count = count,
};
```

---

### 3.13 Sequential assignments use next-state semantics

In an event-controlled block, assignments to persistent state update at the end of the event step.

Example:

```siox
if clk::rising {
    a = b;
    b = a;
}
```

Meaning:

```text
next_a = old_b
next_b = old_a
```

This swaps `a` and `b`.

Local variables update immediately:

```siox
if clk::rising {
    let tmp = a;
    a = b;
    b = tmp;
}
```

---

### 3.14 Combinational assignments use source-order override in one driver context

Within one combinational driver context, later assignments override earlier assignments under their conditions.

Example:

```siox
y = b;

if sel {
    y = a;
}
```

Meaning: a mux — `y` takes `a` when `sel` holds, else `b`.

This allows clean default-then-override coding.

**There is no ternary `?:` operator.** Conditional selection is always written
with `if`/`else`. Two forms exist and lower to the same select:

```siox
// statement form: default-then-override drivers (above)
y = b;
if sel { y = a; }

// expression form (Rust-style; `else` is required, branches are single
// expressions, `else if` chains allowed)
y = if sel { a } else { b };
z = if x > 200 { 200 } else if x < 10 { 10 } else { x };
```

The condition follows the same `Boolean` rule as statement `if` (spec 3.16).

Invalid or warning-prone cases:

- Missing default assignment that creates latch-like behavior.
- Multiple unrelated driver contexts for the same signal.
- Conflicting assignments from multiple blocks.

Phase 1 should simulate these, but diagnostics should warn where behavior is suspicious.

---

### 3.15 Reset is normal logic

Reset is not a magic built-in concept.

Synchronous reset:

```siox
if clk::rising {
    if rst == '1' {
        q = 0;
    } else {
        q = d;
    }
}
```

Asynchronous reset pattern:

```siox
if rst == '1' {
    q = 0;
} else if clk::rising {
    q = d;
}
```

The compiler may recognize these patterns later for synthesis diagnostics, but Phase 1 simulation treats them as normal logic and events.

---

### 3.16 Digital conditions

A condition (in `if`, and later `while`/assertions) must have a type that
implements the `Boolean` trait — a type provides a truth representation, which
is applied only in condition position (not as a general implicit cast). Truth
is the kernel base type `integer`: 1 is true, 0 is false.

```siox
trait Boolean {
    fn as_bool(self) -> integer;
}
```

- `Bool` and `Bit` have built-in `Boolean` impls, so both are valid conditions.
- `Logic` has **no** `Boolean` impl, so it requires an explicit comparison —
  because `Logic` may be `'X'`, `'Z'`, etc.
- A user type opts in by implementing `Boolean` for it.

Valid (`ready: Bit`):

```siox
if ready {
    y = '1';
}
```

Valid (explicit comparison yields `Bool`):

```siox
if rst == '1' {
    q = 0;
}
```

Invalid (`rst: Logic` has no `Boolean` impl):

```siox
if rst {
    q = 0;
}
```

A user type becomes usable as a condition by implementing `Boolean`:

```siox
impl Boolean for State {
    fn as_bool(self) -> integer {
        match self {
            State::Idle => return 0,
            _ => return 1,
        }
    }
}
```

---

### 3.17 No implicit broad conversions

Avoid hidden conversions between unrelated digital types.

Use constructors/casts:

```siox
let b = Bit(x);
let l = Logic(b);
let u = uint[8](value);
```

This is especially important for `Logic` to `Bit` because unknown/high-impedance states may need explicit handling.

---


**Explicit conversions** use type application — `T(x)` — plus the
family-preserving `resize`:

```siox
y = uint[16](a);      // widen: zero-extends (uint target)
z = int[16](s);       // widen: sign-extends when the source is int
n = uint[4](a);       // narrow: truncates
k = integer(s);       // cross to the kernel word (sign-extending from int)
r = resize(a, W + 1); // keeps uint/int-ness; width is const-evaluable
```

Extension comes from the source/target families; `resize(x, n)` takes its
width as a value argument because the language is static — a const-evaluable
value argument in width position *is* a generic argument.

**Constant conversion arguments must fit the target**: `uint[4](300)` and
`int[4](-9)` are compile-time errors (`-8..7` is int[4]'s domain), the same
rule as ranged-numeric initialisers. Dynamic values get simulation-time
range checks when the simulation-reporting machinery lands (with `assert`
severity/`print!`).

**Named types convert through the `From` trait** (std::ops): `T(x)` on a
struct/enum dispatches to `impl From<Source> for T`, selected by the
argument's type — `Complex(10)`, or the resolved/unresolved crossing
`Logic(u)` / `ULogic(l)` (both are std source). Conversions never fire
implicitly — only at a conversion expression. Kernel numeric conversions
stay width-driven builtins.

### 3.18 `in`, `out`, and `inout` are permission/connection semantics

Directions define who may drive/read a field at an entity boundary.

They are not normal runtime values.

Valid:

```siox
entity Producer {
    out data: uint[8];
}
```

Inside the producer implementation, `data` is writable.

From outside the producer, `data` is readable as the output of the instance.

Port direction is primarily compiler/type-checker information, not a normal user-facing system attribute.

---

### 3.19 Bus modes are directional views over structs

Structs do not contain direction. Directional modes define how a struct behaves at a boundary.

Example struct:

```siox
struct Stream<T> {
    clk: Clock,
    rst: Logic,
    valid: Bit,
    ready: Bit,
    data: T,
}
```

Source mode:

```siox
impl out Stream<T>::Source {
    in clk;
    in rst;
    out valid;
    out data;
    in ready;
}
```

Sink mode:

```siox
impl in Stream<T>::Sink {
    in clk;
    in rst;
    in valid;
    in data;
    out ready;
}
```

Usage:

```siox
entity Producer {
    bus: out Stream<uint[32]>::Source;
}

entity Consumer {
    bus: in Stream<uint[32]>::Sink;
}
```

If no custom named mode is used, direction may apply recursively to all leaves.

Example:

```siox
bus: in Packet;
```

means all leaf fields are input/read-only inside the entity.

---

### 3.20 Traits are compile-time contracts

Traits are not runtime polymorphism.

They define required functions/methods/properties for compile-time checking and generic code.

Example:

```siox
trait Source<T> {
    fn send(self, value: T);
    fn can_send(self) -> Bit;
}
```

Implementation:

```siox
impl Source<T> for out Stream<T>::Source {
    fn send(self, value: T) {
        self.valid = '1';
        self.data = value;
    }

    fn can_send(self) -> Bit {
        return self.ready;
    }
}
```

No `virtual`, no vtables, no dynamic dispatch in Phase 1.

---

### 3.21 `override` is not needed for traits

Trait implementations do not use `override`.

Reserved future rule:

- `override` should only be for runtime virtual inheritance/polymorphism, if that ever exists.
- Phase 1 has no runtime virtual dispatch.

---

### 3.22 Pattern matching supports digital values

Phase 1 should support `match` over enums and simple bit/vector patterns.

Enum match:

```siox
match state {
    State::Idle => {
        next = State::Start;
    }
    State::Start => {
        next = State::Shift;
    }
    _ => {
        next = State::Idle;
    }
}
```

Bit-pattern match with wildcard:

```siox
match opcode {
    b"00??" => op = Op::Alu,
    b"01??" => op = Op::Load,
    b"10??" => op = Op::Store,
    _       => op = Op::Nop,
}
```

The `?` wildcard lives inside the pattern string. A prefixed string like
`b"00??"` is not a special literal token — it lexes as an identifier glued to a
string and is interpreted as a bit pattern via a *string overload* (a library
mechanism). This is not yet implemented.

Invalid:

```siox
let x: uint[4] = b"10??";
```

unless Phase 1 explicitly introduces a pattern type, which is not recommended.

---

### 3.23 Arrays and ranges

Phase 1 should support:

```siox
let data: Logic[31..0];
let byte: Logic[7..0] = data[7..0];
let bit0: Logic = data[0];
```

**Direction.** A width-only index is ascending: `Bit[4]` declares elements
`0..3`. A written range keeps its direction: `Logic[7..0]` declares
elements 7 down to 0 (descending), `Bit[0..3]` ascending. Slices follow the
written order too: `w[7..4]` extracts MSB-first (the natural bit order),
while `w[4..7]` extracts the same bits with the order reversed.

**Unconstrained arrays.** Empty brackets leave the range to be set at use
(VHDL's `range <>` box):

```siox
pub using string = Char[];   // std::text
in s: string[5];             // the use supplies the range: Char[5]
```

Using an unconstrained array where a concrete size is needed (a port, a
signal) without supplying one is an error. A **string literal supplies the
length**: `let s: string = "hello";` infers `Char[5]`, assigns one code
point per element, and whole-string equality (`s == "hello"`) compares
element by element. Whole-array assignment of the same element type and
length copies (`o = s`).

**Named ranges.** A `range` constant names a range, preserving direction,
usable in both type and slice position:

```siox
const BYTE: range = 7..0;
in b: Logic[BYTE];
z = w[BYTE];
```

Module constants and `using` aliases participate in widths, lengths, and
slice bounds (`const N: integer = 4; let a: Bit[N];`).

Range attributes:

```siox
data::width
data::range
data::low
data::high
data::left
data::right
data::direction
```

For:

```siox
let data: Logic[31..0];
```

meaning:

```text
data::left      = 31
data::right     = 0
data::high      = 31
data::low       = 0
data::width     = 32
data::direction = descending
data::range     = 31..0
```

Important distinction:

```text
range direction
    array/vector direction such as ascending/descending

port direction
    in/out/inout compiler permission model
```

Both may use the English word “direction”, but they are different concepts.

---

### 3.24 Literal suffixes and bit-string literals

A numeric literal may carry an adjacent identifier suffix (no space):

```siox
let t: Time = 10ns;      // std::sim::Time, via impl Suffix for Time
let f: Freq = 100MHz;    // std::sim::Freq
let z: Complex = 5i;     // std::math::Complex
```

Suffixes are defined by the `Suffix` trait: **each fn's name is the suffix it
defines**, and the literal desugars to that fn, inlined at lowering like an
operator impl (3.25):

```siox
impl Suffix for Time {
    fn ns(v: integer) -> Time { return Time { .fs = v * 1000000 }; }
}
```

Two loaded types defining the same suffix is an ambiguity error at the use
site. An unknown suffix is an error; a fixed fs/Hz scale table (typing the
literal as `integer`) backs bare files that load no `Suffix` impls, e.g.
`await 10ns` without imports.

A one-letter prefix glued to a string is a bit-string literal (VHDL-style),
a sized `uint` constant. The `Prefix` trait is their declared home
(`impl Prefix for uint { fn x(digits: string) -> uint; }`), with evaluation
intrinsic until const string operations exist:

```siox
let a: uint[8]  = x"AB";        // hex: width = 4 * digits
let m: uint[8]  = b"01010101";  // binary: width = digits
let k: uint[24] = x"123ABC";
```

Digits must be valid for the base; widths participate in the strict
assignment/connection width rules (3.17) and in concatenation sizing.

---

### 3.25 Operator traits (Rust-style)

Operator overloading follows Rust's `std::ops` model: each operator maps to
a **named trait** with a method named after it, and a type opts into an
operator by implementing that trait. The trait names are **compiler
built-ins** — no declaration or import:

| operator | trait | method |    | operator | trait | method |
|---|---|---|---|---|---|---|
| `+` | `Add` | `add` |    | `and` | `BitAnd` | `bitand` |
| `-` | `Sub` | `sub` |    | `or` | `BitOr` | `bitor` |
| `*` | `Mul` | `mul` |    | `xor` | `BitXor` | `bitxor` |
| `/` | `Div` | `div` |    | `nand` | `Nand` | `nand` |
| `<<` | `Shl` | `shl` |    | `nor` | `Nor` | `nor` |
| `>>` | `Shr` | `shr` |    | `xnor` | `Xnor` | `xnor` |
| `not` (unary) | `Not` | `not` |    | `< <= > >=` | `Ord` | `cmp` |

```siox
impl Add for Complex {
    fn add(self, rhs: Complex) -> Complex {
        return Complex { .re = self.re + rhs.re, .im = self.im + rhs.im };
    }
}
```

Using an operator on a user struct/enum without a matching impl is an error
(`==`/`!=` stay built-in on enums as discriminant comparison). `Self` in an
impl refers to the implementing type. User-invented operator *symbols* are
out of scope for Phase 1; the planned escape hatch keeps the named set
closed: `impl ops::custom<"sym", Rhs> for T`.

**Comparisons (`Ord`).** One `cmp` impl returning `std::ops::Ordering`
(`Less`/`Equal`/`Greater`) derives all six comparisons — like Rust's `Ord`
(or C++'s `operator<=>`): `a < b` lowers to `a.cmp(b) == Ordering::Less`,
and struct equality (which has no built-in form) comes with it:

```siox
impl Ord for Version {
    fn cmp(self, rhs: Version) -> Ordering {
        if self.major < rhs.major { return Ordering::Less; }
        if self.major > rhs.major { return Ordering::Greater; }
        if self.minor < rhs.minor { return Ordering::Less; }
        if self.minor > rhs.minor { return Ordering::Greater; }
        return Ordering::Equal;
    }
}
// v1 < v2, v1 >= v2, v1 == v2, ... all work.
```

The intrinsic numeric operators on `uint`/`int`/`integer` keep their built-in
semantics; operator traits extend the same syntax to std and user types
(`Logic` truth tables, `Complex`, ...).

**Boolean operators are boolean-per-bit.** `and`/`or`/`xor`/`nand`/`nor`/
`xnor`/`not` are one family — there is no separate bitwise-vs-logical pair.
Their meaning is fixed by the operand type: on a `Bool` they are plain
boolean; on a **bit-derived type** (`Bit`, `Logic`, `uint`, `int`, any
`#[vector]` family) they apply **per bit and return the same bit array**, the
way VHDL's `and`/`or` work on `std_logic_vector` (`"1010" and "0110"` =
`"0010"`). Boolean is simply the one-bit case of boolean-per-bit. Because it
is intrinsic to being bits, no per-type `impl` is needed — the kernel
provides it for every bit-derived type; only `Logic` overrides it, for its
four-value `'X'`/`'Z'` truth table. These operators are **rejected on
non-bit-derived types** (`real`, `Char`): per-bit boolean has no meaning
there.

Operator impls are **inlined at lowering time** as pure expression trees: the
body must be `return e;` or `if`/`else` chains ending in returns (no loops,
no state). Enum- and struct-typed operands are supported (a struct result
lowers to one driver per field).

**Mixed operands** use the trait's type argument, exactly Rust's spelling:
`impl Add<Rhs> for T`. A bare `impl Add for T` reads as `Add<Self>`; impls
on `integer` catch literal left operands:

```siox
impl Add for Complex {
    fn add(self, rhs: Complex) -> Complex { ... }
}

impl Add<integer> for Complex {
    fn add(self, rhs: integer) -> Complex { ... }   // z + 3
}

impl Add<Complex> for integer {
    fn add(self, rhs: Complex) -> Complex { ... }   // 10 + 5i
}
```

Selection is by (trait, lhs type, rhs type) — the trait argument declares
the rhs, falling back to the method's rhs parameter type. `Self` reads as
the impl target.

---

### 3.26 Ranged numerics

A value-range constraint on the numeric base types uses the parameter
brackets — `[]` stays bit/array-shaped:

```siox
using Byte = integer<0..255>;
using Short = integer<-32768..32767>;
const SHORT: range = -32768..32767;
using Short2 = integer<SHORT>;         // range constants compose
let gain: real<0.0..1.0>;
```

Semantics:

- **Storage width** derives from the range: the smallest width whose
  domain covers it (two's complement when the range dips below zero).
  `integer<0..255>` is 8 bits; `integer<0..1114111>` is 21.
- **Constant range check**: `let b: Byte = 300;` is a compile-time error.
- **Runtime range check** (implemented): a dynamic value leaving the range is a
  simulation check (later; VHDL semantics). Until it lands, arithmetic
  wraps at the storage width.
- `real<lo..hi>` documents and (later) checks the constraint; storage
  stays f64.

`std::numeric` provides the everyday names: `Byte`, `Short`, `Int`,
`Long`, `Natural`, `Positive`. (`Char` is *not* among them — characters
are non-numeric symbols; see "The type kernel".)

---

### 3.27 Foreign C functions (`extern "C"`)

C libraries connect through Rust-style extern blocks:

```siox
extern "C" {
    pub fn sqrt(x: real) -> real;      // std::math is exactly this
    pub fn labs(v: integer) -> integer;
}
```

Type mapping: `real` is `double`; `integer` and the word-sized numeric types
are 64-bit words. Calls are usable in hardware expressions and testbenches.
The JIT resolves symbols from the process; the native binary resolves them at
link time (the math library is linked by default); the interpreter — the
differential oracle — evaluates the libm set natively and does not call
arbitrary foreign code. Only the `"C"` ABI is supported.


## 4. Phase 1 implementation stages

Phase 1 should be implemented in stages. Each stage must have a concrete endgoal and acceptance tests.


### 3.28 Nominal type derivation

A new nominal type may derive from an existing one, reusing its representation
while being a distinct type. `using` stays an exact alias; `:` derives:

```siox
enum Logic : ULogic;            // same variants, new type (gains its own impls)
enum ULogic : Bit { 'Z', 'X' }  // inherit Bit's variants, add more
struct Packet : Header { data: uint[8] }   // inherit Header's fields, add more
struct Meter : real;            // a newtype over a scalar
```

Rules: an enum base must be an enum (its variants come first, so a
same-variant derivation is representation-identical); a field-adding struct
body needs a struct-shaped base — deriving fields over an *array* base is an
error (index vs. field access would collide; use composition). Duplicate
inherited variants/fields are errors. A derived type's own trait impls never
leak to its base — this is how `Logic : ULogic` gains `Resolve` while
`ULogic` stays unresolved.

**Conversions follow from derivation.** `T(x)` (the only conversion syntax —
no `as`, never implicit) is *auto-synthesized* when the derivation makes it
total: a parent-struct projection (`Header(pkt)`) or an enum whose every
source variant exists in the target (`Logic(u)`, `ULogic(bit)`). Non-total
directions (adding fields, narrowing an enum) require an explicit
`impl From<S> for T`, which `T(x)` also dispatches to. Because the mechanism
is a constructor call, a conversion is always visible at the site.
---

## Stage 1 — Syntax freeze and examples

### Goal

Freeze the Phase 1 surface syntax enough to build the parser and early compiler.

### Work items

Define exact syntax for:

- Comments.
- Modules.
- Imports.
- Type aliases.
- Parameter lists.
- Structs.
- Enums.
- Entities.
- Implementations.
- Traits.
- Trait implementations.
- Attribute declarations.
- Attribute applications.
- Function/method declarations.
- Assignments.
- If/else.
- Match.
- Loops over static ranges.
- Instance construction.
- Array/range syntax.
- Literals.
- Path syntax with `::`.
- Field syntax with `.`.

### Endgoal

A document named something like:

```text
siox_phase1_syntax.md
```

containing a frozen grammar sketch and 10 to 20 valid examples.

### Acceptance criteria

The following examples must have final Phase 1 syntax:

- Counter.
- Register with reset.
- Combinational mux.
- FSM.
- Ready/valid stream producer.
- Ready/valid stream consumer.
- Enum transition monitor using `::old`.
- Test entity with assertions.
- External entity binding.
- Attribute declaration and usage.

---

## Stage 2 — Lexer and parser

### Goal

Parse Phase 1 source files into an AST.

### Work items

Implement:

- Tokenization.
- Source spans.
- Error recovery.
- Module item parser.
- Type parser.
- Expression parser.
- Statement parser.
- Attribute parser.
- Entity parser.
- Impl parser.
- Trait parser.
- Struct parser.
- Enum parser.
- Instance construction parser.
- Pattern parser.

### AST should represent

- Modules.
- Imports/aliases.
- Attributes.
- Attribute declarations.
- Entities.
- Structs.
- Enums.
- Traits.
- Implementations.
- Functions/methods.
- Parameters.
- Ports.
- Types.
- Expressions.
- Patterns.
- Statements.
- Assignments.
- Instances.

### Endgoal

The compiler can run:

```bash
siox parse examples/counter.siox
```

and print a stable AST or pretty-printed source.

### Acceptance criteria

- Valid examples parse successfully.
- Invalid syntax gives useful error spans.
- Parser can recover after common mistakes.
- Pretty-printer round-trips simple examples.

---

## Stage 3 — Name resolution and module system

### Goal

Resolve all names to declarations.

### Work items

Implement:

- Module namespace tree.
- Imports using `using`.
- Type aliases.
- Public/private visibility.
- `::` path resolution.
- Associated items on types.
- Trait names.
- Impl target names.
- Entity instance type names.
- Attribute names.

### Name-resolution rules

`using` imports names:

```siox
using std::logic::{Bit, Logic, Clock};
```

Aliases create local names:

```siox
using Word = uint[32];
```

Fully-qualified paths remain valid:

```siox
std::logic::Bit
```

### Endgoal

The compiler can say exactly what declaration every identifier refers to.

### Acceptance criteria

- Unknown names are reported.
- Ambiguous imports are reported.
- Private items cannot be accessed from outside their module.
- Attribute usage fails if the attribute was not declared/imported.
- Associated paths like `State::Idle` resolve correctly.

---

## Stage 4 — Type system and kind checking

### Goal

Check all Phase 1 types and expressions.

### Work items

Implement:

- Primitive digital types.
- Integers and widths.
- `Bit`, `Logic`, `Bool`.
- Struct types.
- Enum types.
- Array/vector types.
- Entity types.
- Directional views.
- Bus modes.
- Function/method signatures.
- Trait bounds.
- Attribute value typing.
- Pattern typing.

### Digital type rules

Digital/discrete values support:

```siox
x::event
x::old
```

Range-like values support:

```siox
x::width
x::range
x::high
x::low
x::left
x::right
x::direction
```

Analogue attributes are not part of Phase 1:

```siox
x::ddt // invalid in Phase 1
```

### Endgoal

The compiler can reject ill-typed programs before elaboration.

### Acceptance criteria

- Cannot assign `uint[8]` to `uint[16]` without explicit conversion, unless widening rules are explicitly added.
- Cannot use undeclared attributes.
- Cannot apply attributes to wrong targets.
- Cannot write to `in` ports inside an entity.
- Cannot read unconnected outputs before they are driven, where detectable.
- Cannot call methods that are not available for a type or directional mode.
- Cannot use `Logic` as a condition without explicit comparison, if that rule is kept.

---

## Stage 5 — Entity specialization and elaboration

### Goal

Turn parameterized entities and instances into a concrete elaborated hierarchy.

### Work items

Implement:

- Entity parameter substitution.
- Type parameter substitution.
- Instance creation.
- Port connection resolution.
- Shorthand `.clk` connection.
- Nested hierarchy.
- External entity stubs.
- Bus mode expansion.
- Direction checking.
- Constant expression evaluation for parameters.

### Elaboration example

Source:

```siox
let c = Counter<W = 8> {
    .clk,
    .rst,
    .en,
    .count = count8,
};
```

Elaborated result:

```text
instance c: Counter<W=8>
    clk   -> local clk
    rst   -> local rst
    en    -> local en
    count -> local count8
```

### Endgoal

The compiler can produce a concrete instance graph.

### Acceptance criteria

- All entity parameters are known after elaboration.
- All required ports are connected or given a defined default policy.
- Direction violations are reported.
- Bus modes expand to leaf permissions.
- External entities are represented as black boxes.
- The elaborated hierarchy can be printed as a tree.

---

## Stage 6 — Digital IR generation

### Goal

Lower typed/elaborated code into a simulator-friendly digital IR.

### Work items

Represent:

- Signals/state values.
- Combinational drivers.
- Event-controlled blocks.
- Assignments.
- Next-state updates.
- Instance connections.
- System attribute reads.
- Method calls after resolution/inlining or dispatch lowering.
- Match expressions.
- Assertions.

### Important IR distinction

Combinational assignment:

```text
Driver(signal, expression, condition/context)
```

Sequential assignment:

```text
OnEvent(event_condition): next(signal) = expression
```

`x::event` and `x::old` should become explicit IR operations.

Example:

```siox
if clk::rising {
    q = d;
}
```

IR concept:

```text
EventBlock(
    condition = Rising(clk),
    updates = [Next(q) = Current(d)]
)
```

Where `Rising(clk)` lowers to:

```text
Event(clk) && Old(clk) == '0' && Current(clk) == '1'
```

### Endgoal

The compiler can print a normalized digital IR.

### Acceptance criteria

- Event dependencies are explicit.
- Combinational dependencies are explicit.
- Sequential updates are separated from immediate local assignments.
- `::event` and `::old` are represented directly.
- Method calls used in hardware code are resolved or lowered.

---

## Stage 7 — Event-driven simulator core

### Goal

Simulate Phase 1 digital designs.

### Required simulator concepts

- Current value.
- Old value.
- Event flag.
- Delta cycle.
- Driver evaluation.
- Next-state queue.
- Commit phase.
- Wakeup scheduling.
- Stable-state detection.

### Basic simulation loop

```text
1. Apply initial values.
2. Evaluate combinational drivers.
3. Commit signal changes.
4. Mark ::event for changed values.
5. Wake event-controlled blocks whose event conditions may now be true.
6. Evaluate event-controlled blocks.
7. Queue next-state updates.
8. Commit next-state updates.
9. Repeat delta cycles until stable.
10. Advance simulation time when requested by test/stimulus.
```

### `::old` rule

For every digital value:

```text
x::old = value of x before the current committed change
x      = current value after the committed change
```

### `::event` rule

```text
x::event = true in the delta cycle where x changed
```

For structs:

```text
struct::event = any field changed
```

For arrays:

```text
array::event = any element changed
```

For enums:

```text
enum::event = variant changed
```

### Endgoal

The simulator can run basic designs and produce final signal values.

### Acceptance criteria

Must simulate correctly:

- Combinational mux.
- Register.
- Counter.
- FSM.
- Ready/valid handshaking.
- Enum transition monitor using `::old`.
- Struct event detection.
- Array element event detection.

---

## Stage 8 — Test entities, assertions, and stimulus

### Goal

Allow users to write tests in siox itself.

### Test entity

```siox
#[test]
entity CounterTest {
}
```

A test entity may instantiate a DUT and create simulation stimulus.

### Phase 1 minimum test syntax

Keep this small initially.

Primitives — `await` is the one timing primitive (there is no `wait`, and a
manual clock pulse is plain code; `tick()` returns later as a std function):

```siox
await 10ns;                            // advance simulation time
clk = '1'; await 5ns; clk = '0';       // a manual pulse is just code
assert!(condition, "message");
```

#### `after`, `await`, and background clocks

Testbench assignments take an optional VHDL-style **`after`** delay. The
self-toggle idiom is the canonical clock generator; any other right-hand side
is a one-shot delayed write (the value is evaluated at schedule time, VHDL
waveform semantics):

```siox
clk = not clk after 5ns;    // free-running clock, 10ns period (VHDL style)
rst = '0' after 12ns;       // one-shot: applied 12ns from now
```

`after` is **not a keyword** — like `not` and `await` it is a plain
identifier recognized by position (between an assignment's value and `;`),
and stays usable as a name everywhere else. `after` is testbench-only in
Phase 1; hardware impls reject it. (The old `clock()` sugar was removed —
the after-form is the one generator.)

#### Generic functions and trait bounds

A function may be generic over a type with an optional trait bound:

```siox
fn maxi<T: Ord>(a: T, b: T) -> T {   // or:  fn maxi<T>(a, b) where T: Ord
    if a > b { return a; }
    return b;
}
```

Functions inline, so a call is its own monomorphization: the body dispatches
operators on the caller's concrete type (`int`'s signed `Ord`, not the kernel
compare), and the bound is checked at the call site — a named struct/enum must
carry an explicit `impl Tr`, while kernel scalars and vectors satisfy the
built-in capabilities. `where T: Ord` is exact sugar for `<T: Ord>`; the two
forms parse to the same declaration. Abstract bodies are not type-checked on
their own — each call is.

#### Macros vs. functions

The **bang marks a macro** — a compile-time syntactic form (format-string
expansion, source-location capture), like Rust's. Everything else is an
ordinary function; the language does not classify functions as pure,
impure, or procedures:

```siox
print!("n = {} and sqrt({}) = {}", n, x, y);  // macro: {} expands per argument kind
assert!(cond, "message");                     // macro: captures the source location

stop();                                       // function: halt (test passes so far)
finish();                                     // function: end the simulation cleanly
seed(42);                                     // functions: deterministic RNG
let r: uint[8] = randint(10, 20);             //   (runtime-provided, std::rand)
```

`print!` auto-newlines (like `$display`); real values print as floats, `Char`
as the character, everything else as decimal. Range violations on ranged
numerics report automatically — no syntax.

`warn!(cond, "msg")` is the **non-fatal** sibling of `assert!`: a false
condition reports to stderr and counts toward the test's warning total, but
the test still passes. It is the recoverable tier of error handling.

#### No exceptions

siox has no `throw`/`catch`. Hardware has no exceptions — no call stack, no
unwinding — and functions are pure inlined expressions with nothing to
unwind, so exception control flow has no meaning here. Errors instead take
one of three forms: **hardware error conditions are ordinary signals**
(`out error: Bit`, ready/valid); **fatal simulation errors** (`assert!`,
range violations, missing files) fail the test, like a panic; and
**recoverable conditions** use probe-and-branch (`if exists(p) { read(p) }
else { ... }`) or `warn!`. A `Result`-style value would ride on future
payload-carrying enums — never a keyword.

Testbench `let`s run in **statement order**; a name not connected to a DUT
port is a plain local. `for` binds its loop variable, and any array iterates
directly, Python-style — length is the `::len` system attribute (an
elaboration-time fact, like `::width`):

```siox
let acc: uint[8] = 0;
for x in xs { acc = acc + x; }      // desugars over 0..xs::len
for i in 0..xs::len { acc = acc + xs[i]; }
```

For edge/level-driven stimulus (mirroring cocotb's async model), `await` waits
on the scheduler those clocks run on:

```siox
await 10ns;                 // advance simulation time
await clk::rising;          // wait for the next rising edge (::falling/::event)
await count == 7;           // wait until a condition holds
```

`await`'s three forms are: a **duration** (advance time), an **edge**
(`clk::rising`/`::falling`/`::event`), and a **condition** (any boolean). Edge
and condition forms are driven by the background clocks — the scheduler steps
the clocks until the trigger fires. `await` runs identically on the
interpreter, the JIT, and the native `--no-run` binary. (`wait`/`tick` remain
for straight-line, self-clocked stimulus.)

Example:

```siox
#[test]
entity CounterTest {
}

impl CounterTest {
    let clk: Logic = '0';
    let rst: Logic = '1';
    let en: Bit = '1';
    let count: uint[8];

    let dut = Counter<W = 8> {
        .clk,
        .rst,
        .en,
        .count,
    };

    clk = not clk after 5ns;   // background clock, 10ns period

    await 10ns;
    rst = '0';

    for i in 0..10 {
        await clk::rising;
    }

    assert!(count == 10, "counter should increment 10 times");
}
```

Exact test-time syntax can be simplified for MVP.

### Endgoal

`siox test` can discover and run `#[test]` entities.

### Acceptance criteria

- Passing assertions report success.
- Failing assertions report file/span/message.
- Multiple tests can run.
- Simulation time can advance.
- Clock stimulus can be generated.

---

## Stage 9 — Waveform and tracing output

### Goal

Export simulation traces for debugging.

### Work items

- Record signal changes.
- Record hierarchy paths.
- Record enum values as symbolic names.
- Record struct fields recursively.
- Export VCD first.
- Add FST later if desired.

### Example CLI

```bash
siox sim examples/counter_test.siox --wave counter.vcd
```

### Endgoal

The user can open a waveform file in GTKWave or another viewer.

### Acceptance criteria

- Counter waveform shows `clk`, `rst`, `en`, `count`.
- FSM waveform shows symbolic states or encoded values.
- Struct fields appear as separate trace paths.
- `::old` does not need to be dumped by default, but may be enabled as debug trace.

---

## Stage 10 — Diagnostics and lint rules

### Goal

Make the compiler useful and safe to develop with.

### Required diagnostics

Errors:

- Unknown name.
- Duplicate item.
- Type mismatch.
- Invalid port direction write.
- Missing port connection.
- Invalid attribute target.
- Invalid attribute value type.
- Invalid method call.
- Invalid pattern.
- Use of Phase 2-only analogue syntax.

Warnings:

- Signal assigned in multiple independent driver contexts.
- Possible latch from incomplete combinational assignment.
- Unused signal.
- Unused parameter.
- Unused import.
- Unreachable match arm.
- Non-exhaustive enum match, if no `_` arm exists.
- Suspicious `Logic` comparison.
- Reset pattern possibly unintended.

### Endgoal

Error messages should point to useful spans and explain the rule.

### Acceptance criteria

Every diagnostic should include:

- Error code.
- Main span.
- Clear message.
- Optional help text.
- Related spans where useful.

Example:

```text
error[E-P0XX]: cannot assign to input port `ready`
  --> stream.siox:42:9
   |
42 |         self.ready = '1';
   |         ^^^^^^^^^^ input fields are read-only in `out Stream<T>::Source`
help: `ready` is declared as `in ready;` in this bus mode
```

---

## Stage 11 — Minimal digital standard library

### Goal

Provide enough standard types and helpers to write real Phase 1 examples.

### Modules

Suggested initial modules:

```text
std::logic
std::bits
std::ops
std::math
std::attrs
std::sim
std::assert
```

### The type kernel

The language kernel provides exactly three base types:

- **`integer`** — unconstrained integer; also the truth type (1 true,
  0 false; see 3.16).
- **`real`** — unconstrained float (f64 in simulation).
- **`Char`** — a character *symbol*, deliberately **non-numeric**.
  Characters and strings have no numbers: a numbering exists only relative
  to an encoding table (ASCII is not the only table), so `integer <-> Char`
  conversion goes through a named std table (`Unicode.code(c)`,
  `Ascii.char(65)`, ... — VHDL's `'pos`/`'val` made explicit and
  table-scoped). Equality on symbols is intrinsic; ordering and arithmetic
  are not. Storage is an implementation detail.

  **A character literal's identity comes from the context type**: against a
  `Char` (or `string` element) it reads through Unicode — the default
  table; against `Bit`/`Logic`/an enum it reads as the matching variant
  (`'Z'` is the Logic value, not code point 90); against a future encoding
  type (`Ascii`) it reads through that table. A context that cannot read a
  character — a numeric type — is an error: `n == 'c'` with `n: uint[8]`
  does not compile; convert through a table instead. Any Unicode symbol is
  a valid literal (`'€'`).

Plus the type machinery: enums (including character-literal variants —
symbol domains themselves), structs, arrays, ranges, value-range
constraints (3.26), and events. Every other type is declared in `std/` as
ordinary source, the way VHDL declares `bit`, `boolean` and `std_ulogic`
in library code — `uint[N]`/`int[N]` derive from `Logic`, `std::numeric`
derives the C-style ranged integers, and `string` is `Char[N]` with its
length fixed at elaboration from the initializer (pending
unconstrained-array machinery). UTF-8 is a std encoding table applied at
source/IO boundaries, never the in-memory shape.

*Shim note:* until operator overloading (3.13 traits) can carry their
semantics, the compiler still recognizes the std::logic/std::bits names
intrinsically; the declarations below are canonical and the shim is deleted
when operators move to std.

### `std::logic`

Canonical declarations:

```siox
pub enum Bit {
    '0',
    '1',
}

pub enum Logic {
    '0',
    '1',
    'Z',
    'X',
}

pub enum Bool {
    false,
    true,
}

pub enum Clock {
    '0',
    '1',
}
```

`Clock` is a `Bit` carrying clock intent; edge detection stays built-in syntax
(`clk::rising`, per 3.10).

### `std::bits`

Contains the derived numeric vectors:

```siox
uint[N]   // vector of Logic, unsigned interpretation  (VHDL `unsigned`)
int[N]    // vector of Logic, two's-complement          (VHDL `signed`)
```

Both are derived from `Logic` but accept the kernel base type `integer` on
assignment (`let x: uint[8] = 42;`), plus operations:

- Arithmetic.
- Bitwise logic.
- Comparisons.
- Shifts.
- Slices.
- Concatenation via `{hi, lo}`.

### `std::attrs`

Should contain:

```siox
pub attr top: Bool for entity;
pub attr test: Bool for entity;
pub attr keep: Bool for let, port;
pub attr library: string for entity;
pub attr name: string for entity;
```

### `std::sim`

Should contain test/simulation helpers built on the `await`/`after`
primitives — e.g. `tick(clk)` as library source once functions are callable
from testbenches:

```siox
tick
run
```

### Endgoal

Examples should not need private compiler magic except for primitive types and system attributes.

### Acceptance criteria

- Counter compiles with standard imports.
- FSM compiles with standard imports.
- Stream bus compiles with standard imports.
- Tests compile with standard imports.

---

## Stage 12 — CLI and project workflow

### Goal

Make Phase 1 usable from the command line.

### Commands

Minimum commands:

```bash
siox check <file>
siox parse <file>
siox sim <file>
siox test <path>
```

Useful debug commands:

```bash
siox ast <file>
siox ir <file>
siox tree <file>
```

### Endgoal

A user can write examples, check them, run simulations, and inspect output.

### Acceptance criteria

- `siox check examples/counter.siox` reports success.
- `siox sim examples/counter_test.siox --wave counter.vcd` produces a waveform.
- `siox test examples/` runs all tests.
- Compiler exits nonzero on failed checks/tests.

---

## 5. Phase 1 example suite

The Phase 1 repository should include examples that double as regression tests.

Required examples:

1. `basic_mux.siox`
2. `register.siox`
3. `counter.siox`
4. `fsm.siox`
5. `enum_event_monitor.siox`
6. `packet_struct_event.siox`
7. `stream_bus.siox`
8. `producer_consumer.siox`
9. `external_entity_stub.siox`
10. `attribute_usage.siox`
11. `counter_test.siox`
12. `fsm_test.siox`

---

## 6. Phase 1 final deliverable

Phase 1 is complete when the project can:

```text
1. Parse the Phase 1 syntax.
2. Resolve modules, names, attributes, and paths.
3. Type-check digital entities, structs, enums, traits, and impls.
4. Elaborate parameterized entities into a concrete hierarchy.
5. Lower designs into digital simulation IR.
6. Simulate combinational and sequential digital behavior.
7. Support `::event` and `::old` on all digital/discrete values.
8. Run `#[test]` entities.
9. Evaluate assertions.
10. Export waveforms.
11. Report useful diagnostics.
```

At that point, siox has a real digital HDL foundation. Phase 2 can then add analogue `domain`, `across`, `through`, `::ddt`, analysis domains, physical solvers, and mixed-signal bridges without destabilizing the digital core.

---

## 7. Recommended Phase 1 implementation order

The shortest practical path is:

```text
1. Syntax examples and grammar sketch.
2. Lexer/parser/AST.
3. Pretty-printer.
4. Name resolution.
5. Type checking.
6. Entity specialization and elaboration.
7. Digital IR.
8. Event-driven simulator.
9. Test runner and assertions.
10. Waveform output.
11. Diagnostics polish.
12. Standard library cleanup.
```

Do not start analogue until the digital simulator is stable enough to support tests, clocks, events, and waveforms.

---

## 8. Phase 1 design philosophy

Phase 1 should stay strict and simple.

Prefer:

```text
clear type rules
explicit conversions
entity bodies as pure interfaces
attributes declared before use
single assignment operator by context
system attributes for simulation hooks
simulation-first implementation
```

Avoid:

```text
implicit broad conversions
hidden entity constants
payload enums
runtime virtual dispatch
analogue syntax
schematic syntax
synthesis-specific behavior
```

The result should feel like a modern, compact HDL core: less verbose than VHDL, more hardware-specific than Rust, and easier to simulate than a general-purpose language pretending to be hardware.
