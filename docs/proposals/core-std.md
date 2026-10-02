# `core` and `std`

Status: **slice 1 implemented**: `core` is compiled in, the compiler's hooks and
directives live there, and the compiler finds them by lang item. `builtin #`
macros, `error!`, the leaf-name lookups and the new `std` content remain.

Today one flat `std/` holds two different kinds of thing: declarations the
compiler cannot work without, and ordinary library code a user could have
written. This proposal splits them, the way Rust splits `core` from `std`, but
along siox's own line:

- **`core`** is the part of the compiler that is reachable through the
  language: what cannot be done by an external library.
- **`std`** is everything a user can program with: data types, helpers,
  conversions and common metadata, written in ordinary siox on top of `core`.

## The rule

A declaration belongs in **`core`** when the compiler gives it meaning that
siox source cannot express: it is a hook the compiler calls, a type the
grammar produces, a directive or macro the compiler implements, or an
operation only the compiler or runtime can perform. Deleting it would break
the language itself.

A declaration belongs in **`std`** when it could live in a third-party
package with no loss: it is built from `core`'s hooks with ordinary `impl`s,
`fn`s, `type`s and `attr` declarations. Deleting it would remove a
convenience, not a capability.

This differs from Rust's split, which runs along "needs an operating system".
In siox the question is "needs the compiler", because the simulator is the
compiler's runtime and every target has one.

## `core`

| Area | Contents | Why it cannot be a library |
| --- | --- | --- |
| Kernel types | `integer`, `real`, `Char`, `Bool` (`true`/`false`), `string = Char[]`, `Range` | The grammar produces them: integer, real, character, string and range literals, and conditions. |
| Operator hooks | `Operator<"sym", In, Out>`, `Ordering` (driving all six comparisons), `Prefix`, `Suffix`, `Index`, `IndexAssign` | Expression syntax dispatches to them. |
| Value hooks | `Boolean`, `Resolve`, `New`, `From`, `LogicEncoding` | Conditions, parallel drivers, defaults, conversions and metavalue planes call them. |
| Directives | `#[test]`, `#[allow]`/`#[warn]`/`#[deny]`/`#[forbid]` and `enum Lint`, `#[latched]`/`#[latch]` ([pipelines.md](pipelines.md)), `lang` | They change what the compiler emits or reports. |
| Compiler-read attributes | `precedence` | The parser reads it to group expressions. |
| Macros | `print!`, `assert!`, `warn!`, and a new `error!` | They capture source location and expand format strings. |
| Runtime services | `await`, `stop()`, `finish()`, `read<T>`, `exists`, `rand`/`randint`/`uniform`/`seed` | Only the simulator can provide them. |
| Assertion levels | `Severity` | `assert!`/`warn!`/`error!` will take it. |

`error!("message")` is new. It fails the simulation unconditionally, with its
source location, like Rust's `panic!`. Today the spelling is
`assert!(false, "message")`, which reads as a mistake. Every simulation error
the runtime raises (range violations, failed reads) can use the same path.

`core` is versioned with the compiler. It is compiled into `sioxc` (like the
fixed runtime) rather than read from `--std`, so a compiler can never load a
`core` it does not match, and a frontend-only build needs no files on disk.

## `std`

Everything a user programs with, built on `core`:

| Area | Contents |
| --- | --- |
| Logic values | `Bit`, `ULogic`, `Logic` with the IEEE 1076-2019 truth tables and resolution, `ClockLike` with `rising`/`falling`/`edge`, `LOW`/`HIGH` |
| Numeric vectors | `unsigned[N]`, `signed[N]` and their operators, `sext`, conversions |
| Ranged integers | `Byte`, `Short`, `Int`, `Long`, `Natural`, `Positive`, `clog2` |
| Math | `Complex`, `abs`/`min`/`max`, the C math functions, `PI`/`E` |
| Text | the `Unicode`/`Ascii` encoding tables and their shorthands |
| Time | `time`, `frequency`, the `fs`…`ms` and `Hz`…`GHz` suffixes |
| New: linear algebra | `Vector<T, N>` and `Matrix<T, R, C>` over any `T` with the needed operators, element-wise operations, dot and matrix products, transpose; fixed-point families once they exist |
| New: hardware helpers | the reusable models in TODO: synchronizers, counters, memories, FIFOs, stream adapters |
| Common metadata | below |

`Bit` and `Logic` move to `std` even though the compiler mentions them today.
The compiler's real dependency is on `LogicEncoding` (how a multi-valued
element maps to value and metavalue planes) and on `ClockLike`-style methods
built from `'event`/`'old`. The four places that still name `Bit` or `Logic`
by spelling become lang items (see
[compiler-foundations.md](compiler-foundations.md) §5) or move behind
`LogicEncoding`.

### Common metadata in `std::attrs`

Metadata the compiler never reads but vendor flows and tools expect,
declared once in vendor-neutral spelling with defaults. A backend maps each
one to its vendor's name.

