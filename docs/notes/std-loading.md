# Proposal: loading `std/` (and multi-file modules)

Status: **implemented** (`--std <dir>` default `./std`; a bad import is a hard
error, `E-P011`). Fork 1 was later **revised**: the kernel keeps only `integer`
and `real` as base types — Bit/Logic/Bool/Clock are now canonical `enum`
declarations in std/logic.siox, and uint/int are derived Logic vectors, with
the compiler's by-name handling kept as a shim until operator overloading
(see the spec's "type kernel" section under Stage 11). Kept for the design
rationale; the live status lives in docs/implementation.md (Stage 11).

## Where we were (before this landed)

- The CLI loads **one file** (`read_to_string(path)` → one `Module`). There is no
  multi-file front end.
- `siox-resolve` **seeds** the primitives and well-known names as spanless
  builtins: `Bit, Logic, Bool, Clock, uint, int, usize, string, Boolean` and the
  attrs `top, test, keep, library, name`.
- `using std::logic::{Bit}` parses to `Import { base: std::logic, names: [Bit] }`.
  Resolve binds each name; if it isn't already a builtin it becomes an **opaque
  `External`** def (no real resolution). So the `std::logic` path is *ignored* —
  imports only "work" when the name coincides with a seeded builtin.
- `std/` contains only `ops.siox` (the `Boolean` trait). It is never read.

## What "loading std" means

Three separable pieces:

1. **Discovery** — find the std root on disk.
2. **Module→file mapping + multi-file front end** — turn `using a::b` into "parse
   `a/b.siox`", transitively, before resolving.
3. **Cross-module resolution** — bind `using a::b::{X}` to the *actual* `pub X`
   declared in module `a::b`, replacing the `External` stub.

Each is small on its own. The order above is the build order.

## Fork 1 — the primitives can't live in a std file

`Bit`, `Logic`, `uint`, … are **intrinsic** to the compiler (the type checker and
IR special-case them). A `std/logic.siox` cannot *define* `Bit` from nothing.
Two ways to reconcile that with "load std":

- **(A) Primitives stay intrinsic; std/ carries only derived items.**
  Keep seeding `Bit/uint/...` as builtins. `std/` provides library-level things
  that *are* expressible in siox: the `Boolean` trait (already in `ops.siox`),
  `std::assert` helpers, ready/valid types, etc. `using std::logic::Bit` still
  resolves `Bit` to the builtin; `std/logic.siox` exists mainly to re-export/doc.
  Retiring "seeded builtins" then applies only to the **trait + attrs**, not the
  primitive types.
  *Least churn, honest about what's intrinsic. Recommended.*

- **(B) Declare primitives in std with an `intrinsic`/`extern type` marker.**
  `std/logic.siox` says `pub intrinsic type Bit;` and the compiler matches those
  markers to its built-in handling. Fully "std-defined" surface, but needs new
  syntax + a binding layer, and the checker/IR still special-case them anyway.
  *More machinery for a mostly-cosmetic win.*

I recommend **(A)**: it removes the *trait/attr* seeding (which genuinely belongs
in std) while leaving primitives where the compiler already needs them.

## Fork 2 — how to discover the std root

- **(A) `--std <dir>` flag, default `./std`.** Zero magic, works from the repo.
  Add `SIOX_STD` env override later. *Recommended for Phase 1.*
- **(B) Relative to the binary** (`$exe/../std`). Nice for an installed toolchain,
  but brittle in `cargo run` / tests.

Recommend **(A)** now, **(B)** when there's an install story.

## Proposed shape (given A/A)

```
siox-cli:
  load(entry) -> Vec<Module>:
    parse entry; queue its `using` bases
    while queue non-empty:
      base -> file: std_root / base.segments.join("/") + ".siox"
      if unread and exists: parse, enqueue its bases
    return all modules (entry first)

siox-resolve::resolve(&[Module], ...):        # already takes a slice
  index modules by `module.path`
  seed *primitives + attrs only* (drop the Boolean trait seed)
  for `using base::{names}`:
    find module whose path == base
    bind each name to that module's matching `pub` def
    (unknown module or private/missing name -> today's diagnostics, now real)
```

`resolve` already accepts `&[Module]`, so most of the change is: build the
module-path index, and make the `Import` arm look names up in the target module
instead of minting an `External`. `std/logic.siox` (re-export shims) and
`std/bits.siox`, `std/assert.siox`, `std/sim.siox` get filled in as needed.

## Build order (small, testable steps)

1. Multi-file loader in the CLI (module→file, transitive parse) + a `--std` flag.
2. Module-path index in resolve; `using` binds to real `pub` defs across modules.
3. Move the `Boolean` trait (and eventually attrs) out of the seed into
   `std/ops.siox` / `std/attrs.siox`; keep primitives seeded.
4. Fill `std/logic.siox`, `std/bits.siox`, `std/assert.siox` and switch the
   examples to real imports; the `External`-stub path becomes an error.

## Open questions for you

- Approve **A/A** (primitives intrinsic, `--std ./std`)?
- Should a missing/opaque import stay a *warning* (lenient, as the `External`
  stub is now) or become a hard *error* once std loading lands?
- Is `a::b` → `a/b.siox` the mapping you want, or `a/b/mod.siox`-style dirs?
