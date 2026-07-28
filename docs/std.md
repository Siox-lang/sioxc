# The siox standard library

> Target picture + build order: [proposals/std-buildout.md](proposals/std-buildout.md).


The standard library lives in `std/` as ordinary siox source, loaded
transitively from `--std <dir>` (default `./std`): `using std::logic::{...}`
parses `<dir>/logic.siox`, and imports bind to real `pub` declarations (a
bad import is a hard error, `E-P011`).

The compiler bootstraps only the single `Operator` name. Its public contract
lives in `std::ops`; every operator is an
`Operator<"symbol", Input, Output>` implementation, and a user operator
(a non-standard symbol) carries an attributed precedence discovered before
expression parsing.

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
| `std::prelude`| (implicit `std.standard`)          | auto-loaded: `Bit`/`Logic`/`Bool`, `unsigned`/`signed`, `Boolean`/`Ordering`, `string`, `Time`/`Freq` |
| `std::logic`  | std.standard + ieee.std_logic_1164 | `Bit`, `Logic`, `Bool` enums; `LOW`/`HIGH`; Logic truth tables |
| `std::bits`   | ieee.numeric_std                 | `unsigned[N]` / `signed[N]` operators as `Operator` impls (incl. `signed`'s signed `<=>`) |
| `std::ops`    | (operators are functions in VHDL packages) | the `Boolean` condition trait |
| `std::math`   | ieee.math_complex                | `Complex` over `real`, `+`/`-` impls, the `i` suffix |
| `std::numeric`| natural/positive subtypes        | ranged integers: `Byte`, `Short`, `Int`, `Long`, `Natural`, `Positive` |
| `std::text`   | std.standard `string` + `'pos`/`'val` | `string = Char[]`; encoding tables (`Unicode`/`Ascii`) planned |
| `std::sim`    | std.standard `time`              | `Time`, `Freq` + unit suffixes; FS..MS constants |
| `std::attrs`  | (attributes; VHDL has none)      | `top`, `test`, `keep`, `library`, `name` |
| `std::assert` | `assert ... severity` levels     | `Severity` |

## `std::logic`

```siox
pub enum Bit   { '0', '1' }
pub enum Logic { 'U', 'X', '0', '1', 'Z', 'W', 'L', 'H', '-' }  // IEEE std_ulogic
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

There is no dedicated clock type: any `Logic`/`Bit` signal is a clock when edge
detection is applied to it — `clk.rising()` / `clk.falling()` (the
`rising_edge(clk)` analogue), built-in syntax over `'event`/`'old`.

## `std::bits`

`unsigned[N]` (VHDL `unsigned`) and `signed[N]` (`signed`) are *derived* Logic
vectors with numeric interpretation, and accept `integer` on assignment
(`let x: unsigned[8] = 42;`). Only the kernel types (`integer`/`real`) have
built-in operators; unsigned/signed get theirs **here** as `Operator` impls:
`"+"`/`"-"`/`"*"`/`"/"`/`"<<"`/`">>"` over the kernel word operators (wrap at
the stored width), and `signed` gets a **sign-aware `Operator<"<=>", signed, Ordering>`** — signed
comparison is library source, not compiler code (`-1 < 1` on signed[8], while
unsigned compares unsigned). Inside an operator impl, operands read as kernel
words and `self'length` gives the operand's bit width. Remaining kernel
territory: slices (`x[7..4]`), concatenation (`{hi, lo}`), widths, and
literal typing; signed `Div` and arithmetic `Shr` are library source too (magnitude divide + sign restore; top-bit mask fill), built on `resize` and `self'length`.
Radix bit-string literals `x"AB"` / `o"17"` are sized `unsigned` constants,
declared by `impl Prefix<"x", _> for unsigned` in `std::bits` (spec 3.24); a plain string
`"0101"` covers the binary case with no prefix. A file that never imports
`std::bits` falls back to kernel word semantics.

## `std::ops`

```siox
pub enum Ordering { Less, Equal, Greater }
pub trait Boolean { fn as_bool(self) -> Bool; }
```

**`Ordering`** — the result of `impl Operator<"<=>", T, Ordering>` (the `apply`
three-way compare): one impl derives all of `< <= > >= == !=` (spec 3.25).

**`Boolean`** — a type usable as a condition provides `as_bool` returning the
system `Bool` type (`true`/`false`), applied only in condition position.
`Bit`/`Bool` opt in; `Logic` deliberately does not.

`Operator`, `Suffix`, and `Prefix` are compiler bootstraps. Operator symbol,
precedence, input, and output are std/user declarations. Impls are inlined
at lowering as pure expression trees; mixed operand types overload by the
`Input` parameter type, and `impl Operator<"+", Complex, _> for integer`
catches literal left operands (`10 + 5i`). An `impl Suffix<"ns", _> for T` defines the literal suffix named
by its symbol argument, its `suffix` method inlined at the use site (`10ns` →
a `Time`); two loaded types defining one suffix is an ambiguity error. See
spec 3.24/3.25.

## `std::math`

```siox
pub struct Complex { re: real, im: real }
```

Complex over the **reals** (f64 in simulation): `+`/`-` component-wise,
`integer` promotion both ways, and the `i` suffix, so `10 + 5i` works as
written. Real arithmetic uses the float operators in the IR; integer
literals coerce (`.re = 10` stores 10.0).

## `std::sim`

```siox
pub struct Time { fs: integer }   // 10ns  -> Time { .fs = 10_000_000 }
pub struct Freq { hz: integer }   // 100MHz -> Freq { .hz = 100_000_000 }
```

Unit suffixes `fs ps ns us ms` and `Hz kHz MHz GHz` on the 1 fs base tick
(the VCD timescale), plus raw `FS..MS` integer multipliers. `wait`/`tick`
stimulus control is built-in simulator syntax; `wait 10ns` also works in
bare files through a fixed fallback table typed as `integer`.

## `std::numeric`

Ranged integers (spec 3.26): each stores in the smallest width covering its
range; constants outside it are compile errors.

```siox
pub using Byte = integer<0..255>;
pub using Short = integer<-32768..32767>;
pub using Int = integer<-2147483648..2147483647>;
pub using Long = integer<-9223372036854775808..9223372036854775807>;
pub using Natural = integer<0..9223372036854775807>;
pub using Positive = integer<1..9223372036854775807>;
```

## `std::attrs`

The five system metadata attributes (spec 3.5), declared here and mirrored
by a compiler seed so bare files keep working:

```siox
pub attr top: Bool for entity;      // elaboration root
pub attr test: Bool for entity;     // discovered by `sioxc --test`
pub attr keep: Bool for let, port;  // keep through optimization
pub attr library: string for entity;
pub attr name: string for entity;
```

## `std::assert`

`assert!(cond, "msg")` is built-in simulator syntax; this module carries the
severity ladder (VHDL `severity_level`) for when assertions grow a severity
argument:

```siox
pub enum Severity { Note, Warning, Error, Failure }
```

## Current boundaries

- **File services exist**, but `read`/`read_to_string` fixtures are consumed
  while building a native test executable and baked into it. Runtime mutable
  file I/O still needs owned dynamic strings/arrays.
- **Real math exists** through `std::math` and the native C math ABI. `real`
  remains IEEE binary64; quad precision is a separate LLVM/runtime feature.
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
