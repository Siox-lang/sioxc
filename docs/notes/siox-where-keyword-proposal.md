# Proposal: `where` Clauses in Siox

## Status

**Proposal**

This document proposes a Rust-style `where` clause for Siox.

The goal is to separate generic parameter declaration from semantic constraints, especially when:

- several generic parameters have trait requirements;
- constraints become too long for the declaration header;
- constraints depend on associated types or other type expressions;
- implementations need conditional applicability;
- entities, structs, enums, functions, traits, or impl blocks need readable generic bounds.

The proposal does not replace inline bounds such as:

```siox
fn add<T: Add>(a: T, b: T) -> T {
    ...
}
```

Instead, it adds an equivalent and more scalable form:

```siox
fn add<T>(a: T, b: T) -> T
where
    T: Add,
{
    ...
}
```

---

## 1. Motivation

Siox already uses generic parameter syntax such as:

```siox
entity Counter<W: integer> {
    out count: uint[W];
}
```

and trait relationships such as:

```siox
impl Add for uint {
    ...
}
```

Inline constraints work well for short declarations:

```siox
fn compare<T: Ord>(a: T, b: T) -> Ordering {
    ...
}
```

But they become difficult to read when several parameters or constraints are involved:

```siox
fn transform<T: Add + Mul + Ord, U: From<T> + Clone, const N: integer>(...)
```

A `where` clause moves those constraints into a separate block:

```siox
fn transform<T, U, N: integer>(...)
where
    T: Add + Mul + Ord,
    U: From<T> + Clone,
{
    ...
}
```

This keeps the declaration header focused on the function's shape rather than all of its semantic requirements.

---

## 2. Core syntax

The basic form is:

```siox
where
    TypeOrParameter: Constraint,
    TypeOrParameter: Constraint,
```

Example:

```siox
fn maximum<T>(a: T, b: T) -> T
where
    T: Ord,
{
    if a > b {
        return a;
    }

    return b;
}
```

Multiple bounds may be combined:

```siox
fn process<T>(value: T)
where
    T: Add + Mul + Ord,
{
    ...
}
```

Equivalent inline form:

```siox
fn process<T: Add + Mul + Ord>(value: T) {
    ...
}
```

---

## 3. Relationship with inline bounds

Both forms are valid:

```siox
fn f<T: Add>(x: T) {
    ...
}
```

and:

```siox
fn f<T>(x: T)
where
    T: Add,
{
    ...
}
```

They are semantically equivalent.

A declaration may also use both:

```siox
fn f<T: Add, U>(x: T, y: U)
where
    U: Ord,
{
    ...
}
```

This should be allowed, although style guidance may recommend moving all nontrivial constraints into the `where` clause for consistency.

---

## 4. Functions

Functions may use `where` clauses after the parameter list and return type.

```siox
fn convert<T, U>(value: T) -> U
where
    U: From<T>,
{
    return value;
}
```

Another example:

```siox
fn sum<T>(a: T, b: T) -> T
where
    T: Add,
{
    return a + b;
}
```

The clause appears before the function body.

---

## 5. Structs

Struct declarations may use `where`.

```siox
struct Pair<T, U>
where
    T: Ord,
    U: Clone,
{
    first: T,
    second: U,
}
```

This is equivalent to:

```siox
struct Pair<T: Ord, U: Clone> {
    first: T,
    second: U,
}
```

For a derived struct:

```siox
struct NumericVector<T> : T[]
where
    T: Add + Mul;
```

This creates a nominal array-derived type whose element type must satisfy the declared constraints.

---

## 6. Enums

Enum declarations may also use `where`.

```siox
enum Result<T, E>
where
    T: Clone,
    E: Clone,
{
    Ok,
    Error,
}
```

For enum derivation:

```siox
enum Extended<T> : Base<T>
where
    T: Ord,
{
    Extra,
}
```

The `where` clause constrains when the derived enum type is valid.

---

## 7. Entities

Entities may use `where` clauses for generic elaboration constraints.

```siox
entity Register<T, W: integer>
where
    T: LogicLike,
{
    in clk: Clock;
    in d: T[W];

    out q: T[W];
}
```

Another example:

```siox
entity Adder<T>
where
    T: Add,
{
    in a: T;
    in b: T;

    out result: T;
}
```

The constraints must be satisfied when the entity is specialized or instantiated.

---

## 8. Traits

Traits may use `where` clauses.

```siox
trait Numeric<T>
where
    T: Add + Sub + Mul,
{
    fn zero() -> T;
}
```

A trait may also constrain itself or related types:

```siox
trait OrderedNumeric<T>
where
    T: Numeric + Ord,
{
    fn min(a: T, b: T) -> T;
}
```

---

## 9. Impl blocks

`where` is especially useful for conditional implementations.

```siox
impl Add for Vector<T>
where
    T: Add,
{
    fn add(self, rhs: Vector<T>) -> Vector<T> {
        ...
    }
}
```

