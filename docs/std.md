# The siox standard library

The standard library lives in `std/` as ordinary siox source, loaded
transitively from `--std <dir>` (default `./std`): `using std::logic::{...}`
parses `<dir>/logic.siox`, and imports bind to real `pub` declarations (a
bad import is a hard error, `E-P011`).

Compiler *mechanisms* are never declared in std: operator overloading
(`impl "+" for T`), literal suffixes/prefixes (`impl Suffix for T`), and the
operator set itself are built in (spec 3.24/3.25) — std and user code just
write the impls, no trait declaration or import needed.

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
| `std::logic`  | std.standard + ieee.std_logic_1164 | `Bit`, `Logic`, `Bool`, `Clock` enums; `LOW`/`HIGH`; Logic truth tables |
| `std::bits`   | ieee.numeric_std                 | `uint[N]` / `int[N]` surface (docs; ops intrinsic for now) |
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
pub enum Logic { '0', '1', 'Z', 'X' }
pub enum Bool  { false, true }
pub enum Clock { '0', '1' }

pub const LOW: Bit = '0';
pub const HIGH: Bit = '1';
```

- `Bit` — two-valued scalar (VHDL `bit`). Keeps the built-in two-value
  operators; a valid condition via `Boolean`.
- `Logic` — four-valued scalar (VHDL `std_ulogic`, reduced): `'Z'`
  high-impedance, `'X'` unknown. **The word operators `and or xor nand nor
  xnor` are implemented here as truth tables** with unknown propagation: a
  dominant operand decides (`'0' and 'X' = '0'`, `'1' or 'X' = '1'`),
  otherwise the result is `'X'`. Not a condition — compare explicitly
  (`if rst == '1'`), because `'X'`/`'Z'` truth is ambiguous.
- `Bool` — condition results (VHDL `boolean`), an ordinary enum.
- `Clock` — a Bit carrying clock intent. Edge detection is built-in syntax:
  `clk::rising` / `clk::falling` (the `rising_edge(clk)` analogue).

## `std::bits`

`uint[N]` (VHDL `unsigned`) and `int[N]` (`signed`) are *derived* Logic
vectors with numeric interpretation, but accept `integer` on assignment
(`let x: uint[8] = 42;`). Arithmetic, bitwise logic, comparisons, shifts,
slices (`x[7..4]`) and concatenation (`{hi, lo}`) are compiler-implemented
as part of the type-kernel shim; they move here as operator impls when
vector operators land. Bit-string literals `x"AB"` / `b"0101"` are sized
`uint` constants (spec 3.24).

## `std::ops`

```siox
pub enum Ordering { Less, Equal, Greater }
pub trait Boolean { fn as_bool(self) -> integer; }
```

**`Ordering`** — the result of a three-way `impl "<=>" for T` (spaceship):
one impl derives all of `< <= > >= == !=` (spec 3.25).

**`Boolean`** — a type usable as a condition provides `as_bool` returning
the kernel truth type `integer` (1 true, 0 false), applied only in condition
position. `Bit`/`Bool` opt in; `Logic` deliberately does not.

The overloading *mechanisms* — operator strings
(`+ - * / << >> == != < <= > >= and or xor nand nor xnor not`), `Suffix`,
`Prefix` — are compiler built-ins, not std declarations. Impls are inlined
at lowering as pure expression trees; mixed operand types overload by the
rhs parameter type, and `impl "+" for integer` catches literal left operands
(`10 + 5i`). Each fn of an `impl Suffix for T` defines the literal suffix of
its name (`10ns` → `Time::ns(10)`); two loaded types defining one suffix is
an ambiguity error. See spec 3.24/3.25 and notes/literal-suffixes.md.

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
pub attr test: Bool for entity;     // discovered by `siox test`
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

## What is deliberately absent (and why)

- **`textio` / file IO** — no string/file model in the Phase 1 simulator.
- **`math_real`** — needs `real` arithmetic; Phase 2 (analogue) territory.
- **`resize` / `to_integer` conversions** — need free or associated
  functions callable in expressions; today fns exist only as trait-impl
  bodies for inlining.
- **9-value `std_ulogic`** (`'U' 'W' 'L' 'H' '-'`) — the 4-value reduction
  covers Phase-1 simulation; widening `Logic` is a std-only change once
  operators fully live here.

Examples exercising the library through real imports: `std_test.siox`
(every module), `logic_test.siox` (X-propagation), `complex_test.siox`
(`10 + 5i`), plus the counter/register/mux/FSM/struct/array tests.
