# The siox language at a glance

A quick tour of what siox can express today. This is the overview; the
authority for exact syntax and semantics is [spec.md](spec.md), and the
standard library is catalogued in [std.md](std.md).

## Structure

- **Entities and impls.** An `entity` declares ports; an `impl` gives its
  behaviour. Entities take parameters (`Counter<W: integer>`), instantiate
  sub-entities, and connect ports — including **struct/bus bundles** and
  **`inout` tristate nets** that resolve parallel drivers.
- **Instance hierarchy.** A design is a tree of instances; each lowers into its
  own signals with connections wired as drivers.
- **Generate loops.** `for i in a..b { let stage = Sub { .. } }` unrolls to one
  instance per iteration, with the loop index substituted into connections.

## Logic

- **Combinational vs. sequential are kept distinct.** A continuous assignment
  (`count = value;`) is a wire; an event block (`if clk::rising { … }`) updates
  only on the edge. Edge and history queries — `clk::rising`, `x::event`,
  `x::old` — are first-class.
- **Four-value logic.** `Logic` carries `'0'/'1'/'Z'/'X'` with the std_logic
  truth tables and parallel-driver resolution; `Bit` is the two-value scalar.
  There is no dedicated clock type — any `Logic`/`Bit` signal is a clock when
  edge detection (`clk::rising`/`clk::falling`) is applied to it.
- **`'c'` is a value, `"c"` is a string.** A character literal (`'0'`, `'Z'`,
  an enum variant like `'a'`) is a single `Bit`/`Logic`/`Char`/enum value; a
  double-quoted `"…"` is a `string` (a `Char` array) and never stands in for one
  scalar — so an enum array is written `{'a', 'b'}`, not `"ab"`. Bit vectors use
  the bit-string literal `b"0101"` / `x"AB"`.
- **Numeric vectors.** `uint[N]` / `int[N]` are library types built on `Logic`
  vectors; signedness lives in the operator impls (int's arithmetic shift,
  signed division and comparison), not in a type flag.
- **Bit operations.** Slices (`a[7..4]`, direction-aware), concatenation
  (`{hi, lo}`, also as an assignment target), and bit-pattern `match`
  (`b"01??"` with `?` don't-cares).
- **Array literals.** `[a, b, c]` builds an array value one element at a time
  (`table = [10, 20, 30, 40]`), distinct from `{..}` concatenation.

## Types and generics

- Generics with trait bounds and `where` clauses.
- **Rust-style operator traits** — `impl Add for T`, one `impl Ord`
  (`cmp -> Ordering`) deriving all six comparisons.
- **Methods** — `recv.method(args)` on a value's inherent or trait impl
  (`impl T { fn m(self, ..) }`); value-returning methods inline into an
  expression, statement methods (`s.send(v)`) inline as drivers on the
  receiver's fields.
- **Derived nominal types** — `enum B : A` / `struct B : A`, with total
  derivation conversions synthesised automatically.
- `#[…]` attributes, including type-targeted ones.
- System attributes for metadata: `x::width`, `xs::len`.

## Testbenches and simulation

- **`#[test]` entities** are testbenches: instantiate a DUT, drive it, assert on
  it. `sioxc test` runs them like `cargo test`.
- **Timing.** `await 10ns` (advance time), `await clk::rising` (edge),
  `await cond` (condition); background clocks via `clk = not clk after 5ns;`.
  Multiple clocks interleave on one event wheel with real timestamps.
- **Reporting.** `assert!`, `warn!`, `print!` (with symbolic enum/logic
  rendering), `stop!`/`finish!`.
- **I/O and FFI.** `extern "C"` functions, and file reads (`read`,
  `read_to_string`, `exists`) resolved relative to the source file.
- **Waveforms.** VCD output, with four-value logic dumped as native `0/1/z/x`.

## Diagnostics

Every diagnostic has a stable code. Beyond errors, the compiler lints for
possible latches, unused imports, unresolved multiple drivers, combinational
loops (a signal that feeds itself with no register in the path), and
non-exhaustive / unreachable match arms. Type errors carry targeted fix-it
help — e.g. a string literal used where a single value is wanted (`Logic = "0"`)
points at the character literal `'0'`, and one used where a vector is wanted
points at the bit-string literal `b"…"`.

## Not yet (by design, this phase)

No synthesis, no analogue/mixed-signal, no schematic layer — those are Phases
2–3 ([roadmap.md](roadmap.md)). Phase-2 syntax (`domain`, `across`/`through`,
`::ddt`) is deliberately rejected with an error rather than silently accepted.
