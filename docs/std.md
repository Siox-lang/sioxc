# The siox standard library

> Target picture + build order: [proposals/std-buildout.md](proposals/std-buildout.md).


The standard library lives in `std/` as ordinary siox source, loaded
transitively from `--std <dir>` (default `./std`): `use std::logic::{...}`
parses `<dir>/logic.siox`, and imports bind to real `pub` declarations (a
bad import is a hard error, `E-P011`).

Beneath it sits **`core`**: the part of the compiler reachable through the
language, laid out like rustc's `core`. Its sources
are in the repository's `core/` directory, compiled into `sioxc`, so
`core::…` never reads `--std`. It holds what the compiler gives meaning to:
`Bool`, `string` and `integer`'s methods (`core::primitive`), the hook traits
(`core::ops`), the comparison traits `Eq`/`Ord` and `Ordering` (`core::cmp`), `From` (`core::convert`), `New` (`core::default`),
the built-in macros and `Severity` (`core::macros`), and the attributes the
compiler reads, `precedence` and `lang` (`core::attrs`). Each declaration
tells the compiler its role with a lang item, `attr lang for Add =
"add";`, and the compiler finds its hooks by role, never by path; only
`core` and `std` may bind `lang`. `core::prelude` reaches every module, and
`std` re-exports `core`'s modules as rustc's does (`std::cmp::Ordering` is
`core::cmp::Ordering`).

Directives — `#[test]`, `#[allow(..)]`, `#[warn(..)]`, `#[deny(..)]`,
`#[forbid(..)]` — are not declared anywhere: like rustc's, they are built into
the compiler.

Every standard operator is a named `core::ops` trait (`impl Add<Rhs, Out>
for T` with `fn add`; `Sub`, `Mul`, `Div`, `Shl`, `Shr`, `And`, `Or`, `Not`,
`Neg`),
and a user operator is `CustomOperator<"symbol", Rhs, Out>` with `fn apply`,
binding its precedence inside its impl (`attr precedence = N;`), discovered
before expression parsing.

