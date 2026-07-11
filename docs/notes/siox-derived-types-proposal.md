# Proposal: Nominal Type Derivation and Extension in Siox

## Status

**Proposal**

This document proposes a unified mechanism for nominal type derivation and extension in Siox.

The goal is to support:

- new nominal types with the same representation as an existing type;
- enum extension;
- struct extension;
- array-derived nominal type families such as `uint` and `int`;
- trait implementations that apply only to the derived type;
- a clean hierarchy for digital logic types such as `Bit -> ULogic -> Logic -> uint | int`.

The proposal deliberately keeps `using` reserved for imports and exact aliases.

---

## 1. Motivation

Siox currently needs several kinds of related but semantically distinct types.

For example:

```siox
Bit
ULogic
Logic
uint[8]
int[8]
```

These types may share representation while differing in:

- available values;
- interpretation;
- legal operators;
- trait implementations;
- resolution behavior;
- type identity.

A concrete example is resolved versus unresolved logic.

`ULogic` should have the same logic states as `Logic`, but only `Logic` should implement `Resolve`.

Similarly, `uint[N]` and `int[N]` are both arrays of `Logic`, but they have different numeric interpretations and different operator semantics.

Siox therefore needs a way to express:

> Create a distinct nominal type based on an existing type, optionally extending its values or fields.

This proposal uses the existing `enum` and `struct` keywords for that purpose.

---

## 2. Core syntax

### 2.1 Exact aliases remain `using`

```siox
using Word = uint[32];
```

This does not create a new type.

`Word` and `uint[32]` are exactly the same type.

The same keyword is also used for imports:

```siox
using std::logic::Logic;
using std::logic::{Bit, Logic, Clock};
```

So the distinction is:

```text
using B = A     exact alias
enum B : A      new nominal enum type
struct B : A    new nominal struct-derived type
```

---

## 3. Enum derivation

### 3.1 Same value set, new nominal type

```siox
enum B : A;
```

This creates a new nominal enum type `B` derived from enum `A`.

`B` has the same variants as `A`, but `B` and `A` are distinct types.

Example:

```siox
pub enum Logic : ULogic;
```

This allows `Logic` to gain additional trait implementations without affecting `ULogic`.

For example:

```siox
impl Resolve for Logic {
    fn resolve(self, rhs: Logic) -> Logic {
        if self == 'Z' { return rhs; }
        if rhs == 'Z' { return self; }
        if self == rhs { return self; }
        return 'X';
    }
}
```

`ULogic` does not gain `Resolve`.

---

### 3.2 Enum extension

```siox
enum B : A {
    NewVariant1,
    NewVariant2,
}
```

This creates a new nominal enum type `B` that contains all variants of `A` plus the newly declared variants.

Example:

```siox
pub enum Bit {
    '0',
    '1',
}

pub enum ULogic : Bit {
    'Z',
    'X',
}
```

The effective value set of `ULogic` is:

```text
'0'
'1'
'Z'
'X'
```

But `ULogic` remains a distinct nominal type.

---

## 4. Struct derivation

### 4.1 Newtype form

```siox
struct B : A;
```

This creates a new nominal type `B` based on `A`.

No additional fields are added.

The derived type:

- has distinct nominal identity;
- reuses the representation of `A`;
- may receive its own trait implementations;
- does not cause those implementations to propagate back to `A`.

Example:

```siox
struct Meter : real;
```

Conceptually, `Meter` has the representation of `real`, but is a separate type.

---

### 4.2 Struct extension

```siox
struct B : A {
    extra: T,
}
```

This creates a new nominal type `B` that derives from struct-like type `A` and adds fields.

Example:

```siox
struct Header {
    valid: Bit,
}

struct Packet : Header {
    data: uint[32],
}
```

`Packet` is a new nominal type containing the inherited fields of `Header` plus `data`.

---

## 5. Array-derived nominal types

A central use case is defining nominal types based on arrays.

Example:

```siox
pub struct uint : Logic[];
pub struct int  : Logic[];
```

This means:

- `uint` is a distinct nominal family based on arrays of `Logic`;
- `int` is another distinct nominal family based on arrays of `Logic`;
- the concrete width is supplied at the use site.

Example:

```siox
let a: uint[8];
let b: uint[32];
let c: int[16];
```

The width belongs to the array-shaped type instance, not to an explicit generic parameter declared on `uint` or `int`.

Therefore this:

```siox
struct uint<N: integer> : Logic[N];
```

is intentionally not the preferred form.

The preferred form is:

```siox
struct uint : Logic[];
```

---

## 6. Array extension restriction

Array-derived nominal types may use the bodyless newtype form:

```siox
struct uint : Logic[];
```

But adding named fields to an array-derived declaration is rejected:

```siox
struct Foo : Logic[] {
    parity: Bit,
}
```

