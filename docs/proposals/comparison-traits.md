# Comparison traits, and `abs`/`rem`/`mod` as functions

Status: **proposal**, decided; being implemented.

## Problem

Today one three-way `impl Operator<"<=>", T, Ordering>` derives all six
comparisons: `a < b` lowers to `(a <=> b) == Ordering::Less`. That has two
costs:

- **Equality drags in order.** A type that can only be compared for equality
  (a complex number, a bus struct, a protocol enum) must still invent a
  `Less`/`Greater` answer.
- **No "unordered".** IEEE-754 says every comparison with a NaN is false, but
  `<=>` must answer Less, Equal or Greater, so `std::float` orders NaN above
  every number. Hardware float comparators follow IEEE; ours cannot.

Every comparison already yields `Bool`; the problem is the route there.

## Decision: `Eq` and `Ord` in `core::cmp`

As rustc's `PartialEq`/`PartialOrd`, each comparison is a method returning
`Bool`:

```siox
pub trait Eq<Rhs> {
    fn eq(self, rhs: Rhs) -> Bool;                                  // ==
    fn ne(self, rhs: Rhs) -> Bool { return not self.eq(rhs); }      // !=
}

pub trait Ord<Rhs> {
    fn lt(self, rhs: Rhs) -> Bool;                                  // <
    fn le(self, rhs: Rhs) -> Bool;                                  // <=
    fn gt(self, rhs: Rhs) -> Bool { return rhs.lt(self); }          // >
    fn ge(self, rhs: Rhs) -> Bool { return rhs.le(self); }          // >=
}
```

- `a == b` calls `a.eq(b)`, `a < b` calls `a.lt(b)`, and so on. The compiler
  finds the traits by lang item (`eq`, `ord`), never by name, and no longer
  derives anything through `Ordering`.
- Equality and order are separate: a type implements `Eq` alone when it has no
  order. `Ord` does not require `Eq`.
- Only `eq`, `lt` and `le` are required. `ne`, `gt` and `ge` have defaults,
  and a type may override them (IEEE `ne` is true for a NaN).
- **Two traits, not rustc's four.** rustc splits `PartialEq`/`Eq` and
  `PartialOrd`/`Ord` because its library relies on totality (sorting, hashing).
  Nothing in siox does, so the partial/total distinction would be names
  without behaviour. siox's `Eq`/`Ord` are rustc's *partial* traits.
- Built-in comparison stays where it is today: the kernel types (`integer`,
  `real`, `Char`), enums compared by discriminant, and `Logic`-element
  vectors with no impl. An `Eq` or `Ord` impl, where there is one, takes over.
- `Ordering` stays in `core::cmp` as an ordinary enum for code that wants a
  three-way answer. It is no longer a lang item.
- `<=>` is no longer an operator symbol. An `impl Operator<"<=>", …>` is an
  error whose help shows the `Eq`/`Ord` impls that replace it, as the removed
  `using` keyword does.

```siox
impl Eq<Version> for Version {
    fn eq(self, rhs: Version) -> Bool {
        return self.major == rhs.major and self.minor == rhs.minor;
    }
}

impl Ord<Version> for Version {
    fn lt(self, rhs: Version) -> Bool {
        return self.major < rhs.major or (self.major == rhs.major and self.minor < rhs.minor);
    }
    fn le(self, rhs: Version) -> Bool { return not rhs.lt(self); }
}
```

## Decision: `abs`, `rem` and `mod` are functions

As in mathematics and in Rust (`x.abs()`, `x.rem_euclid(m)`), these are
functions, not operators, so the grammar does not grow:

| method | result | example |
| --- | --- | --- |
| `x.abs()` | the magnitude | `(0 - 5).abs() == 5` |
| `x.rem(m)` | remainder with the sign of the dividend (VHDL `rem`, Rust `%`) | `(0 - 7).rem(2) == -1` |
| `x.mod(m)` | remainder with the sign of the divisor (VHDL `mod`) | `(0 - 7).mod(2) == 1` |

They are declared for `integer`, `signed`, `unsigned`, `sfixed`, `ufixed` and
`float` where they make sense (`unsigned` has no sign, so its `rem` and `mod`
agree and its `abs` is itself). `std::math::abs` stays as the free-function
spelling for `integer`.

## Migration

- std: `unsigned`, `signed`, `time`, `frequency`, `ufixed`, `sfixed` and
  `float` move from `<=>` to `Eq`/`Ord`; `float` gains IEEE NaN semantics
  (every comparison with a NaN is false except `!=`).
- Corpus programs with `<=>` impls move the same way.
- language §3.25 and std.md describe the traits; the compiler's two
  derivations (`inline_cmp` on the design path, `inline_process_comparison`
  on the Process path) become plain calls to `eq`/`ne`/`lt`/`le`/`gt`/`ge`.
