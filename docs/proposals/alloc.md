# What siox takes from Rust's `alloc`

Status: **proposed** (2026-11-23); **the string switch is adopted**
(2026-11-23, see "Strings" below). It widens [`vec.md`](vec.md): `Vec<T>` is
one part of a small allocation layer for testbenches, and `String` is a
dynamic string, with fixed text as `Char[N]`.

## Rust's `alloc` crate

`alloc` is everything in Rust's standard library that needs heap memory but
no operating system. `std` re-exports it; `core` sits below it with no heap
at all. None of it is a compiler intrinsic: every type is ordinary library
code on a few primitives, raw pointers and an allocator.

| Module | What it holds |
| ------ | ------------- |
| `alloc` | `Allocator`, `GlobalAlloc`, `Layout`: the allocation interface. `RawVec` (private) owns a buffer: pointer, capacity, growth. |
| `vec` | `Vec<T>`: `RawVec` plus a length. Derefs to the slice `[T]` for indexing, iteration and algorithms. |
| `string` | `String`: a `Vec<u8>` that is always valid UTF-8, and `ToString`. Derefs to `str`. |
| `collections` | `VecDeque` (a ring buffer), `BinaryHeap` (a heap over a `Vec`), `BTreeMap`/`BTreeSet` (ordered maps and sets), `LinkedList`. (`HashMap` is in `std`, since its default hasher needs OS randomness.) |
| `boxed`, `rc`, `sync` | `Box<T>` (one owner), `Rc<T>`/`Arc<T>` (shared, counted), `Weak<T>`. |
| `borrow` | `Cow<T>` (borrowed or owned), `ToOwned`. |
| `fmt`, `str`, `slice` | `format!` returning a `String`; the allocating methods of `str` and slices (`to_vec`, `concat`, `join`, `repeat`, `to_uppercase`, stable `sort`). |
| `ffi` | `CString`: a NUL-terminated string for C. |

The layering is what matters for siox:

1. **Primitives**: memory from an allocator, and pointer reads and writes.
2. **One buffer type** (`RawVec`): allocate, grow by doubling, free.
3. **Containers over it**, all library code: `Vec`, then `String` (a
   `Vec<u8>`), `VecDeque`, `BinaryHeap` (both over a buffer), and the
   B-trees.
4. **Ownership and sharing** (`Box`, `Rc`, `Arc`, `Cow`): only meaningful
   with references.

## Where siox stands

siox has no pointers, no references, and copies values. A testbench's
runtime storage is freed when the test ends. So everything about lifetimes
and sharing in `alloc` has no job here.

What testbenches do need is what testbench languages already have:

| SystemVerilog | VHDL | Rust (`alloc`) |
| ------------- | ---- | -------------- |
| dynamic array `int d[]` | access types | `Vec<T>` |
| queue `int q[$]` | access types | `VecDeque<T>` |
| associative array `int m[int]` | (none built in) | `BTreeMap<K, V>` |
| `string` | `line` (`textio`) | `String` |

Uses include stimulus and response queues, scoreboards, sparse memory
models, expected-value tables, coverage bins and log messages. siox has
`Vec` (phase 1) and nothing else of this list.

## `std::alloc`: the module

Ruled 2026-11-23: std has an `alloc` module that acts as Rust's `alloc`
crate does; the only thing left out is pointers. Its layout is Rust's, and
std re-exports it as Rust's std does (`std::vec::Vec` is
`std::alloc::vec::Vec`):

| Module | Holds | std re-export |
| ------ | ----- | ------------- |
| `std::alloc::vec` | `Vec<T>` (`vec!` later) | `std::vec` |
| `std::alloc::string` | `String`, `ToString` | `std::string` |
| `std::alloc::collections` | `VecDeque`, `LinkedList`, `BTreeMap`, `BTreeSet`, `BinaryHeap` | `std::collections` |
| `std::alloc::fmt` | `format`, `format!` | `std::fmt` |
| `std::alloc::slice`, `std::alloc::str` | the allocating methods of arrays and text (`to_vec`, `concat`, `join`, `repeat`, `to_uppercase`, stable `sort`) | |

