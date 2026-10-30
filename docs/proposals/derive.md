# `#[derive]`

Status: **partially implemented** (2026-10-30). `#[derive(Eq)]` and
`#[derive(Ord)]` on a non-generic struct are implemented; `#[derive(Resolve)]`
and derives on generic or newtype structs remain (both `E-P040` today). The
rest of the original
compiler-directives proposal has landed: `#[...]` holds only directives
(language §3.5), and lint control is `#[allow]`/`#[warn]`/`#[deny]`/`#[forbid]`
plus `-A`/`-W`/`-D`/`-F` (§3.5a).

## Idea

`#[derive(Eq, Ord)]` on a struct would generate the `Eq`/`Ord` impls that drive
the six comparisons (field by field, lexicographically), and `#[derive(Resolve)]`
would generate an element-wise fold. Both are code generation, so both are
directives, declared in `std::attrs` beside `test` and the lint levels.

This is speculative. It needs a decision about where generated implementations
live for coherence (3.26 restricts inherent implementations to the defining
module), and it should not be built before there is a second real user.

Candidates for that second user (2026-10-30): `Default` is now a trait
(sioxc #304), so `#[derive(Default)]` would spell what the structural default
already does; `std::math::Complex` has no `Eq` at all, which a derive would
supply field by field; and
[payload enums](payload-enums.md) would want `Eq` on `Option<T>`/`Result<T, E>`
for any payload with one.

## As implemented

`#[derive(Eq, Ord)]` directly before a struct writes, in the struct's own
module, the impls a user would write by hand:

```siox
#[derive(Eq, Ord)]
struct Version { pub major: unsigned[4], pub minor: unsigned[4] }

// writes:
impl Eq<Version> for Version {
    fn eq(self, rhs: Version) -> Bool {
        return (self.major == rhs.major) and (self.minor == rhs.minor);
    }
}
impl Ord<Version> for Version {
    fn lt(self, rhs: Version) -> Bool {
        return (self.major < rhs.major) or ((self.major == rhs.major) and (self.minor < rhs.minor));
    }
    fn le(self, rhs: Version) -> Bool { … the same, ending in `<=` … }
}
```

`ne`, `gt` and `ge` keep their trait defaults. The coherence question is
settled by placement: a derive sits on the struct, so its impls belong to the
defining module, where 3.26 already allows them. The expansion runs with the
macros, before names resolve. Each impl is a source file of its own
(`<derive Eq for Version>`) that belongs to the struct's module, so a field
whose type has no `==` is an error shown in the generated code, noted with the
derive it came from. Any other trait, or a generic or newtype struct, is
`E-P040` at the attribute.

## What does not qualify: `cfg`

Conditional compilation is already decided against. The compile-time target
value is `std::target: std::Target`, and the unified pipeline plan is explicit
that it "is folded before reachability and target validation. It selects code
within the same pipeline; it does not select another frontend or IR."

A value that folds is strictly better than a directive that deletes: it
type-checks both branches, it cannot desynchronise two configurations, and it
keeps one IR. `#[cfg]` should not be added.

## Non-goals

- A macro system. `#[...]` directs a fixed set of compiler behaviors; it does
  not run user code over a token stream (see [language.md](https://github.com/Siox-lang/siox-paper/blob/main/docs/language.md), §3.30).
