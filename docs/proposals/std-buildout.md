# Remaining standard-library build-out

Status: active proposal; implemented exports live in [std.md](https://github.com/Siox-lang/siox-paper/blob/main/docs/std.md).
Outstanding items are tracked under [std in TODO.md](../../TODO.md#std).

## Boundary

Std is the mandatory, vendor-independent library of source-defined types,
operators, conversions, math, text, time and small technology-independent
helpers. Compiler mechanisms and primitive hooks live in the compiler/core;
the current split is documented in [std.md](https://github.com/Siox-lang/siox-paper/blob/main/docs/std.md), not proposed here.

Synchronizers, base metadata, fixed-point formats and initial floating-point
operators are implemented. Floating-point operators execute through the same
canonical Process pipeline in hardware and test processes; the old hardware
tree-inlining limitation is gone. Their syntax and behavior belong in the
reference, not in a completed migration plan.

## Remaining numeric capabilities

1. **Fixed point.** Division and the resizing constructor are implemented
   (std.md): same-format quotients rounding toward minus infinity, zero for a
   quotient by zero, and a nearest/saturating resize. Remaining: wrap and
   truncate resize styles. Keep
   `ufixed<W, F>`/`sfixed<W, F>` semantics source-owned.
2. **Floating point.** Add division, square root, subnormal support, additional
   rounding modes and conversions to/from fixed point. Specify exceptional
   values and rounding per operation; do not silently change existing
   flush-to-zero behavior. Preserve source-defined operators, shared layouts
   and the common hardware/procedural pipeline.
3. **Linear algebra (optional).** Generic `Vector<T, N>` and
   `Matrix<T, R, C>` over types with the required operator traits. Start with
   demonstrated uses for element-wise operations, dot/matrix products and
   transpose. These are ordinary array-backed library types, not a revived
   compiler vector trait or a second array representation.

The target query remains separate work: simulation accepts reachable host
services, while future elaboration must reject runtime-only operations.
Do not advertise synthesis support merely because a helper is source-defined.

## Acceptance

Each added public declaration needs reference documentation and a runnable
program in the sibling `siox-tests` repository. Exercise hardware and procedural
use through the same native backend, including rounding, overflow, exceptional
inputs, format boundaries and default/`bitpack` parity. Add compiler tests only
where the library exposes a language mechanism; do not hardcode type spellings
or numeric truth tables in the compiler.

## Exclusions

Memories, FIFOs, streams, bus protocols, scoreboards and vendor primitives belong
to IP/project libraries, not std. Vendor-specific metadata belongs to vendor
packages. Foreign HDL package loading and synthesis-facing output remain Phase 3
project/backend work. A verification framework should not grow inside the
compiler as a substitute for the isolated cocotb integration.