This should be a compile-time error.

Reason: the declaration would otherwise mix two incompatible access models:

```siox
x[3]
x.parity
```

It would also create ambiguity around:

- width;
- slicing;
- conversion to the base array type;
- equality;
- aggregate layout;
- whether extra fields are part of the indexed value domain.

Explicit composition remains available:

```siox
struct Foo {
    data: Logic[],
    parity: Bit,
}
```

This is clearer and avoids special hybrid semantics.

### Proposed rule

A struct body may add fields only when the resolved base representation is struct-like.

The restriction applies after alias resolution.

Therefore this also errors:

```siox
using LogicArray = Logic[];

struct Foo : LogicArray {
    parity: Bit,
}
```

because `LogicArray` resolves to an array type.

---

## 7. Proposed digital type hierarchy

The intended digital hierarchy becomes:

```siox
pub enum Bit {
    '0',
    '1',
}

pub enum ULogic : Bit {
    'Z',
    'X',
}

pub enum Logic : ULogic;

pub struct uint : Logic[];
pub struct int  : Logic[];
```

Conceptually:

```text
Bit
 │
 ▼
ULogic
 │
 ▼
Logic
 │
 ▼
Logic[]
 ├───────────┐
 ▼           ▼
uint[]      int[]
```

Behavior can then be layered through traits:

```text
Bit
    basic two-state logic behavior

ULogic
    inherits Bit values
    adds 'Z' and 'X'
    remains unresolved

Logic
    same value set as ULogic
    adds Resolve

uint[]
    Logic array representation
    adds unsigned numeric behavior

int[]
    Logic array representation
    adds signed numeric behavior
```

This allows semantic capabilities to grow downward without leaking upward.

---

## 8. Trait behavior

Derived types may receive additional trait implementations independently.

Example:

```siox
impl Resolve for Logic {
    ...
}
```

does not make `ULogic` implement `Resolve`.

Similarly:

```siox
impl Add for uint {
    ...
}
```

does not make arbitrary `Logic[]` values arithmetic-capable.

Likewise:

```siox
impl Ord for int {
    ...
}
```

does not affect `uint`.

### Proposed direction

Trait implementations added to a derived type never propagate to its base type.

Whether trait implementations from the base type are inherited by the derived type should be defined separately and explicitly.

A reasonable default for Siox is:

- inherited representation: yes;
- inherited enum variants or fields: yes;
- base trait behavior: available to the derived type unless overridden;
- derived trait behavior: isolated to the derived type;
- implicit coercion between base and derived types: no by default;
- explicit conversion: allowed where a conversion exists or can be derived safely.

This gives nominal safety while preserving useful behavioral reuse.

---

## 9. Enum semantics

For:

```siox
enum B : A;
```

the proposed semantics are:

- `A` must resolve to an enum type;
- `B` is a new nominal enum type;
- `B` contains all variants of `A`;
- no new variants are added.

For:

```siox
enum B : A {
    X,
    Y,
}
```

the proposed semantics are:

- `A` must resolve to an enum type;
- `B` inherits all variants of `A`;
- `B` additionally defines `X` and `Y`;
- duplicate variant names are an error.

Example:

```siox
enum Base {
    A,
    B,
}

enum Extended : Base {
    C,
}
```

Effective variants of `Extended`:

```text
A
B
C
```

---

## 10. Struct semantics

For:

```siox
struct B : A;
```

the proposed semantics are:

- `B` is a new nominal type;
- `B` uses `A` as its base representation;
- no extra fields are added.

For:

```siox
struct B : A {
    x: T,
}
```

the proposed semantics are:

- `A` must resolve to a struct-like type;
- `B` is a new nominal type;
- inherited fields remain available;
- new fields are appended to the representation;
- field-name collisions are errors.

For an array base:

```siox
struct B : A[];
```

only the bodyless form is allowed.

---

## 11. Grammar sketch

A possible grammar extension is:

```text
enum_decl
    := visibility? "enum" IDENT enum_base? enum_body_or_semi

enum_base
    := ":" type

enum_body_or_semi
    := ";"
     | "{" enum_variants? "}"

struct_decl
    := visibility? "struct" IDENT struct_base? struct_body_or_semi

struct_base
    := ":" type

struct_body_or_semi
    := ";"
     | "{" struct_fields? "}"
```

Examples:

```siox
enum Logic : ULogic;

enum ULogic : Bit {
    'Z',
    'X',
}

struct uint : Logic[];

struct Packet : Header {
    payload: uint[32],
}
```

---

## 12. Static validation rules

### Valid

```siox
enum Logic : ULogic;
```

```siox
enum ULogic : Bit {
    'Z',
    'X',
}
```

```siox
struct uint : Logic[];
```

```siox
struct Child : Parent {
    extra: Bit,
}
```

### Invalid: enum deriving from non-enum

