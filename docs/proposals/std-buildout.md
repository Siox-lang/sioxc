# Standard-library build-out

Status: **active proposal**. Existing exports are documented in
[`std.md`](../std.md); open work is tracked under `std` in
[`TODO.md`](../../TODO.md).

## Boundary

The compiler owns mechanisms and representation:

- parsing, types, traits, operator dispatch, attributes, elaboration;
- digital IR, event semantics, native ABI, and runtime intrinsics.

The standard library owns domain meaning:

- visible scalar/vector types and traits;
- operator and resolution implementations;
- conversions, math, text, time, assertions, and reusable hardware models.

```mermaid
flowchart TD
    K["compiler kernel<br/>mechanisms + IR intrinsics"]
    P["std::prelude"]
    OPS["std::ops"]
    LOGIC["std::logic"]
    BITS["std::bits"]
    CORE["std::math + std::text"]
    SIM["std::sim + std::assert + std::fs"]
    MODEL["future reusable models<br/>sync · memory · fifo · stream · fixed"]

    K --> P
    P --> OPS
    P --> LOGIC
    LOGIC --> BITS
    OPS --> BITS
    P --> CORE
    CORE --> SIM
    BITS --> MODEL
    SIM --> MODEL
```

Core type/operator modules must remain pure and suitable for later synthesis.
Simulation services may call runtime intrinsics. Reusable models may depend on
both but should state whether they are intended for hardware or testbenches.

## Existing modules

`core` (compiled into `sioxc`, proposals/core-std.md) holds what the compiler
gives meaning to: `Bool`, the hook traits, `Range`, `Ordering`, `string`, the
directives, `Severity` and the built-in macros. std re-exports each.

- `std::prelude` — auto-loaded surface: `Bit`, `Logic`, `unsigned`, `signed`,
  `time`, `frequency`, and the `core` names again.
- `std::ops` — `Bit`'s operators and condition; re-exports the `core::ops` hooks.
- `std::logic` — `Bit`, nine-value `Logic`/`ULogic`, clock helpers, truth
  tables, and resolution.
- `std::bits` — `unsigned`/`signed`, numeric operators, comparisons,
  conversions, and resizing.
- `std::attrs` — tool metadata; re-exports the `core::attrs` directives.
- `std::sim` — time/frequency units and simulation helpers.
- `std::assert` — re-exports `Severity`.
- `std::math` — real/complex math surfaces backed by native functions.
- `std::text` — encoding tables over `Char`.
- `std::fs` — fixture reads and existence checks.

## Build order

1. **Synchronizers and reset helpers** — `std::sync`, being implemented:

   | entity | ports | behaviour |
   | --- | --- | --- |
   | `Sync2` | `clk`, `d` in; `q` out | two-flop synchronizer for a level crossing into `clk`'s domain |
   | `ResetSync` | `clk`, `rst_in` in; `rst_out` out | active-high reset: asserts at once (asynchronously), releases two `clk` edges after `rst_in` falls |
   | `EdgeDetect` | `clk`, `d` in; `rise`, `fall` out | one-cycle pulses when `d` (already in `clk`'s domain) changes |
   | `PulseSync` | `src_clk`, `pulse_in`, `dst_clk` in; `pulse_out` out | carries single-cycle pulses between domains: a toggle in the source, `Sync2`, and an edge detector in the destination |

   All are `Bit`-typed, as internal signals and clocks are. The flops of a
   synchronizer are bound `async_reg` (below), so a synthesis flow keeps them
   together and does not optimise them. Multi-bit values do not cross with
   `Sync2`; they need a handshake or a Gray-coded FIFO (item 3).

   With it, `std::attrs` gains the vendor-neutral metadata from
   [core-std.md](core-std.md): `async_reg`, `ram_style`, `rom_style`,
   `fsm_encoding`, `max_fanout`, `mark_debug`, `clock`, `io_standard` and
   `pin`, with the enums `RamStyle`, `RomStyle` and `FsmEncoding`. The
   compiler reads none of them; a later backend maps each to its vendor's
   name. A `frequency` attribute waits until attribute values of a struct
   type are checked.
2. **Memories**
   - synchronous single/dual-port RAM shapes;
   - initialization from arrays/files;
   - collision behavior documented and tested.
3. **Streams and FIFOs**
   - canonical ready/valid backing structs and views;
   - skid buffer, pipeline register, width adapter;
   - synchronous FIFO first, asynchronous FIFO after CDC coverage.
4. **Numeric families**
   - fixed-point `ufixed`/`sfixed`;
   - saturation/rounding policies as explicit types or template parameters;
   - conversions to/from integer and real.
5. **Verification helpers**
   - deterministic random generation;
   - scoreboards and monitors only after the external scheduler/API boundary is
     stable.

Every new public declaration needs:

- source-level documentation in `std.md`;
- focused compiler/unit coverage where it exercises a language mechanism;
- at least one runnable program in `siox-tests`;
- no compiler special case based only on the library type’s spelling.

## Deliberate exclusions

- Vendor primitives and generated IP belong to project/vendor packages.
- VHDL/Verilog package loading belongs to the future project/API layer.
- A UVM-sized verification framework waits for cocotb integration rather than
  growing inside the compiler repository.