This means:

> `Vector<T>` implements `Add` only when `T` implements `Add`.

Another example:

```siox
impl Ord for Pair<T, U>
where
    T: Ord,
    U: Ord,
{
    fn cmp(self, rhs: Pair<T, U>) -> Ordering {
        ...
    }
}
```

This allows trait behavior to be derived conditionally from the capabilities of contained or underlying types.

---

## 10. Generic constraints on derived types

This proposal works naturally with nominal type derivation.

Example:

```siox
struct NumericArray<T> : T[]
where
    T: Add + Mul;
```

Or:

```siox
struct ResolvedArray<T> : T[]
where
    T: Resolve;
```

This makes the type family available only for element types satisfying the required trait.

For example:

```siox
let a: ResolvedArray<Logic>[8];
```

is valid when `Logic: Resolve`.

But:

```siox
let a: ResolvedArray<ULogic>[8];
```

is invalid when `ULogic` does not implement `Resolve`.

---

## 11. `From` constraints

The `where` syntax is useful for generic conversion code.

```siox
fn convert<T, U>(value: T) -> U
where
    U: From<T>,
{
    return value;
}
```

Because Siox uses `From<A> for B` as the authority for total directional conversions, this constraint means:

> `U` must support total conversion from `T`.

This enables generic code such as:

```siox
fn assign_as<T, U>(value: T) -> U
where
    U: From<T>,
{
    return value as U;
}
```

The explicit `as` form and implicit conversion both resolve through the same `From<T>` implementation.

---

## 12. Multiple constraints

Several constraints may be listed:

```siox
fn compute<T, U, V>(a: T, b: U) -> V
where
    T: Add + Mul,
    U: Ord,
    V: From<T> + From<U>,
{
    ...
}
```

Each line ends with a comma.

Trailing commas should be allowed:

```siox
where
    T: Add,
    U: Ord,
```

This matches the formatting style used for fields, parameters, and enum variants.

---

## 13. Constraints on concrete types

A `where` clause should not be limited to generic identifiers.

For example:

```siox
fn drive<T>(value: T)
where
    Logic: Resolve,
{
    ...
}
```

This is legal, although often redundant.

More importantly, future associated-type constraints could use full type expressions:

```siox
where
    T::Output: LogicLike,
```

This is useful once Siox supports associated types or equivalent trait-level type members.

---

## 14. Associated type constraints

If Siox later supports associated types, `where` should support them naturally.

Example:

```siox
fn process<T>(value: T)
where
    T: Iterator,
    T::Item: LogicLike,
{
    ...
}
```

Or:

```siox
impl Add for Matrix<T>
where
    T: Add,
    T::Output: From<T>,
{
    ...
}
```

This proposal does not require associated types to exist immediately.

The grammar should simply avoid preventing such extensions later.

---

## 15. Equality constraints

A future extension may support type equality constraints.

Example:

```siox
where
    T::Output = Logic,
```

or:

```siox
where
    T::Width = 8,
```

However, this is not required for the initial implementation.

The initial scope may restrict `where` clauses to trait bounds only.

---

## 16. Constant and elaboration constraints

Siox entities and type families frequently use compile-time integer parameters.

A future extension could support boolean compile-time predicates:

```siox
entity ShiftRegister<W: integer>
where
    W > 0,
{
    ...
}
```

Or:

```siox
struct ByteArray<N: integer> : uint[8][N]
where
    N > 0;
```

This is attractive for HDL elaboration, but it introduces a different class of constraint from trait bounds.

Recommended implementation order:

1. trait bounds;
2. type relation constraints;
3. compile-time boolean predicates.

The grammar should leave room for all three.

---

## 17. Scope and satisfaction

A `where` clause applies to the declaration immediately preceding it.

Example:

```siox
fn f<T>(x: T)
where
    T: Add,
{
    ...
}
```

Inside the body, the compiler may assume:

```text
T: Add
```

Therefore:

```siox
x + x
```

is valid.

At the call site:

```siox
f(value);
```

the concrete type of `value` must satisfy the bound.

If not, compilation fails.

---

## 18. Conditional impl coherence

For:

```siox
impl Add for Vector<T>
where
    T: Add,
{
    ...
}
```

the implementation exists only for concrete `Vector<T>` instances where the bound is satisfied.

Examples:

```text
Vector<integer>
    Add impl available if integer: Add

Vector<Logic>
    Add impl unavailable unless Logic: Add
```

This should participate in normal trait coherence and overlap checking.

Two implementations that can apply to the same concrete type should be rejected unless Siox later introduces explicit specialization.

Example:

```siox
impl Foo for T
where
    T: Add,
{
    ...
}

impl Foo for T
where
    T: Mul,
{
    ...
}
```

If a type may implement both `Add` and `Mul`, the implementations overlap.

Without specialization rules, this should be an error.

---