```siox
enum X : Logic[];
```

Possible diagnostic:

```text
error: enum base type must be an enum
```

### Invalid: struct field extension over array base

```siox
struct Foo : Logic[] {
    parity: Bit,
}
```

Possible diagnostic:

```text
error: cannot add fields when deriving from an array type

  struct Foo : Logic[] {
               ^^^^^^^ array-shaped base

array-derived types may only use the bodyless form:

  struct Foo : Logic[];
```

### Invalid: duplicate enum variant

```siox
enum ULogic : Bit {
    '1',
    'Z',
}
```

Possible diagnostic:

```text
error: variant `'1'` already exists in base enum `Bit`
```

### Invalid: duplicate inherited field

```siox
struct Parent {
    value: integer,
}

struct Child : Parent {
    value: Bit,
}
```

Possible diagnostic:

```text
error: field `value` already exists in base struct `Parent`
```

---

## 13. Interaction with `using`

`using` remains an exact alias and import mechanism.

Example:

```siox
using Word = uint[32];
```

`Word` and `uint[32]` are the same type.

By contrast:

```siox
struct Word : uint[32];
```

creates a distinct nominal type.

Therefore:

```text
using Word = uint[32]
    exact alias

struct Word : uint[32];
    distinct nominal type
```

This distinction should remain strict.

---

## 14. Conversion model: `From` and `as`

Siox uses the `From` trait as the single authority for semantic conversions between types.

A conversion implementation:

```siox
impl From<A> for B {
    fn from(value: A) -> B {
        ...
    }
}
```

defines a directional conversion:

```text
A -> B
```

It does not imply the reverse conversion.

For example:

```siox
impl From<uint> for Logic[];
```

allows:

```siox
let value: uint[8] = 42;

let a: Logic[8] = value;
let b = value as Logic[8];
```

Both forms use the same `From<uint> for Logic[]` implementation.

The distinction is only syntactic:

```text
let x: B = a
    implicit conversion through From<A> for B

a as B
    explicit conversion through From<A> for B
```

No separate cast mechanism is required.

---

### 14.1 Directionality

Conversions are directional.

This:

```siox
impl From<uint> for Logic[];
```

does not imply:

```siox
impl From<Logic[]> for uint;
```

A separate implementation is required for the reverse direction.

This is important because the reverse direction may not be total.

For example:

```text
uint[8] -> Logic[8]
```

is always representable.

But:

```text
Logic[8] -> uint[8]
```

may be undesirable as an automatic conversion if the array contains:

```text
'X'
'Z'
```

Therefore the language can allow the safe direction while rejecting the unsafe or ambiguous one.

---

### 14.2 Total conversions only

A `From<A> for B` implementation should represent a total conversion:

> Every valid value of `A` must produce a valid value of `B`.

For example:

```siox
struct Complex {
    re: real,
    im: real,
}

impl From<real> for Complex {
    fn from(value: real) -> Complex {
        return {
            .re = value,
            .im = 0.0,
        };
    }
}
```

Every `real` can become a valid `Complex`, so the conversion is total.

This enables:

```siox
let c: Complex = 5.0;
```

and:

```siox
let c = 5.0 as Complex;
```

Both invoke the same `From<real> for Complex` implementation.

---

### 14.3 Derived enum conversions

Given:

```siox
enum Bit {
    '0',
    '1',
}

enum ULogic : Bit {
    'Z',
    'X',
}

enum Logic : ULogic;
```

the compiler knows these parent-to-child conversions are total:

```text
Bit    -> ULogic
Bit    -> Logic
ULogic -> Logic
```

because every parent variant exists in the child enum.

The language may therefore synthesize the equivalent of:

```siox
impl From<Bit> for ULogic;
impl From<Bit> for Logic;
impl From<ULogic> for Logic;
```

This allows:

```siox
let b: Bit = '1';

let u: ULogic = b;
let l: Logic = u;
```

and explicitly:

```siox
let u = b as ULogic;
let l = u as Logic;
```

The reverse directions are not automatically available:

```text
ULogic -> Bit
Logic  -> ULogic
Logic  -> Bit
```

because those conversions may fail depending on the current variant.

For example:

```text
ULogic('1') -> Bit
    representable

ULogic('X') -> Bit
    not representable
```

Therefore no automatic `From<ULogic> for Bit` should be generated.

If Siox later introduces a fallible conversion trait, such partial conversions should use that mechanism rather than `From`.

---

### 14.4 Struct-derived conversions

For:

```siox
struct A {
    x: integer,
    y: integer,
}

struct B : A {
    z: integer,
}
```

the compiler knows that every `B` contains the complete representation of `A`.

Therefore a conversion:

```text
B -> A
```

is total.

The language may synthesize the equivalent of:

```siox
impl From<B> for A;
```

This allows:

