# A growable vector: `std::vec::Vec<T>`

Status: **proposed**, with a proof of concept (2026-11-16): `std::vec::Vec<T>`
names the simulator's growable store, has `len`, `is_empty`, `last`, `pop` and
`clear` written in siox, and is rejected in hardware. Phases 2–5 below are not
started. [`alloc.md`](alloc.md) (2026-11-23) widens this into the allocation
layer siox takes from Rust's `alloc` crate, and proposes phase 2 as store
primitives with `Vec` written entirely in std.

Ruling it builds on (2026-11-05): a growable array is its own type, as Rust's
`Vec<T>`; `T[]` is never dynamic. Ten open issues are runtime-array bugs that
this proposal moves onto the new type: #423, #424, #426, #439, #440, #457,
#470, #471, #542, #646.

## Problem

`T[]` means two things today.

- In hardware, and in a type, it is an **unconstrained array**: its length
  comes from context, as `unsigned` is `Logic[]` and a `string` is `Char[]`.
- In a testbench `let`, it is a **runtime array**: a handle to storage the
  simulator owns, which grows with `xs.push(v)` and whose length is known
  only at run time (#95–#98, 2026-10-14, never reviewed).

The second meaning was an unreviewed default. It is also where the open
array bugs live: elements wider than a word, struct and tuple elements,
nesting, formatting, attributes, and copying between a fixed array and a
runtime one all go through a representation the type system cannot describe,
since nothing in `T[]` says "growable".

## How Rust does it

Rust keeps three things apart:

| Rust | What it is | Length |
| ---- | ---------- | ------ |
| `[T; N]` | a fixed array, a value | in the type |
| `[T]` (behind `&`) | a slice: a view of contiguous elements | at run time, fixed for the view |
| `Vec<T>` | a growable, owned buffer | at run time, changes |

`Vec<T>` is a library type, not a language one:

- **Representation.** A pointer, a capacity and a length (`RawVec<T>` plus
  `len`). `push` grows the capacity **by doubling** (amortized O(1)), from a
  small minimum; `with_capacity` and `reserve` let a caller grow it ahead.
  Dropping the vector frees the buffer; cloning copies every element.
- **Slices do the rest.** `Vec<T>` dereferences to `[T]`, so indexing
  (`v[i]`, panicking out of range), `len`, iteration, `contains`, sorting and
  searching are slice methods every contiguous store shares.
- **Its own API** is about growth: `new`, `with_capacity`, `push`,
  `pop -> Option<T>`, `insert`, `remove`, `swap_remove`, `truncate`, `clear`,
  `extend`, `capacity`, `reserve`, and the `vec![..]` macro.
- **Equality and formatting** are element-wise (`PartialEq` when `T` has it,
  `Debug` prints `[1, 2, 3]`).

## Proposal

### The type

`std::vec::Vec<T>`, a std type like `std::fs`'s functions: a service of the
simulator, not part of the core language. std marks it with
`attr lang for Vec = "vec";` so the compiler knows its representation, and
writes its methods in siox.

```siox
use std::vec::Vec;

#[test] entity Tb {}
impl Tb {
    process {
        let v: Vec<integer>;            // empty
        for i in 0..4 { v.push(i * 10); }
        print!("{} {}\n", v.len(), v.last());   // 5 40
        let top: integer = v.pop();     // 40
        for x in v { print!("{} ", x); }
        v.clear();
    }
}
```

### Representation

A vector is a handle to simulator storage: element words, a length and a
capacity. The runtime already has this store (`sx_runtime_array_*` in
`runtime/process.c`), and it already grows as Rust's does, by doubling its
capacity when full. Storage is freed when the test ends, as every runtime allocation is.

### Testbench only

A vector's length changes at run time, so it has no hardware form. A `Vec`
signal or port in a non-test entity is `E-P003` where it is declared ("a
`Vec` grows at run time, so it has no hardware form"), pointing at `T[N]`. A
bounded hardware FIFO is a different, future type (as Rust's
`heapless::Vec<T, N>` is to `Vec<T>`).

### Semantics

siox values are copied, not moved, and parameters are copy-in/copy-out
(#94). A vector follows the same rules: assignment copies the elements, and
a callee that pushes onto a `Vec` parameter changes the caller's vector.

### API

Phase 1 (the proof of concept) has what the store already supports, plus
methods written over it:

| | |
| --- | --- |
| built in | `let v: Vec<T>;` (empty), `v.push(x)`, `v[i]` (checked at run time), `v'length`, `for x in v` |
| std methods | `len()`, `is_empty()`, `last()`, `pop()`, `clear()` |

Later phases add Rust's remaining growth API (`insert`, `remove`,
`swap_remove`, `truncate`, `extend`, `contains` for `T: Eq`, `with_capacity`
and `capacity`), `Eq` and `Display`/`{:?}` element-wise, and a constructor
from a fixed array, `Vec::from([1, 2, 3])`.

`pop` returns `T`, not `Option<T>`: siox has no payload enums yet
(`payload-enums.md`). Popping an empty vector stops the test, as an
out-of-range index does. When payload enums land, `pop` and a `get` return
`Option<T>` as Rust's do.

### What happens to `T[]`

`T[]` keeps only its first meaning: an unconstrained array whose length
comes from context. A testbench `let xs: T[] = [1, 2]` is a `T[2]`, as a
`string` local is a `Char[N]` since #762. Growth (`push`) and run-time
length belong to `Vec<T>`, and `read<T[]>(path)` becomes
`read<Vec<T>>(path)`.

## Phases

Each phase is its own PR, in this order.

1. **Name the store** (the proof of concept). `Vec<T>` is checked as the
   runtime store of `T`, its methods dispatch through `method_head` and
   `process_type_key`, and a hardware `Vec` is reported. `T[]` runtime arrays
   keep working.
2. **A nominal type** (superseded by `alloc.md`'s store primitives: `Vec<T>`
   becomes a std struct over intrinsics, nominal by construction).
   `Vec<T>` stops being the same checked type as `T[]`:
   `Ty::Named(vec, [T])`, which carries its argument since the generics work
   (#739). Runtime operations are keyed on it, and `push` on a `T[]` warns,
   pointing at `Vec<T>`.
3. **Element layouts.** A vector's elements take `T`'s own layout: structs,
   tuples, fixed arrays, strings and nested `Vec<Vec<T>>` (#423, #424, #426,
   #440, #470).
4. **Formatting, attributes, conversions.** `{}`/`{:?}` print `[a, b]`
   (#439, #646). `'low`, `'high` and `'ascending` are 0, `len - 1` and true
   (#471). Copying into and out of fixed arrays checks the length (#457), and
   a `-> Logic[]` result is a fixed array, not a handle (#542).
5. **Retire runtime `T[]`.** `push` on `T[]` becomes an error. The corpus
   (siox-tests) and std (`fs::read`) move to `Vec<T>`, and `docs/language.md`
   and `docs/std.md` in siox-paper describe it.

## Open questions

1. **Prelude.** Rust's prelude has `Vec`. Should siox's? (Proposed: yes, in
   phase 2, once it is its own type.)
2. **Slices.** siox has no references, so `v[a..b]` cannot be a view. It
   copies into a new vector, or a fixed array when the bounds are constant.
   Is that enough, or does a slice type belong in this proposal?
3. **`vec![..]`.** A macro (siox has `macro_rules`-style built-ins) or only
   `Vec::from([..])`?
4. **Capacity.** Expose `with_capacity`/`capacity`, or keep growth invisible
   as in a simulator it costs nothing observable?

## Proof of concept

- `std/vec.siox`: `struct Vec<T>`, `attr lang for Vec = "vec"`, and
  `len`/`is_empty`/`last`/`pop`/`clear`.
- `src/types/ast_types.rs`: `Vec<T>` is checked as the runtime store of `T`.
- `src/types/keys.rs`, `src/types/members.rs`: a store's methods resolve to
  `Vec`'s (`vec_key`, `method_head`).
- `src/types/items.rs`: a `Vec` outside a `#[test]` entity is reported, and
  isn't required to have fields to be stored.
- `src/ir/lower/source_processes.rs`: `Vec<T>` lowers as the runtime store,
  and its methods dispatch through `process_type_key`.
- `tests/vec_poc.rs`: the example above, and the hardware rejection.
