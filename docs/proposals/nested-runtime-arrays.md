# Runtime arrays of runtime arrays

Status: **proposal; needs the owner's syntax decision**. Nothing is
implemented. The runtime side is small: a nested element is a 64-bit handle,
and a copy recurses one level per nesting.

## Problem

A testbench can hold a runtime array (`T[]`: `read<T[]>`, `push`, element
writes) of any packed element, of fixed arrays (`unsigned[8][2][]`) and of
character enums (`Logic[]`). It cannot hold an array whose elements are
themselves runtime-sized: rows of differing length, the lines of a file as
strings, a list of stimulus vectors built with `push`.

The obvious spelling already means something else. An index on an
unconstrained array **fills its hole** instead of nesting (§ types:
`string[5]` is `Char[5]`, since `type string = Char[]`). So `integer[][]` is
`integer[]`, and `string[]` is `Char[]`. That rule is load-bearing for
`string[N]` and for nominal families (`struct F(Logic[])`, then `F[8]`), and
changing it changes existing programs.

## Options

1. **Parenthesized element type.** `(integer[])[]`, `(string)[]`. Brackets
   on a parenthesized type always nest. It is explicit and changes nothing
   that exists. It needs a parser change (a type in parentheses, which tuples
   already parse as `(A, B)`; a one-element form without a comma would be the
   grouping).
2. **A distinct unsized-array spelling for the outer level.** For example,
   `integer[..][]` or `integer[*]`. This reads as a new kind of array, which
   is what it is (a handle), but it adds a second spelling for "unsized".
3. **Nest only when the inner hole cannot be filled.** `T[][]` nests, while
   `T[][5]` still fills the hole. This is the least new syntax, but the meaning
   of `[]` would depend on what follows it, and `string[]` would silently
   change from `Char[]` to an array of strings.
4. **Wrap the inner array in a struct.** `struct Row { xs: integer[] }` then
   `Row[]`. No syntax change, but it needs runtime arrays of non-packed
   structs, which is a larger runtime change than the nesting itself.

Recommendation: **option 1**. It is the only option that leaves every
existing type meaning what it does today, and `(string)[]` reads naturally.

## Semantics, under any spelling

These follow #95 and #97, so they apply whatever syntax is chosen:

- An element is a runtime array, held as a handle. `rows[i]` reads it,
  `rows[i][j] = v` writes into it in place, and `rows[i].push(v)` grows it,
  storing the handle back when the row was empty.
- Arrays are values. `rows.push(xs)` and `let r = rows[i]` copy, and
  `ys = rows` copies every level (`sx_runtime_array_copy` gains a depth).
- Strings as elements (`(string)[]`) share their immutable handles.
- `read` does not produce nested arrays. A future `read<(string)[]>`
  could split a file into lines; that is a separate decision.