```siox
let b: B;

let a: A = b;
let c = b as A;
```

Both forms extract the inherited `A` portion.

The reverse direction:

```text
A -> B
```

is not total because `A` does not contain `z`.

Therefore it is not synthesized.

---

### 14.5 Array-derived conversions

For:

```siox
struct uint : Logic[];
struct int  : Logic[];
```

the compiler or standard library may define safe conversions such as:

```siox
impl From<uint> for Logic[];
impl From<int> for Logic[];
```

This enables:

```siox
let value: uint[8] = 42;
let bits: Logic[8] = value;
```

and:

```siox
let bits = value as Logic[8];
```

But this does not automatically permit:

```siox
let value: uint[8] = bits;
```

unless:

```siox
impl From<Logic[]> for uint;
```

also exists.

This keeps conversions explicit at the trait-definition level and prevents accidental reverse conversions.

---

### 14.6 Recommended rule

The recommended conversion model is:

> `From<A> for B` defines a total directional conversion from `A` to `B`. The compiler may invoke it implicitly when the expected target type is known, or explicitly through `a as B`.

Thus:

```text
From<A> for B
    defines A -> B

let b: B = a
    implicit use of From

a as B
    explicit use of the same From

B -> A
    unavailable unless separately defined
```

The type hierarchy itself may synthesize safe total conversions where the relationship guarantees representability.

Recommended automatic derivation rules:

```text
enum parent -> enum child
    yes, always total

enum child -> enum parent
    no, may fail for child-only variants

struct child -> struct parent
    yes, parent portion always exists

struct parent -> struct child
    no, child fields may be missing

array-derived type -> base array
    allowed when explicitly synthesized or implemented

base array -> array-derived type
    only when a suitable From implementation exists
```

This gives Siox one coherent conversion mechanism without mixing language-level casts and trait-based conversions.

---

## 15. Fixed-point compatibility

This design also fits future fixed-point types.

For example:

```siox
pub struct ufixed : Logic[];
pub struct sfixed : Logic[];
```

Usage could later use ranged indexing:

```siox
let a: ufixed[7..-8];
let b: sfixed[3..-12];
```

The index range would carry binary-point position, while `ufixed` and `sfixed` remain distinct nominal numeric interpretations over `Logic[]`.

This mirrors the same general rule used by `uint` and `int`:

```text
same underlying bit representation
different nominal type
different operators and semantics
```

---

## 16. Summary of the proposed type model

```text
using
    import or exact alias

enum A { ... }
    new enum type

enum A : B;
    new nominal enum with the same variants as B

enum A : B { ... }
    new nominal enum extending B with more variants

struct A { ... }
    ordinary aggregate struct

struct A : B;
    new nominal type based on B

struct A : B { ... }
    new nominal type extending struct-like B with fields

struct A : T[];
    new nominal array-derived type family

struct A : T[] { ... }
    invalid
```

---

### Conversion model

```text
impl From<A> for B
    defines a total directional conversion A -> B

let b: B = a
    implicit conversion through From

a as B
    explicit conversion through the same From implementation
```

Safe hierarchy conversions may be synthesized automatically, such as:

```text
Bit -> ULogic
ULogic -> Logic
derived struct -> parent struct
```

Potentially partial reverse conversions are not synthesized.

---

## 17. Recommended initial implementation scope

The first implementation can be deliberately conservative.

### Phase 1

Support:

```siox
enum B : A;
enum B : A { ... }

struct B : A;
struct B : A { ... }
struct B : T[];
```

Enforce:

- enum bases must be enum-shaped;
- struct field extension requires a struct-shaped base;
- array-derived struct declarations must be bodyless;
- duplicate inherited variants and fields are errors;
- every declaration creates a distinct nominal type.

### Phase 2

Define precisely:

- inherited trait implementations;
- override rules;
- base/derived conversions;
- reflection and metadata behavior;
- ABI/layout guarantees.

---

## 18. Recommended final syntax for the logic hierarchy

```siox
pub enum Bit {
    '0',
    '1',
}

pub enum ULogic : Bit {
    'Z',
    'X',
}

pub enum Logic : ULogic;

impl Resolve for Logic {
    fn resolve(self, rhs: Logic) -> Logic {
        if self == 'Z' { return rhs; }
        if rhs == 'Z' { return self; }
        if self == rhs { return self; }
        return 'X';
    }
}

pub struct uint : Logic[];
pub struct int  : Logic[];
```

This provides a direct progression from representation to interpretation:

```text
Bit
    two-state enum

ULogic
    extends Bit with unknown/high-impedance states

Logic
    same value domain as ULogic
    gains resolution behavior

uint[]
    numeric unsigned interpretation of Logic arrays

int[]
    numeric signed interpretation of Logic arrays
```

The model stays nominal, explicit, library-oriented, and well suited to HDL semantics.
