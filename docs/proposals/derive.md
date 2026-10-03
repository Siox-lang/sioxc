# `#[derive]`

Status: **proposal**. Nothing here is implemented. The rest of the original
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
  not run user code over a token stream (see [macros.md](macros.md)).
