# Runtime-sized arrays in testbenches

Status: **proposal**. Nothing here is implemented.

## Problem

A testbench can hold one kind of value whose size is known only when it runs:
the unconstrained `string` returned by `read<string>(path)`. Everything else is
sized at compile time. Reading a stimulus file of unknown length therefore
means choosing a capacity up front:

```siox
let samples: unsigned[16][4096] = read<unsigned[16]>("stimulus.bin"); // zero-fills, or fails if longer
```

TODO.md (LLVM, Host-service extensions) asks for "general runtime-owned dynamic
arrays", with allocation, persistent state and host contact behind the fixed
runtime ABI, and LLVM emitting value semantics only.

## Today's string is the template

A runtime string is already a runtime-owned array:

| piece | today, for `Char` |
| --- | --- |
| type | `string`, an unconstrained `Char[]` |
| layout | `LayoutKind::Array { range: None, element: Char }` |
| value | a 64-bit handle into the runtime's table (`sx_runtime_string`) |
| created by | `read<string>(path)` (`sx_runtime_read_utf8`) |
| `'length` | `ProcessHostValueOp::StringLength` |
| `s[i]` | `StringIndex`, checked against the loaded length |
| `for c in s` | length, then checked index per element |
| `print!`, debugger | runtime services over the handle |

## Decision

Generalize that one representation from `Char` to any packed element type.
Do not add a second mechanism.

```siox
let samples: unsigned[16][] = read<unsigned[16][]>("stimulus.bin");
for sample in samples { din = sample; await 10ns; }
assert!(samples'length > 0, "stimulus present");
let first: unsigned[16] = samples[0];
```

- **Type.** `T[]` with no size, for a packed `T` (`Bit`, `unsigned[N]`,
  `signed[N]`, `integer`, `real`, fixed and float formats, packed structs).
  It is allowed only in a `#[test]` implementation. In hardware it stays an
  error, because a hardware array has a size.
- **Creation.** `read<T[]>(path)` reads the whole file. Each element consumes
  `ceil(T'length / 8)` bytes, as a fixed `read<T>` does today, and a trailing
  partial element is an error. `read<string>` is this with UTF-8 decoding.
- **Use.** `'length`, `xs[i]` (checked; out of range fails the test with the
  index and length), `for x in xs`, `print!("{:?}", xs)`, and passing an
  array to a function parameter of type `T[]`. Assigning one copies the
  handle, so both names share the same elements. That is safe because these
  arrays are read-only in the first version.
- **Not in the first version:** writing elements, growing (`push`), creating
  an empty array or one from a literal, arrays of arrays, `Logic` elements
  (companion planes), and passing to hardware ports. Each is its own later
  change.

## Shape

- **IR.** Rename the string operations to array ones (`StringLength` becomes
  `ArrayLength`, `StringIndex` becomes `ArrayIndex`) and give the index result
  the element's layout and width. `ProcessHostValueOp::ReadArray` replaces the
  string-only read where `T` is not `Char`. The layout check for a handle
  (`runtime_handle_layout`) accepts any packed element.
- **Runtime.** The string table becomes an array table whose entries hold the
  element width in bits, the count, and the packed words. A string keeps its
  UTF-8 copy for printing and paths. `sx_runtime_array_index(handle, index,
  words)` copies one element's words into a caller buffer. Element widths up
  to 64 bits return in a register.
- **LLVM.** Index and length emit calls to the runtime, as strings do today.
  No allocation is emitted in the design object.
- **Lifetime.** Handles live until the test ends, as strings do. There is no
  reference counting in the first version.

## Delivery

1. Generalize the runtime table and the host operations, with strings still
   passing every existing test (no behaviour change).
2. Checker: `T[]` in testbenches, `read<T[]>`, `'length`, indexing, iteration
   and parameters; hardware use rejected.
3. Lowering and LLVM for packed elements up to 64 bits, then multiword ones.
4. Tests: every packed element kind; empty files; a trailing partial element;
   an out-of-range index; iteration with `await` in the body; passing to a
   function; `{:?}` printing; default and `bitpack`, ordinary and `--debug`.

## Open questions

1. **Spelling.** `T[]` (consistent with `Logic[]` meaning "unconstrained" in
   std signatures) or a named type such as `Vec<T>`?
2. **Assignment.** Should it share (a handle copy, as proposed), or copy the
   elements once writes exist?
3. **Text formats.** Should `read<T[]>` stay binary-only, with hex or decimal
   text loaders left to libraries as `std::fs` says today?