Left out, because they are pointers or exist only to manage them: Rust's
`alloc::alloc` (`Allocator`, `GlobalAlloc`, `Layout`, raw `alloc`/
`dealloc`), `boxed`, `rc`, `sync` (`Arc`), `task` (`Wake` over `Arc`),
`borrow` (`Cow`, which borrows), `ffi` (`CString`, a pointer for C), and the
raw-part methods of the kept types (`as_ptr`, `from_raw_parts`,
`into_raw_parts`, `leak`, `spare_capacity_mut`). A method that hands out a
reference in Rust (`get_mut`, `iter_mut`, `entry`) takes the copy-in/
copy-out form siox already uses for parameters, or waits for a later ruling.

Landed:
- the layout, with `Vec<T>` and `String` (step 1 below) moved under it and
  re-exported (#782);
- structs holding `Vec`/`String` fields, copied as values (#783), and
  generic structs over them (`struct Q<T> { items: Vec<T> }`, #784): the
  shape every collection here is written in;
- `std::alloc::collections::VecDeque<T>`, re-exported as
  `std::collections::VecDeque`: `push_back`, `push_front`, `pop_front`,
  `pop_back`, `front`, `back`, `get`, `len`, `is_empty`, `clear` (#785);
- `BTreeMap<K, V>` (sorted keys and values; `insert`, `get`,
  `contains_key`, `remove`, `keys`, `values`, `len`, `is_empty`, `clear`),
  and `Vec`'s `insert`, `remove` and `truncate`.

## What siox takes

The lists below were written before that ruling; it moves `LinkedList`
from "not taken" to taken, and makes the rest of Rust's API on the kept
types the target, not only the listed methods.

### Needed

1. **A store primitive**: siox's `RawVec`. The simulator owns memory, so the
   primitive is a handle to runtime storage plus intrinsics: create, length,
   read element `i`, write element `i`, resize. The runtime already has one
   (`sx_runtime_array_*`); it becomes an intrinsic interface, and the
   containers are std siox over it, as Rust's are library code over
   `RawVec`. This replaces `vec.md`'s built-in `push`: growth policy, bounds
   checks and element layout move into std, where the parked struct-element
   and width bugs (#424, #426, #470) are std's to get right.
2. **`Vec<T>`**, as `vec.md`.
3. **A dynamic `String`**: see below.
4. **`format!(..)` returning a `String`**, and `to_string()` for any
   `Display`. Messages are built today only by `print!`/`write!` into a
   formatter.
5. **Conversions between fixed and growable**: `Vec::from([1, 2])` and
   `String` from `Char[N]`. For vectors the other way, a length-checked conversion to
   `T[N]` that stops the test on a mismatch (#457); `read<String>` and
   `read<Vec<T>>` from files.

### Beneficial

6. **`VecDeque<T>`**: SystemVerilog's queue. `push_back`, `pop_front`,
   `front`, `len`: the shape of every stimulus and expected-response queue.
   It is a ring buffer over the store primitive, written in std.
7. **`BTreeMap<K, V>` and `BTreeSet<K>`**: SystemVerilog's associative
   array, for sparse memory models (`addr -> data`), scoreboards keyed by ID,
   and coverage bins. Ordered rather than hashed: iteration order is
   deterministic, so a simulation reproduces exactly. Keys need `Ord`.
8. **Slice-style algorithms on `Vec`**: `sort`, `contains`, `reverse`,
   `dedup`, and `join`/`concat` for strings.
9. **Element-wise `Eq` and `{}`/`{:?}`** for the containers (#439, #646).

### Nice to have

10. **`BinaryHeap<T>`**: a priority queue (event models, schedulers), in std
    over `Vec`.
11. **C strings for `extern "C"`**: either `CString`, or FFI marshalling a
    `String` itself.
12. **String methods past the basics**: `split`, `trim`, `find`,
    `to_uppercase`, `parse`. Useful for reading stimulus files and checking
    log text.

### Not taken

- **`Box`, `Rc`, `Arc`, `Weak`, `Cow`**: they exist to own or share through
  references, which siox does not have. A recursive structure is a `Vec`
  with indices.
- **`Allocator`, `GlobalAlloc`, `Layout`**: the simulator owns memory; there
  is no allocator to choose.

## Strings: `Char[N]` static, `String` dynamic

**Adopted** (2026-11-23): fixed text is `Char[N]`, and `String` (Rust's
name, beside `Vec`; not the lowercase `string`) is a growable string. A
`Char[N]` can be assigned to a `String`, never the reverse.
This mirrors Rust's string literals and `[char; N]` against `String`,
VHDL's `string` against `line`, and SystemVerilog's `string`.

- **`Char[N]`** is fixed text. A literal `"abc"` is a `Char[3]`. It is
  hardware-capable (a constant ROM of characters, a fixed message buffer),
  and its length is part of its type.
- **`String`** grows: `push(c)`, `push_str(s)`, `+`, `format!`, `len()`,
  indexing, comparison, `{}`, `read<String>(path)`. Like `Vec`, it is
  testbench storage with no hardware form (`E-P003` outside a `#[test]`
  entity).
- **Conversions.** A `Char[N]` converts into a `String` where a `String` is
  expected (`let s: String = "abc";`, a `String` parameter given a literal),
  as std's numeric ladder converts upward. A `String` never converts into a
  `Char[N]` by assignment.
- **Representation.** The runtime already has dynamic strings
  (`sx_runtime_string_*`: UTF-8 text by handle, with length, indexing and
  equality). `String` is std's type over them, as `Vec` is over the store,
  so `String` is to `Char` what `Vec<T>` is to `T`.

**Relation to the 2026-11-13 ruling.** That ruling said `string` is a
fixed `Char[N]` everywhere, and #762 and #764 implemented it: a `string`
local with a literal initializer is a `Char[N]`. This reverses that:

- The lowercase `string` (core's `type string = Char[]`) goes away. Fixed
  text is written `Char[N]` (or `Char[]` where its length comes from
  context, as for compile-time text: attribute values, `CustomOperator<op:
  Char[], ..>`); growable text is `String`.
- `let s: String = "hello"` is a dynamic string, initialized from the
  literal.
- `fn f(s: String)` takes a dynamic string (a literal argument converts);
  `fn f<N: integer>(s: Char[N])` takes fixed text of any length.
- #737's cases keep working (`echo("hello")`, a callee writing its
  parameter), now through the dynamic store.
- `String` in hardware is an error pointing at `Char[N]`.

Steps, each its own PR:

1. `std::string::String` (in the prelude) beside the existing `string`:
   a lang type over the runtime string store, `Char[N]` assignable into it,
   `len`/`is_empty`/indexing/`==`/`{}`, rejected in hardware. Nothing
   existing changes. **Landed** (`std::alloc::string`,
   `tests/string_type.rs`).
2. core and std move to `String` or `Char[..]`, with the compiler's own
   references to `string`.
3. The tests and the corpus (siox-tests) move, then `string` is removed.

## Order of work

1. Store primitives as intrinsics, and `Vec<T>` rewritten in std over them
   (replaces `vec.md` phase 2).
2. `String` over the runtime string store, with the `Char[N]` conversion;
   hardware `String` reported (the three steps above).
3. `format!` and `to_string()`, returning `String`.
4. Element layouts, equality and formatting for the containers (#423, #424,
   #426, #439, #440, #470, #471, #646).
5. `VecDeque<T>`, then `BTreeMap<K, V>`/`BTreeSet<K>`.
6. Fixed/growable conversions and retiring runtime `T[]` (#457, #542);
   algorithms and `BinaryHeap` after.

## Decisions

1. **The string switch:** adopted (2026-11-23).
2. **Conversion:** a `Char[N]` assigns into a `String`; never the reverse.
3. **Naming:** `String`, not `string`.
4. **Map choice:** `BTreeMap` only (ordered and reproducible), unless a
   later need for hashing shows up.
