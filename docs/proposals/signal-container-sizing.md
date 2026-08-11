# Signal container sizing

Status: **implemented design record**.

This proposal is complete. The current implementation lives in `src/ir.rs`,
`src/llvm/mod.rs` and `src/llvm/emit.rs`; active follow-ups are tracked under
the IR and LLVM headings in [`TODO.md`](../../TODO.md).

## Implemented layout

Logical width belongs to each signal/type, never to the whole design:

```mermaid
flowchart LR
    TY["source type"] --> BITS["semantic bit width"]
    BITS --> IR["IR Signal.width"]
    IR --> DEF["default storage<br/>smallest practical LLVM integer"]
    IR --> PACK["bitpack storage<br/>shared 64-bit words"]
    IR --> ABI["external ABI<br/>ceil(width / 64) words"]
```

- Default LLVM state uses width-sized integer fields (`i8`, `i16`, `i32`,
  `i64`, then LLVM `iN` for wider values).
- `bitpack` packs sub-word signals into shared 64-bit words without letting a
  field straddle a word. Wide values are word-aligned and reserve consecutive
  words.
- Event flags use a separate one-bit-per-signal bitset under `bitpack`; their
  storage is independent of value width.
- The external ABI exchanges low-word-first 64-bit chunks. No global maximum
  word count exists.
- Enums use enough bits for their actual discriminants, including explicit
  non-dense values.
- Structs and hardware arrays flatten into leaf signals, so each leaf receives
  its own minimal representation.
- `Design::source_layouts` retains the pre-flattening recursive shape for every
  concrete value and leaf. `SourceLayout` distinguishes scalar, packed, array,
  struct/view, and unresolved shapes; preserves view directions, written ranges
  and source spans; and computes aggregate bit width and leaf count with checked
  arithmetic. IR storage flattening and range/index operations consume it.

## Exactness rule

Storage is unobservable. Packing may not change a value, delta ordering,
event detection, initialization, or waveform output. Default and `bitpack`
builds run the same semantic tests, including arbitrary-width and X/Z cases.

## Remaining related work

- Move expression-level struct materialization and the native test harness off
  their declaration-shaped field tables and onto the persisted layouts.
- Have LLVM consume the layouts directly if non-flattened aggregate IR values
  are introduced.
- Add repeatable memory/runtime benchmarks for default and packed state.
