# Payload enums

Status: **proposal, on hold** (owner, 2026-10-30). Nothing here is
implemented; the owner asked not to start it yet, and the rule in *Value enums
and sum types* below was agreed then. It supersedes language
§3.8's "Phase 1 should avoid Rust-style payload enums" once accepted, and
builds on tuples ([language §3.7a](https://github.com/Siox-lang/siox-paper/blob/main/docs/language.md#37a-tuples-are-anonymous-structs)).

## Problem

siox enums are plain variant sets. Values that are "one of several shapes"
have no type:

- **Optional and fallible results.** `checked_add` cannot say "no value";
  `read<T>` failures can only be fatal (language §3.25's "a `Result`-style
  value would ride on future payload-carrying enums").
- **Protocol messages and commands.** A bus transaction that is either an
  idle cycle, a write with address and data, or a read with an address is
  today a struct with every field plus a kind enum, with no check that only
  the fields of the current kind are read.
- **State machines with per-state data.** A counter that only exists in one
  state is a separate signal, live (and visible in waveforms) in every state.

SystemVerilog has tagged unions for this; VHDL writes a record with a kind
field and leaves the discipline to the designer.

## Proposal

Variants may carry data, as tuple or struct variants:

```siox
pub enum Option<T> { None, Some(T) }
pub enum Result<T, E> { Ok(T), Err(E) }

enum Command {
    Idle,
    Write { addr: unsigned[16], data: unsigned[32] },
    Read(unsigned[16]),
}

let c: Command = Command::Write { .addr = 0x10, .data = word };
let x: Option<unsigned[8]> = Option::Some(3);

match c {
    Command::Idle => ready = '1',
    Command::Write { addr, data } => { memory[addr] = data; }
    Command::Read(addr) => q = memory[addr],
}

match a.checked_add(b) {
    Option::Some(sum) => total = sum,
    Option::None => overflow = '1',
}
```

### Patterns that bind

A variant pattern names its payload: `Command::Read(addr)`, `Some(x)`,
`Command::Write { addr, data }` or `Command::Write { addr, .. }`. Each name
is a value bound for that arm only. Positions take any pattern, so
`Option::Some(0..15)` and `Command::Read(x"00??")` work, with the semantics
those patterns already have (`Ord`, `Match`). This is the first pattern form
that binds; `_` and the existing forms stay as they are.

### Representation

A payload enum is a **tag** plus a **payload region**:

- The tag is the variant index, encoded as today's enum discriminants.
- The payload region is as wide as the largest variant's payload. Each
  variant's fields are laid out from the region's low end, so variants
  overlay each other, as a SystemVerilog tagged union does.
- Bits a smaller variant leaves unused are written `'0'` when a value is
  constructed, so two equal values have equal bits (and waveforms are
  stable). Reading another variant's fields is not expressible: a field is
  only in scope inside its variant's arm.

In hardware this is exactly what designers write by hand: a kind field and a
shared data bus. Flattened signals are `c.tag` and `c.payload`; waveforms
show the tag's variant name and the active variant's fields. `Logic` fields
keep their metavalue companions.

### Semantics

- **Equality:** same tag and equal active payloads.
- **Construction** is the only way to set a payload; there is no field
  assignment into a variant, so a value is always well-formed.
- **Exhaustiveness** extends the current enum check: every variant must be
  covered, or `_` present.
- **Printing:** `Some(3)`, `Write { addr: 16, data: 255 }`, Rust's `Debug`
  form; `Display` impls as for any type.
- **Generics:** `Option<T>`/`Result<T, E>` use the generic machinery structs
  already have (`struct Box<T>`).

### Standard library

- `core::option::Option` and `core::result::Result`, in both preludes.
- Methods in source: `is_some`, `is_none`, `unwrap_or`, `map`-free forms first
  (siox has no closures), `ok_or`, `is_ok`, `is_err`, `unwrap_or` for
  `Result`.
- `unwrap`/`expect` fail the test in a testbench, like `assert!`. In
  hardware there is nothing to fail, so they are testbench-only; hardware
  code uses `match` or `unwrap_or`.
- `std::bits` gains `checked_add`, `checked_sub`, `checked_mul` returning
  `Option<unsigned>` / `Option<signed>`.

### Value enums and sum types

An enum is one of two kinds, decided by its declaration:

| | Value enum (today's enums) | Sum type (payload enum) |
| --- | --- | --- |
| Variants | names, characters, or both | names only |
| Discriminants | explicit allowed (`Add = 0`, `'U' = 4`) | the tag, chosen by the compiler |
| Payloads | none | allowed on any variant |
| Examples | `Bit`, `ULogic`, `Opcode`, `State` | `Option<T>`, `Result<T, E>`, `Command` |

A character variant is a discriminant: `'X'` is a fixed encoding the logic
tables, metavalue planes and string literals rely on. As in Rust, where a
variant with an explicit discriminant carries no data, a character or
explicitly numbered variant has no payload, and a payload variant has no
discriminant:

```siox
enum A { '0', '1'(unsigned[4]) }         // a character variant has no payload
enum B { Add = 0, Jmp(unsigned[8]) }     // an enum with an explicit discriminant has no payloads
enum C { Idle, Read(unsigned[16]) = 2 }  // a payload variant has no discriminant
```

So `Bit`, `ULogic`, `Logic` and every user character enum keep their
representation, and an enum with no payloads is a value enum exactly as today.
An enum newtype (`enum Logic(ULogic);`) stays value-enum-only. A character
literal inside a payload takes the payload's type from context:
`let b: Option<Bit> = Some('1');` and the pattern `Some('1') =>` both read
`'1'` as a `Bit`, which needs the expected type to flow into generic payload
constructors and patterns.

### What exists to build on (2026-10-30)

Match coverage already proves more than enum variants: a catch-all arm, every
combination of a tuple of enums (sioxc #342) and every value of a number
(#347) are recorded as complete matches and reach the latch lint and the
testbench's last arm. Variant coverage for payload enums extends that one
place. Binding patterns are still the largest missing piece.

## Not in this proposal

- A `?` operator. Functions can return early, so it is implementable, but it
  hides control flow in hardware; a later proposal can weigh it.
- Recursive enums (no heap, no unbounded size in hardware).
- Explicit tag encodings (`#[encoding(onehot)]`): the existing enum
  discriminant rules apply to the tag first.

## Work

Larger than tuples, which it builds on: variant payload syntax and
construction, the tag-plus-region layout through elaboration and both
lowering paths, binding patterns (a new pattern form, with scopes for the
bound names in both paths), exhaustiveness, printing and waveforms, then
`core::option`/`core::result` and the `checked_*` methods. Binding patterns
are the largest single piece; they would also give tuples' `match` arms
bindings for free.

## Open questions

1. Should unused payload bits be `'0'` (deterministic, shown above) or `'-'`
   (don't-care, lets synthesis optimize)? `'0'` is easier to debug; `'-'`
   matches how designers often write it.
2. Should a variant's payload be readable outside a `match` through an
   explicit, checked accessor (`c.as_read()` returning `Option`)?
3. Should `Option<T>` for a `T` with a natural "empty" encoding (a `Logic`
   vector) ever be optimized to no tag? Rust's niche optimization; probably
   not worth it in hardware.