| Attribute | For | Vivado | Quartus | Yosys |
| --- | --- | --- | --- | --- |
| `keep: Bool` | `let`, port | `KEEP`, `DONT_TOUCH` | `preserve`, `keep` | `keep` |
| `async_reg: Bool` | `let` | `ASYNC_REG` | `altera_attribute -name SYNCHRONIZER_IDENTIFICATION` | — |
| `ram_style: RamStyle` | `let` (array) | `RAM_STYLE` | `ramstyle` | `ram_style` |
| `rom_style: RomStyle` | `let` (array) | `ROM_STYLE` | `romstyle` | `rom_style` |
| `fsm_encoding: FsmEncoding` | `let` (enum) | `FSM_ENCODING` | `syn_encoding` | `fsm_encoding` |
| `max_fanout: integer` | `let`, port | `MAX_FANOUT` | `maxfan` | — |
| `mark_debug: Bool` | `let`, port | `MARK_DEBUG` | `preserve` (SignalTap) | — |
| `clock: Bool`, `frequency: frequency` | port | `create_clock` constraint | `create_clock` | — |
| `io_standard: string`, `pin: string` | port | `IOSTANDARD`, `PACKAGE_PIN` | `IO_STANDARD`, `chip_pin` | — |
| `library: string`, `name: string` | entity | library and entity name for foreign HDL | same | same |

`RamStyle`, `RomStyle` and `FsmEncoding` are small `std` enums (`Block`,
`Distributed`, `Registers`, `Auto`; `OneHot`, `Gray`, `Binary`, `Auto`).
Vendor-specific names that have no neutral spelling stay in vendor packages
(`attr vivado::iob for …`), as today.

## Prelude

- `core` has a prelude every module gets: the kernel types, the hooks, the
  directives and the macros. It cannot be turned off.
- `std` keeps its prelude: `Bit`, `Logic`, `unsigned`, `signed`, `sext`,
  `string`, `time`, `frequency`.
- `std` re-exports `core` where today's paths name it, so `std::ops::Operator`
  keeps working as a re-export of `core::ops::Operator`.

There is no `#![no_std]` here: under this split `std` holds the logic and
numeric types that hardware is written in. Restricting a design to what can be
synthesized is a separate question, answered per declaration by the target
(simulation-only items such as `read<T>` at run time or `randint` are reported
when compiling for synthesis), not by excluding a library.

## Migration

1. Create `core` as compiled-in source with module paths `core::ops`,
   `core::attrs`, `core::text`, `core::sim`, `core::assert`, `core::fs`,
   `core::rand`. Move the hook traits, `Range`, `Ordering`, `Bool`, the
   directive declarations, `Severity`, and the runtime-service declarations
   into it.
2. Leave `pub use core::…` re-exports in the old `std` modules, so no program
   changes.
3. Replace the compiler's builtin fallbacks (today seeded by name for
   std-less builds) with `core`, which is always present.
4. Move the four remaining `Bit`/`Logic` spellings in the compiler behind lang
   items or `LogicEncoding`.
5. Add `error!`, then the new `std` content (vectors, matrices, the common
   attributes) one module at a time, each with a corpus program.

## Slice 1

What the first slice does, decided:

- **`core` is compiled in.** Its `.siox` sources live in the repository's
  `core/` directory and are embedded in `sioxc` with `include_str!`; a
  `core::…` path never reads `--std`. Diagnostics name the files
  `<core>/ops.siox` and so on. Every compilation loads `core::prelude`, as it
  loads `std::prelude` today.
- **What moves:**

  | module | contents |
  | --- | --- |
  | `core::ops` | `Bool` and its logical operators, `Operator`, `Prefix`, `Suffix`, `Range`, `Index`, `IndexAssign`, `Ordering`, `Boolean`, `Resolve`, `New`, `From`, `LogicEncoding`, and the element-wise impls over `T[]` |
  | `core::attrs` | `test`, `allow`/`warn`/`deny`/`forbid` and `Lint`, `precedence`, `lang` |
  | `core::text` | `string` |
  | `core::assert` | `Severity` |
  | `core::prelude` | re-exports all of the above |

  `std::fs` and `std::rand` declare nothing today (their functions are
  runtime builtins), so they stay where they are until `builtin #` gives them
  declarations. `time` and `frequency` stay in `std::sim`, as the table above
  says; the compiler finds `time` through its lang item.
- **`std` re-exports** each moved name from its old module (`pub use
  core::ops::Operator;` in `std::ops`, `Bool` in `std::logic`), so no program
  changes. `std::prelude` keeps its list.
- **Lang items replace path matching.** `core::attrs` declares
  `attr lang: string`, and the declaring modules bind it:
  `attr lang for Operator = "operator";`. The resolver builds a table from
  lang name to declaration, and every compiler check that matched a
  `std::…` path asks it instead: the hook traits, `test`, `Bool`, `time`,
  `unsigned`, `LogicEncoding`. Only `core` and `std` modules may bind `lang`,
  and one name bound twice is an error. Lookups by a type's *leaf* name
  (`ty_from_head("Bool")`) are not path matches and stay for now; removing
  them is the rest of compiler-foundations §5.
- **Builtin fallbacks stay** for frontend tests that resolve without loading
  `core`, and they seed the lang table under the same names.

Later slices: `builtin #` and the built-in macros as `core` declarations
(macros.md), `error!`, the leaf-name lookups, and the new `std` content.

## Open questions

- Should `std::attrs`' common metadata live in `std` itself, or in a
  `std::vendor` module so the vendor-facing names are one import away from
  the language's own?
- `Bool` is the condition type, so it is `core`. `Bit` is what most signals
  are. Does a clock edge (`'event`, `'old`) need anything from `std`, or is
  `ClockLike` enough as an ordinary trait?
- Where do the `std` linear-algebra types stop: generic over any numeric `T`,
  or fixed to `signed`/`unsigned` until fixed-point lands?