## 19. Grammar sketch

A possible grammar is:

```text
where_clause
    := "where" where_predicate ("," where_predicate)* ","?

where_predicate
    := type ":" trait_bound_list

trait_bound_list
    := trait_bound ("+" trait_bound)*

trait_bound
    := type
```

Declarations become:

```text
fn_decl
    := "fn" IDENT generic_params? "(" params? ")" return_type?
       where_clause?
       block

struct_decl
    := "struct" IDENT generic_params?
       struct_base?
       where_clause?
       struct_body_or_semi

enum_decl
    := "enum" IDENT generic_params?
       enum_base?
       where_clause?
       enum_body_or_semi

entity_decl
    := "entity" IDENT generic_params?
       where_clause?
       entity_body

trait_decl
    := "trait" IDENT generic_params?
       where_clause?
       trait_body

impl_decl
    := "impl" impl_head
       where_clause?
       impl_body
```

---

## 20. Placement

Recommended placement is Rust-like: after the declaration header and before the body.

Function:

```siox
fn f<T>(x: T) -> T
where
    T: Add,
{
    ...
}
```

Struct:

```siox
struct S<T>
where
    T: Ord,
{
    value: T,
}
```

Derived struct:

```siox
struct uint : Logic[]
where
    Logic: Resolve;
```

Entity:

```siox
entity Adder<T>
where
    T: Add,
{
    ...
}
```

Impl:

```siox
impl Add for Vector<T>
where
    T: Add,
{
    ...
}
```

The `where` clause always belongs between the complete declaration head and its body or terminating semicolon.

---

## 21. Diagnostics

### Missing bound

```siox
fn add<T>(a: T, b: T) -> T {
    return a + b;
}
```

Possible diagnostic:

```text
error: operator `+` is not available for generic type `T`

help: add the required trait bound:

  fn add<T>(a: T, b: T) -> T
  where
      T: Add,
```

### Unsatisfied bound

```siox
fn f<T>(x: T)
where
    T: Resolve,
{
    ...
}

let x: ULogic;
f(x);
```

Possible diagnostic:

```text
error: `ULogic` does not satisfy required bound `Resolve`

  required by:
      T: Resolve

  note: `Logic` implements `Resolve`, but `ULogic` does not
```

### Invalid predicate target

```siox
where
    42: Add,
```

Possible diagnostic:

```text
error: expected a type in `where` predicate
```

---

## 22. Style guidance

Inline bounds are recommended for simple cases:

```siox
fn f<T: Add>(x: T) {
    ...
}
```

`where` clauses are recommended when:

- more than one generic parameter has bounds;
- a type has several bounds;
- a bound uses a complex type expression;
- an impl is conditionally available;
- keeping the declaration header short improves readability.

Example:

```siox
fn transform<T, U, V>(a: T, b: U) -> V
where
    T: Add + Mul,
    U: Ord,
    V: From<T> + From<U>,
{
    ...
}
```

is preferable to:

```siox
fn transform<T: Add + Mul, U: Ord, V: From<T> + From<U>>(a: T, b: U) -> V {
    ...
}
```

---

## 23. Interaction with Siox type derivation

This proposal complements nominal derivation directly.

Example:

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

A generic function may constrain values to resolved logic-like types:

```siox
fn resolve_pair<T>(a: T, b: T) -> T
where
    T: Resolve,
{
    return a.resolve(b);
}
```

This accepts:

```siox
Logic
```

but rejects:

```siox
ULogic
```

unless `ULogic` also implements `Resolve`.

Similarly:

```siox
struct NumericArray<T> : T[]
where
    T: Add + Mul;
```

makes the interaction between nominal derivation and trait requirements explicit.

---

## 24. Recommended initial implementation scope

The first implementation should support:

```siox
where
    T: Trait,
```

and:

```siox
where
    T: TraitA + TraitB,
    U: TraitC,
```

for:

- functions;
- structs;
- enums;
- entities;
- traits;
- impl blocks.

The initial implementation should enforce:

- every predicate target resolves to a type;
- every named constraint resolves to a trait;
- all bounds must be satisfied at specialization, instantiation, or call sites;
- conditional impls participate in coherence checking;
- inline and `where` bounds are semantically equivalent;
- duplicate bounds may be normalized and ignored or diagnosed as redundant.

Later extensions may add:

- associated type constraints;
- equality constraints;
- compile-time boolean predicates;
- const-value relationships;
- specialization.

---

## 25. Summary

The proposed syntax is:

```siox
fn f<T>(x: T)
where
    T: Add + Ord,
{
    ...
}
```

It applies uniformly to:

```text
fn
struct
enum
entity
trait
impl
```

The core principle is:

> Generic parameters declare what a declaration is parameterized over. A `where` clause declares what must be true about those parameters for the declaration to be valid.

This keeps Siox declarations readable while supporting increasingly expressive generic and trait-based programming.
