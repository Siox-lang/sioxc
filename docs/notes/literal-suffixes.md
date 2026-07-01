# Literal suffixes and prefixes as traits

Status: **approved design** (supersedes the earlier `pub suffix` item
proposal). A type opts into literal suffixes by implementing the `Suffix`
trait; **each fn's name is the suffix it defines**, and the literal desugars
to that fn, inlined at lowering exactly like operator impls (spec 3.25).
`Prefix` does the same for string prefixes (`x"123ABC"`).

```siox
// std/ops.siox — deliberately empty: each impl brings its own fn names.
pub trait Suffix {}
pub trait Prefix {}
```

## How a suffix is defined

```siox
impl Suffix for Complex {
    fn i(v: integer) -> Complex {
        return Complex { .re = 0, .im = v };
    }
}
```

`5i` → `Complex::i(5)`, so `let z: Complex = 5i;` typechecks with no
annotation gymnastics: the literal's type is the fn's return type.

## Multiple suffixes, one type

```siox
pub struct Time { fs: integer }

impl Suffix for Time {
    fn fs(v: integer) -> Time { return Time { .fs = v }; }
    fn ps(v: integer) -> Time { return Time { .fs = v * 1000 }; }
    fn ns(v: integer) -> Time { return Time { .fs = v * 1000000 }; }
    fn us(v: integer) -> Time { return Time { .fs = v * 1000000000 }; }
    fn ms(v: integer) -> Time { return Time { .fs = v * 1000000000000 }; }
}
```

`10ns`, `10us`, `10ms` all produce `Time` values on the femtosecond base.

## Multiple types, side by side

Different types define different suffixes; the suffix picks the type:

```siox
pub struct Freq { hz: integer }

impl Suffix for Freq {
    fn Hz(v: integer)  -> Freq { return Freq { .hz = v }; }
    fn kHz(v: integer) -> Freq { return Freq { .hz = v * 1000 }; }
    fn MHz(v: integer) -> Freq { return Freq { .hz = v * 1000000 }; }
    fn GHz(v: integer) -> Freq { return Freq { .hz = v * 1000000000 }; }
}

let period: Time = 10ns;     // Time::ns
let clock:  Freq = 100MHz;   // Freq::MHz
let z: Complex = 5i;         // Complex::i
```

And user types extend the same mechanism — nothing about it is std-only:

```siox
pub struct Voltage { uv: integer }

impl Suffix for Voltage {
    fn mV(v: integer) -> Voltage { return Voltage { .uv = v * 1000 }; }
    fn V(v: integer)  -> Voltage { return Voltage { .uv = v * 1000000 }; }
}

let vdd: Voltage = 3300mV;
```

## Collisions

The suffix table is flat across loaded modules. Two types defining the
*same* suffix is an ambiguity error at the use site:

```siox
impl Suffix for Time  { fn s(v: integer) -> Time  { ... } }  // seconds
impl Suffix for Score { fn s(v: integer) -> Score { ... } }  // points

let x = 5s;   // error: suffix `s` is ambiguous: Time::s, Score::s
```

The fix is import discipline (don't load both) — same rule as any duplicate
name. Within one module the duplicate-item check catches it at declaration.

## Prefixes

Same shape for string prefixes; the fn takes the quoted digits:

```siox
impl Prefix for uint {
    fn x(digits: string) -> uint;   // hex — body intrinsic for now
    fn b(digits: string) -> uint;   // binary
}
```

Caveat: the *bodies* of `x`/`b` need string iteration in const context,
which siox can't express yet — so `x"..."`/`b"..."` keep their intrinsic
evaluation and this impl is their declared home (the same shim pattern as
`Bit`/`Logic`, retired when const string ops exist).

## Compatibility and the fixed table

The compiler's fixed fs/Hz scale table (spec 3.24) remains the *fallback*
for files that don't load a std `Suffix` impl covering the suffix — `wait
10ns` keeps working in bare files, typed as `integer`. When a loaded module
defines the suffix, the trait impl wins and the literal takes its type.

## Still open: `10 + 5i`

`5i` is a `Complex` but `10` is an `integer`, so the literal `10 + 5i` needs
**mixed-operand operator impls** — `fn apply(self, rhs: integer) -> Complex`
under the same `trait "+"`, with impl lookup keyed by (op, lhs, rhs) types.
That is the next increment after suffix traits land.