Design stance (see the spec's "type kernel"): the compiler provides exactly
three base types — `integer`, `real`, and `Char` (a non-numeric character
*symbol*: numbers exist only through an encoding table in std, and UTF-8 is
only a source/IO encoding) — plus the type machinery; everything else is
declared here, the way VHDL declares `bit`, `boolean` and `std_ulogic` in
`std.standard` / `std_logic_1164` rather than in the compiler. `string` is
`Char[N]` with elaboration-inferred length.
Where the compiler still special-cases a name for operator semantics, that
is a documented shim, and the declaration here is canonical.

## Module map

| siox module   | VHDL analogue                    | Contents |
| ------------- | -------------------------------- | -------- |
| `core::prelude` | (implicit `std.standard`)        | always loaded, built in: `Bool`, `string`, `Boolean`, `Range`, indexing, `Resolve`, `Eq`, `Ord`, `Ordering`, `New`, `From`, `precedence`, the built-in macros |
| `core::primitive` | std.standard `boolean`, `string` | `Bool`, `string = Char[]` |
| `core::ops`   | (operators are VHDL functions)     | `Add`, `Sub`, `Mul`, `Div`, `Shl`, `Shr`, `And`, `Or`, `Not`, `Neg`, `CustomOperator`, `Prefix`, `Suffix`, `Index`, `IndexAssign`, `Range`, `Boolean`, `Resolve`, `LogicEncoding` |
| `core::cmp`   |                                    | `Eq`, `Ord` (the comparisons, returning `Bool`), `Ordering` |
| `core::convert` |                                  | `From` |
| `core::default` |                                  | `New` |
| `core::macros` | `assert ... severity`             | `assert!`, `warn!`, `print!`, `error!`, `Severity` |
| `core::attrs` | (attributes; VHDL has none)        | `precedence`, `lang`: the attributes the compiler reads |
| `std::prelude`| (implicit `std.standard`)          | auto-loaded `Bit`/`Logic`, `unsigned`/`signed`/`sext`, `time`/`frequency` |
| `std::primitive`, `std::cmp`, `std::convert`, `std::default` | | re-export the `core` modules of the same name |
| `std::logic`  | std.standard + ieee.std_logic_1164 | `Bit`, `ULogic`, `Logic` and their operators; resolution and logic tables |
| `std::bits`   | ieee.numeric_std                 | `unsigned[N]` / `signed[N]` operators as `Add`/`Sub`/… impls, `Eq`/`Ord` (signed compares signed) |
| `std::ops`    | (operators are functions in VHDL packages) | re-exports `core::ops` |
| `std::math`   | ieee.math_complex                | `Complex` over `real`, `+`/`-` impls, the `i` suffix |
| `std::numeric`| natural/positive subtypes        | ranged integers: `Byte`, `Short`, `Int`, `Long`, `Natural`, `Positive` |
| `std::text`   | `'pos`/`'val`                    | encoding tables `Unicode`/`Ascii` |
| `std::sim`    | std.standard `time`              | `time`, `frequency` + unit suffixes; FS..MS constants |
| `std::sync`   | (vendor CDC macros)              | `Sync2`, `ResetSync`, `EdgeDetect`, `PulseSync` |
| `std::fixed`  | ieee.fixed_pkg                   | `ufixed<W, F>`, `sfixed<W, F>` (W bits, F of them fraction), constructors from `real`/`integer` (`ufixed<8, 4>(2.5)`), `.to_real()` |
| `std::float`  | ieee.float_pkg                   | `float<W, M>` (`float<32, 23>` is binary32), constructors from `real`/`integer` (`float<32, 23>(1.5)`), `.to_real()`, `+ - *`, comparisons, `is_nan` … |
| `std::fs`     | textio / impure host I/O         | typed `read<T>` construction and `exists` fixture probes |
| `std::attrs`  | (attributes; VHDL has none)      | base metadata: `keep`, `top`, `clock`, `library`, `name` |

## `std::logic`

```siox
pub enum Bit   { '0', '1' }
pub enum ULogic { 'U', 'X', '0', '1', 'Z', 'W', 'L', 'H', '-' }
pub enum Logic(ULogic);  // resolved nominal type over the same values
pub enum Bool  { false, true }

pub const LOW: Bit = '0';
pub const HIGH: Bit = '1';
```

- `Bit` — two-valued scalar (VHDL `bit`). Keeps the built-in two-value
  operators; a valid condition via `Boolean`.
- `Logic` — nine-valued scalar (VHDL `std_ulogic`, IEEE 1076-2019): `'U'`
  uninitialized, `'X'` strong unknown, `'0'/'1'` forcing, `'Z'` high-impedance,
  `'W'` weak unknown, `'L'/'H'` weak, `'-'` don't-care. Core `and`/`or`/`not`
  and custom `xor`/`nand`/`nor`/`xnor` are the full `std_logic_1164` truth
  tables (implemented here as impls; verified cell-for-cell against a reference
  simulator), and `impl Resolve` is the `resolved` resolution function. Not a
  condition — compare explicitly (`if rst == '1'`), because unknown truth is
  ambiguous.
- `Bool` — condition results (VHDL `boolean`), an ordinary enum.

`LogicEncoding` is a visible semantic contract, not an empty representation
marker. Its `to_bool`, `is_binary`, `is_high_impedance`, and `to_x01` methods
are the VHDL-directed source of packed value/metavalue classification.
Elaboration evaluates them for every variant and evaluates the ordinary
operator bodies into per-element truth tables stored in `Design`. Backends
therefore know neither ULogic symbols nor their discriminants. The declaration
uses IEEE order while explicit discriminants keep the packed ABI stable.

There is no dedicated clock type: any `Logic`/`Bit` signal is a clock when edge
detection is applied to it — `clk.rising()` / `clk.falling()` (the
`rising_edge(clk)` analogue), built-in syntax over `'event`/`'old`.

## `std::bits`

`unsigned[N]` (VHDL `unsigned`) and `signed[N]` (`signed`) are *derived* Logic
vectors with numeric interpretation, and accept `integer` on assignment
(`let x: unsigned[8] = 42;`). Only the kernel types (`integer`/`real`) have
built-in operators; unsigned/signed get theirs **here** as `Add`/`Sub`/… impls:
`"+"`/`"-"`/`"*"`/`"/"`/`"<<"`/`">>"` over the kernel word operators (wrap at
the stored width), and `signed` gets a **sign-aware `impl Ord<signed> for signed`** — signed
comparison is library source, not compiler code (`-1 < 1` on signed[8], while
unsigned compares unsigned); `std::math`'s `abs`/`rem`/`mod` work on both. Inside an operator impl, operands read as kernel
words and `self'length` gives the operand's bit width. Remaining kernel
territory: slices (`x[7..4]`), concatenation (`{hi, lo}`), widths, and
literal typing; signed `Div` and arithmetic `Shr` are library source too (magnitude divide + sign restore; top-bit mask fill), built on `resize` and `self'length`.
Radix bit-string literals `x"AB"` / `o"17"` are sized `unsigned` constants,
declared by `impl Prefix<"x", _> for unsigned` in `std::bits` (spec 3.24); a plain string
`"0101"` covers the binary case with no prefix. A file that never imports
`std::bits` falls back to kernel word semantics.

## `core::ops` (re-exported by `std::ops`)

```siox
pub trait Boolean { fn as_bool(self) -> Bool; }
pub trait New { fn new() -> Self; }
pub trait From { fn from(value: Self) -> Self; }
pub trait Resolve { fn resolve(self, rhs: Self) -> Self; }
```

**Comparisons** are `core::cmp`'s `Eq<Rhs>` (`eq`, and `ne` by default) and
`Ord<Rhs>` (`lt`, `le`, and `gt`/`ge` by default), each returning `Bool`
(spec 3.25). `Ordering` is an ordinary enum beside them.

**`Boolean`** — a type usable as a condition provides `as_bool` returning the
system `Bool` type (`true`/`false`), applied only in condition position.
`Bit`/`Bool` opt in; `Logic` deliberately does not.

`Resolve` and the core `and`/`or`/`not` operators have constrained blanket
implementations for `T[]`. Nominal array newtypes forward them when their
element type implements the scalar contract. Consequently `unsigned` and
`signed` reuse `Logic` resolution and truth tables without duplicate forwarding
impls; their arithmetic and signed interpretation remain nominal impls in
`std::bits`.

The operator traits, `Suffix`, and `Prefix` are compiler bootstraps. A custom
operator's symbol and precedence, and every impl's input and output, are
std/user declarations. Impls are inlined
at lowering as shared value graphs; mixed operand types overload by the
`Input` parameter type, and `impl Add<Complex, _> for integer`
catches literal left operands (`10 + 5i`). An `impl Suffix<"ns", _> for T` defines the literal suffix named
by its symbol argument, its `suffix` method inlined at the use site (`10ns` →
a `time`); two loaded types defining one suffix is an ambiguity error. See
spec 3.24/3.25.

## `std::math`

```siox
pub struct Complex { re: real, im: real }
```

Complex over the **reals** (f64 in simulation): `+`/`-` component-wise,
`integer` promotion both ways, and the `i` suffix, so `10 + 5i` works as
written. Real arithmetic uses the float operators in the IR; integer
literals coerce (`.re = 10` stores 10.0).

The math functions and constants:

| function | meaning |
| --- | --- |
| `abs(x)` | magnitude |
| `min(a, b)`, `max(a, b)` | the smaller / larger |
| `rem(a, m)` | remainder with the dividend's sign (VHDL `rem`, Rust `%`) |
| `mod(a, m)` | remainder with the divisor's sign (VHDL `mod`) |
| `sqrt`, `sin`, `cos`, `exp`, `log`, `pow`, `floor`, `ceil`, `round` | on `real`, from the C math library |
| `PI`, `E` | `real` constants |

`abs`, `min`, `max`, `rem` and `mod` are generic over the numeric types —
`integer`, `real`, `signed`, `unsigned`, the fixed formats and `float`
(`abs`, `min`, `max`) — and inline with the argument type's own operators.
`abs` of a `float` is `-x` below zero, so `abs(-0.0)` stays `-0.0` (equal to
`+0.0`) and a NaN keeps its sign.

## `std::sim`

```siox
pub struct time(integer);     // nominal integer, stored in femtoseconds
pub struct frequency(real);   // nominal real, stored in hertz
```

Unit suffixes `fs ps ns us ms` construct `time`; `Hz kHz MHz GHz` construct
`frequency`, including fractional values such as `2.5MHz`. The simulator uses
the 1 fs base tick (also the waveform timescale), and both nominal types implement
`Eq` and `Ord` so all six comparisons are available without discarding their
unit identity. `FS..MS` remain raw integer multipliers.
Timing is the built-in `await`: `await 10ns;` advances time, `await
clk.rising();` waits for an edge, and `await cond;` waits for a condition.
`await 10ns` also works in bare files through a fixed fallback table typed as
`integer`. `stop()` and `finish()` are runtime-provided.

## `std::rand`

Native testbenches provide deterministic `rand()`, `uniform()`, `seed(value)`,
and inclusive `randint(left, right)`. `randint` accepts ascending or descending
bounds. Its full unsigned 64-bit domain is valid and consumes exactly one raw
draw, without forming a wrapping zero modulo.

## `std::text`

```siox
pub type string = Char[];
pub struct Unicode {}   // Unicode::code(c) -> integer, Unicode::char(n) -> Char
pub struct Ascii {}     // Ascii::code(c) -> integer, -1 outside 7-bit ASCII
pub fn unicode(c: Char) -> integer;   // shorthand for Unicode::code
pub fn char_of(n: integer) -> Char;   // shorthand for Unicode::char
pub fn ascii(c: Char) -> integer;     // shorthand for Ascii::code
```

`Char` has no number of its own: a number exists only relative to an encoding
table, so conversion names the table (VHDL's `'pos`/`'val`, made explicit).
`Char` stores Unicode code points, so `Unicode` is the identity and `Ascii` is
its 7-bit prefix. The tables are structs with associated functions, so other
encodings can be added the same way. A `string` takes its length from an
explicit size (`string[5]`) or from the literal that initializes it.

## `std::numeric`

Ranged integers (spec 3.26): each stores in the smallest width covering its
range. Constants outside it are compile errors, and a value that leaves the
range while simulating is reported at run time with the signal's path.

```siox
pub type Byte = integer<0..255>;
pub type Short = integer<-32768..32767>;
pub type Int = integer<-2147483648..2147483647>;
pub type Long = integer<-9223372036854775808..9223372036854775807>;
pub type Natural = integer<0..9223372036854775807>;
pub type Positive = integer<1..9223372036854775807>;
```

## `std::attrs` and `core::attrs`

The base metadata (spec 3.5) nearly every flow needs, each with a default so
a read (`probe'keep`) always answers. The compiler preserves it without
giving it semantics; `precedence`, which the parser does read, lives in
`core::attrs` with `lang`:

```siox
pub attr keep: Bool for let, port = false;  // keep through optimization
pub attr top: Bool for entity = false;      // the design's top entity, for tools
pub attr clock: Bool for port = false;      // this port is a clock input
pub attr library: string for entity = "";   // a foreign entity's library
pub attr name: string for entity = "";      // a foreign entity's name there
```

Bind them with `attr keep for probe = true;`. `top` tells tools such as
Vivado or a cocotb flow which entity is the design's top; it does not select
`sioxc`'s root, which stays structural or explicit through `--top`.

Vendor settings — RAM and ROM styles, FSM encodings, fan-out limits, debug
marks, I/O standards, pin assignments — are not std: they belong in vendor
packages, in each vendor's own spelling (`attr vivado::ram_style for …`).

The directives — `#[test]` and the lint levels `#[allow]`, `#[warn]`,
`#[deny]`, `#[forbid]` — are not attributes and are declared nowhere: like
rustc's, they are built into the compiler, which also owns the lint names
(spec §3.5a). A module's own `attr test` is metadata and never makes a test.

## `std::fixed`

Fixed-point numbers after VHDL-2008's `fixed_pkg`. The parameters are the
format: `ufixed<W, F>` has `W` bits, `F` of them fraction, so `ufixed<8, 4>` is
4.4 and `sfixed<16, 8>` is 8.8 two's complement. Inside, the word is indexed
as VHDL's is (`ufixed<8, 4>` runs 3..-4), so an impl reads `self'high + 1`
integer and `-self'low` fraction bits. Both are newtypes over `Logic[]`, like
`unsigned` and `signed`.

```siox
use std::fixed::{ufixed, sfixed};

let gain: ufixed<8, 4> = ufixed<8, 4>(2.5);         // the word 40
let error: sfixed<16, 8> = sfixed<16, 8>(0.0 - 0.75);
let scaled: ufixed<8, 4>;
scaled = (gain + gain) * gain;                      // formats carry through
let r: real = gain.to_real();                       // 2.5
```

- `+`, `-`, `*` between operands of one format give that format. A sum wraps
  on overflow, as `unsigned` does; a product drops its extra fraction bits
  rounding toward minus infinity (VHDL's truncate).
- `<`, `<=`, `>`, `>=`, `==`, `!=` come from each type's `Eq`/`Ord`;
  `sfixed` compares signed; `std::math::abs` gives the magnitude in the same
  format.
- The constructor `ufixed<W, F>(x)` / `sfixed<W, F>(x)` takes a `real` or an
  `integer` to that format, rounding to nearest (ties away from zero) and
  saturating; it works in hardware too. `x.to_real()` goes back.
- `/` keeps the format, rounding toward minus infinity as `*` does; a quotient
  by zero is zero, as for `unsigned`.
- Between formats the constructor resizes: `ufixed<12, 6>(x)` from another
  `ufixed`, `sfixed<8, 4>(y)` from another `sfixed`, rounding to the nearest
  step (ties away from zero) and saturating, VHDL's `resize` defaults.
- Not yet: the other resize styles (wrap, truncate).

## `std::float`

IEEE-754 floating point after VHDL-2008's `float_pkg`. The parameters are the
format: `float<W, M>` has `W` bits, `M` of them mantissa, so the exponent has
`W - M - 1`. Inside, the word is indexed as VHDL's is — the sign at the top
index, then `x'high` exponent bits, then `-x'low` mantissa bits.

```siox
use std::float::float;

type binary16 = float<16, 10>;    // an alias names a format, and builds it
let x: float<32, 23>;             // binary32
let y: float<32, 23>;
x = float<32, 23>(1.5);           // the word 0x3FC00000
y = float<32, 23>(0.0 - 2.25);
r = x * y + x;                    // rounds to nearest, ties to even
let v: real = r.to_real();
```

- `+`, `-`, `*` and the six comparisons (`-0` equals `+0`). Zero, infinity
  and NaN follow IEEE-754: `0 * inf` and `inf - inf` are NaN, overflow is
  infinite, and a NaN is unordered — every comparison with one is false
  except `!=`, so `x != x` holds exactly for a NaN.
- `-x` (`Neg`, the IEEE sign flip), `x.is_nan()`, `x.is_infinite()`,
  `x.is_zero()`, `x.to_real()`; `std::math`'s `abs`, `min`, `max`. The
  constructor `float<W, M>(x)` takes a `real` or an `integer` to the format,
  rounding to nearest even.
- Subnormals are flushed to zero on input and output, VHDL's
  `denormalize => false` and the usual FPGA choice.
- Everything is siox source over the packed word: no compiler intrinsic.
- Operators execute in both test processes and ordinary hardware entities.
  Source-defined function locals retain shared Process value IDs; the old
  hardware tree-inlining size restriction is gone. The hardware and procedural
  binary32 conformance tests check the same 44 operand pairs for `*`, `+`, `-`.
- Not yet: division, square root, subnormals, other rounding modes,
  conversions to and from fixed point.

## `std::sync`

Clock-domain crossing and reset helpers, all `Bit`-typed. Synchronizer flops
are bound `keep`.

| entity | ports | behaviour |
| --- | --- | --- |
| `Sync2` | `clk`, `d` in; `q` out | two-flop synchronizer: `d` appears on `q` two `clk` edges later |
| `ResetSync` | `clk`, `rst_in` in; `rst_out` out | active-high reset: asserts at once, releases two `clk` edges after `rst_in` falls |
| `EdgeDetect` | `clk`, `d` in; `rise`, `fall` out | one-cycle pulses when `d`, already in `clk`'s domain, changes |
| `PulseSync` | `src_clk`, `pulse_in`, `dst_clk` in; `pulse_out` out | single-cycle pulses across domains, through a toggle and `Sync2`; pulses at least three destination cycles apart |

```siox
use std::sync::Sync2;
let ready_sync: Sync2 = { .clk = clk, .d = ready_from_other_domain, .q = ready };
```

A multi-bit value must not cross bit by bit through `Sync2`: its bits can
land in different cycles. Use a handshake or a Gray-coded FIFO.

## `core::macros`

`assert!(cond, "msg")` fails a test, `warn!(cond, "msg")` reports and counts
without failing, `print!` formats a line, and `error!("msg")` fails
unconditionally. All four are macros declared in `core::macros` over the
compiler primitive `builtin # …`, so a failure names the line of the call.
The module also carries the severity ladder (VHDL `severity_level`) for when
assertions grow a severity argument:

```siox
pub enum Severity { Note, Warning, Error, Failure }
```

## Current boundaries

- **File services execute at the owning phase.** Hardware/top initializers bake
  ROM images during compilation; native `#[test]` locals open fixtures at run
  time and own fixed binary arrays or dynamically sized Unicode strings. The
  surface is one typed construct: `read<string>` decodes UTF-8,
  `read<integer>` reads raw binary, and packed numeric `read<T>` constructs `T`
  from that integer representation.
- **Real math exists** through `std::math` and the native C math ABI. `real`
  remains IEEE binary64; quad precision is a future LLVM/runtime capability,
  not an advertised compiler feature.
- **Conversions and resize exist** for the numeric families used by the
  corpus.
- **Vector metavalues propagate** through storage, logical operations,
  arithmetic poisoning, comparisons, port connections, and wide values. The
  companion representation scales beyond one ABI word.
- **Reusable hardware models remain intentionally small.** Counters,
  synchronizers, memories, FIFOs, and stream adapters are the next library
  build-out, tracked under the `std` heading in [`TODO.md`](../TODO.md).

Examples exercising the library through real imports: `std_test.siox`
(every module), `logic_test.siox` (X-propagation), `complex_test.siox`
(`10 + 5i`), plus the counter/register/mux/FSM/struct/array tests.
