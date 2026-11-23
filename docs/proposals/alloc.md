# What siox takes from Rust's `alloc`

Status: **proposed** (2026-11-23). It widens [`vec.md`](vec.md): `Vec<T>` is
one part of a small allocation layer for testbenches, and `string` becomes a
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

## What siox takes

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
3. **A dynamic `string`**: see below.
4. **`format!(..)` returning a `string`**, and `to_string()` for any
   `Display`. Messages are built today only by `print!`/`write!` into a
   formatter.
5. **Conversions between fixed and growable**: `Vec::from([1, 2])` and
   `string` from `Char[N]`. The other way, a length-checked conversion to
   `T[N]`/`Char[N]` that stops the test on a mismatch (#457). `read<string>`
   and `read<Vec<T>>` from files.

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
    `string` itself.
12. **String methods past the basics**: `split`, `trim`, `find`,
    `to_uppercase`, `parse`. Useful for reading stimulus files and checking
    log text.

### Not taken

- **`Box`, `Rc`, `Arc`, `Weak`, `Cow`**: they exist to own or share through
  references, which siox does not have. A recursive structure is a `Vec`
  with indices.
- **`LinkedList`**: rarely the right structure even in Rust; `VecDeque`
  covers queues.
- **`Allocator`, `GlobalAlloc`, `Layout`**: the simulator owns memory; there
  is no allocator to choose.

## Strings: `Char[N]` static, `string` dynamic

Proposed: fixed text is `Char[N]`, and `string` is a growable string.
This mirrors Rust's string literals and `[char; N]` against `String`,
VHDL's `string` against `line`, and SystemVerilog's `string`.

- **`Char[N]`** is fixed text. A literal `"abc"` is a `Char[3]`. It is
  hardware-capable (a constant ROM of characters, a fixed message buffer),
  and its length is part of its type.
- **`string`** grows: `push(c)`, `push_str(s)`, `+`, `format!`, `len()`,
  indexing, comparison, `{}`, `read<string>(path)`. Like `Vec`, it is
  testbench storage with no hardware form (`E-P003` outside a `#[test]`
  entity).
- **Conversions.** A `Char[N]` converts into a `string` where a `string` is
  expected (`let s: string = "abc";`, a `string` parameter given a literal),
  as std's numeric ladder converts upward. The other way is explicit and
  length-checked.
- **Representation.** The runtime already has dynamic strings
  (`sx_runtime_string_*`: UTF-8 text by handle, with length, indexing and
  equality). `string` becomes std's type over them, as `Vec` is over the
  store, so `string` is to `Char` what `Vec<T>` is to `T`.

**Relation to the 2026-11-13 ruling.** That ruling said `string` is a
fixed `Char[N]` everywhere, and #762 and #764 implemented it: a `string`
local with a literal initializer is a `Char[N]`. This proposal reverses that:

- `let s: string = "hello"` becomes a dynamic string, initialized from the
  literal.
- `fn f(s: string)` takes a dynamic string; `fn f<N: integer>(s: Char[N])`
  takes fixed text of any length.
- #737's cases keep working (`echo("hello")`, a callee writing its `string`
  parameter), now through the dynamic store rather than through `Char[N]`
  sizing.
- `string` in hardware becomes an error pointing at `Char[N]`. Today a
  hardware `string` signal must have its width written.

## Order of work

1. Store primitives as intrinsics, and `Vec<T>` rewritten in std over them
   (replaces `vec.md` phase 2).
2. A dynamic `string` over the runtime string store, with the `Char[N]`
   conversion; hardware `string` reported.
3. `format!` and `to_string()`.
4. Element layouts, equality and formatting for the containers (#423, #424,
   #426, #439, #440, #470, #471, #646).
5. `VecDeque<T>`, then `BTreeMap<K, V>`/`BTreeSet<K>`.
6. Fixed/growable conversions and retiring runtime `T[]` (#457, #542);
   algorithms and `BinaryHeap` after.

## Open questions

1. **The string switch.** Adopt it, superseding the 2026-11-13 ruling?
2. **Implicit `Char[N]` → `string`**, or explicit `String::from`/`to_string`
   as Rust requires? (Proposed: implicit, as siox's numeric ladder is.)
3. **Naming.** Keep the lowercase `string` (as `integer` and `unsigned` are),
   or `String` as Rust names it, beside `Vec`?
4. **Map choice.** `BTreeMap` only, or also a `HashMap` with a fixed seed?
   (Proposed: `BTreeMap` only; ordered and reproducible.)
