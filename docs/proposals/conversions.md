# Conversions: `Default`, `From` and `Into`

Status: **mostly implemented** (owner decisions of 2026-10-29). `Default`,
`Into` and the std conversions have landed (sioxc #304, #305); all three
steps of *Order of work* are done, and struct-literal fields are store sites
too (2026-10-30). Port connections are not yet an implicit `Into` site: the
checker does not type a connection against its port today, and Codex's
port-connection type checking (`src/types/connections.rs`, in progress) is
where that site belongs. Landing it finishes this proposal. It settles sioxc #298 and the element-array half
of #301.

Related fixes since (2026-10-30): a vector family or kernel type built from a
struct or unpacked array needs an `impl From` for that pair (#327);
`From<unsigned[8]>` and other sized trait arguments are registered (#334);
`integer(x)` / `natural(x)` use a user `impl From<S> for integer`, as `real(x)`
does (#337); a declared `T::new()` or `impl Default` runs its body rather than
the structural default (#361).

## Three traits, three jobs

| Trait | Spelling | When it runs |
| --- | --- | --- |
| `core::default::Default` | `T::default()`, or `T()` | a declaration without an initializer, or asked for |
| `core::convert::From<S>` | `T(x)` | only at that conversion expression |
| `core::convert::Into<T>` | none | implicitly, where the target type is known |

Rust has the same three (`Default`, `From`, `Into`). siox differs in two
places, both deliberate: `From` is spelled `T(x)` (VHDL's type conversion
form) rather than `T::from(x)`, and `Into` is **not** a blanket impl over
`From`. It is declared separately and makes a conversion implicit; there is
no `x.into()` call.

## 1. `New` is renamed `Default`

```siox
// core/default.siox
pub trait Default {
    fn default() -> Self;
}
attr lang for Default = "default";

impl Default for Logic { fn default() -> Logic { return 'U'; } }
```

- `T::default()` and the short `T()` build a type's default; an uninitialized
  `let p: Phase;` is `Phase::default()` (language §3.29).
- `new` stops being special. `S::new(42)` is an ordinary associated function,
  the Rust convention for a constructor with arguments, and the synthesized
  `T::new()` is gone. A call to `T::new()` on a type that declares no `new`
  is an error whose help names `T::default()`; an `impl New` is an error whose
  help names `Default`.
- Absent an impl, the default stays structural: first enum variant, zero,
  field by field.

## 2. `From` stays explicit

```siox
impl From<Logic[]> for unsigned { fn from(v: Logic[]) -> unsigned { … } }
let u: unsigned[8] = unsigned[8](arr);
```

Unchanged. A `From` never fires on its own (§3.17: storing a value takes an
explicit conversion).

## 3. `Into` is implicit, by declaration

```siox
// core/convert.siox
pub trait Into<T> {
    fn into(self) -> T;
}

// std::bits
impl Into<Logic[]> for unsigned {
    fn into(self) -> Logic[] { … }      // written directly
}
impl Into<unsigned> for Logic[] {
    fn into(self) -> unsigned { return unsigned(self); }   // or through a From
}
```

`impl Into<T> for S` makes `S` convert to `T` wherever a value of `S` stands
in a position whose type is `T`:

- a `let` initializer, an assignment, a port connection;
- a function argument and a returned value;
- a field of a struct literal.

Its body is the conversion. It may call a `From`, but needs none: `From` and
`Into` are independent, and a pair may have either, both or neither.

It does **not** apply where nothing names a single target:

- operator operands: `u + arr` dispatches `Add` on the operands' own types,
  so the operand is converted explicitly, `u + unsigned[8](arr)`;
- method receivers, `match` scrutinees, conditions;
- chains: one implicit step at most;
- the numeric promotion inside arithmetic (§3.17's one exception) is separate
  and unchanged.

### Which pairs std declares

`Into` both ways between `Logic[]` and each vector family that is a newtype
over it:

```siox
pub struct unsigned(Logic[]);
pub struct signed(Logic[]);
pub struct ufixed<W, F>(Logic[W - F - 1 .. 0 - F]);
pub struct sfixed<W, F>(Logic[W - F - 1 .. 0 - F]);
pub struct float<W, M>(Logic[W - M - 1 .. 0 - M]);
```

Same representation, equal lengths (the strict width rule still applies), so
the conversion is a lossless copy by position — the left element to `'left`
— and the target's parameters place any binary point or exponent field:
`"00011000"` is 24 as an `unsigned[8]` and 1.5 as a `ufixed<8, 4>`. This is
VHDL's `to_slv` / `to_ufixed(slv, l, r)` / `to_float(slv, e, f)`, made
implicit.

Conversions between families stay explicit `From`: `ufixed` and `unsigned`,
`ufixed` and `float`, one fixed format and another. Those scale, round or
saturate.

### Assignment between families

With `Into` in place, the checker stops treating nominal families as
interchangeable ("families do not gate assignment"): `Logic[N]` and
`unsigned[N]` are different types, and an assignment between them is legal
because of the `Into` impl, and lowers through its body. Today it typechecks
and copies raw storage, which gives wrong values in a testbench and does not
lower in a design (#298, #301):

```siox
let arr: Logic[8] = "00011000";
let f: ufixed<8, 4> = arr;    // printed 0, not 1.5
```

`unsigned` and `signed` keep their current mutual rule; whether they convert
implicitly is a separate question.

## Order of work

1. Rename `New` to `Default` (core, std, compiler lang item, tests, corpus,
   language.md §3.29), with the migration errors above.
2. Add `Into` to core and the implicit conversion at the listed sites in the
   checker and in lowering.
3. Make nominal families gate assignment, add std's `Into` impls, and close
   #298 and #301.

## Non-goals

- A blanket `impl<T, U: From<T>> Into<U> for T`: it would make every `From`
  implicit.
- `x.into()`: every place it would be written already converts implicitly.
- `let c = Cell { … }` (a `let` whose type comes from its value): a separate
  question.
