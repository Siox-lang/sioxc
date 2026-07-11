# Proposal: Enum & Small-Signal Container Sizing

## Status

**Proposal**

This document proposes sizing the *physical container* of a simulation signal
to its logical width — so an enum or narrow vector is held in an 8-, 4-, 2-, or
1-bit container instead of a full machine slot — to reduce the simulator's
memory footprint and improve cache behaviour.

---

## 0. Guiding principle

> The simulator's job is **exact** simulation, not optimization.

This proposal is deliberately compatible with that principle: container sizing
is a **lossless representation choice**. It changes *where* and *in how many
bits* a value is stored, never the value itself, the delta-cycle order, or any
observable event. Nothing in the language, the IR, or the waveform output
changes. If a sized container could ever alter an observed value or event
ordering, that would be a bug, not a trade-off — exactness is not negotiable.

So the framing is: keep simulation exact, and separately let the backend pick
the cheapest *storage* that reproduces those exact semantics.

---

## 1. Motivation

Simulation values live in fixed-width **slots** — `u64` by default, `u128` when
the design declares signals wider than 64 bits (`--slot auto|64|128`, see
`docs/architecture.md`, "Backend slot widths"). Every signal, regardless of its
logical width, occupies a full slot in each of three parallel arrays:

- the interpreter keeps `SignalState { current: S, old: S, event: bool }` per
  signal (`crates/siox-sim`);
- the LLVM backend keeps three globals `cur`, `old`, `event`, each an
  `array_type(i64, n)` (`crates/siox-llvm/src/emit.rs`).

For a design dominated by small signals this is wasteful. A scalar `Logic`
needs **2 bits** but takes **64** in `cur` and **64** in `old`. The `event`
array is morally a bit per signal, yet the JIT spends a full **i64** on each.
A four-state FSM's state register (`enum State { … }`, ≤4 bits) sits in a 64-bit
slot. Concretely, `cur + old + event` for one 2-bit `Logic` costs **192 bits**
to carry 2 bits of state.

Event-driven simulation is overwhelmingly **memory-bound**: the delta cycle
sweeps a working set of signal values, and throughput tracks how much of that
set stays in cache. Shrinking each signal's footprint shrinks the working set,
raises cache hit rate, and reduces the bytes moved per settle — a speed win that
costs nothing in exactness.

Enums are the sharpest example: their variant set is fixed and known at compile
time, so their width is a small compile-time constant (siox already computes the
minimal discriminant width, `⌈log2 n⌉`, in `enum_reprs`). `Bit` is 1 bit,
`Logic`/`Clock` are 2, a 9-state FSM is 4, an opcode enum with 40 variants is 6.
These are the values that most want a small container.

---

## 2. Background: what siox already does

- **Logical widths are already minimal.** `enum_reprs` sizes an enum to
  `⌈log2(variant_count)⌉` bits; `#[vector]` families carry their `[N]` width;
  the discriminant space is dense (`0..n`), so no codes are wasted. The *logical*
  model is already tight.
- **Physical storage is uniform.** Everything is rounded up to a whole slot
  (64/128 bits). This proposal is entirely about closing the gap between the
  (small) logical width and the (large) physical container.
- **The value encoding is unchanged.** A scalar `Logic` is a 2-bit code
  (`0/1/Z/X`); `#[vector]` families are packed 2-value binary of their width.
  Container sizing operates on top of whatever encoding a signal already uses.

---

## 3. The idea: container tiers

Give each signal the **smallest container that holds its width**, from a fixed
tier ladder:

| Logical width | Container | Notes |
| ------------- | --------- | ----- |
| 1 bit         | 1-bit (bitset lane) | `Bit`, `event` flags, single-bit vectors |
| 2 bits        | 2-bit | `Logic`, `Clock`, ≤4-variant enums |
| 3–4 bits      | 4-bit (nibble) | ≤16-variant enums, `uint[4]` |
| 5–8 bits      | 8-bit (byte) | small vectors, ≤256-variant enums |
| 9–16 bits     | 16-bit | |
| 17–32 bits    | 32-bit | |
| 33–64 bits    | 64-bit | current default for everything |
| 65–128 bits   | 128-bit | today's `--slot 128` |

The container is chosen per signal at lowering time from the width that is
already known. Wide signals keep exactly today's behaviour; only narrow signals
shrink.

---

## 4. Packing strategies (and their trade-offs)

There is a spectrum from "cheap but coarse" to "dense but costly per access".
They are not exclusive — a real implementation likely mixes them.

### 4.1 Byte-granular containers (recommended first step)

Round each signal up to the smallest **byte-aligned** container (`u8`/`u16`/
`u32`/`u64`/`u128`). A `Logic` becomes a `u8`, an FSM state a `u8`, `uint[12]`
a `u16`.

- **Pros:** direct addressing — a load/store is one sized machine access, no
  shift/mask. Already a large win over uniform `u64` (8× for a `Logic`'s `cur`
  and `old`). LLVM lowers sized loads/stores natively; the interpreter dispatches
  on container size.
- **Cons:** sub-byte values still cost a whole byte. A 1-bit or 2-bit signal
  "wastes" up to 7 bits.
- **Verdict:** the sweet spot. Big memory reduction, zero per-access penalty,
  no exactness risk. Start here.

### 4.2 Sub-byte bit-packing (measure before adopting)

Pack several sub-byte signals into shared bytes (four `Logic`s per byte, eight
`Bit`s per byte; a real bitset for the `event` array).

- **Pros:** maximal density; the `event` array collapses from `n` bytes/words to
  `n/8` bytes — often the single biggest and hottest array in a settle.
- **Cons:** each access needs shift + mask; read-modify-write on a shared byte
  can create false sharing between unrelated signals in a threaded future.
- **Verdict:** worth it selectively — the `event` bitset almost certainly pays
  for itself; general sub-byte packing of `cur`/`old` should be gated on
  profiling that shows the design is memory-bound.

### 4.3 Structure-of-arrays by width (orthogonal, optional)

Group signals by container size: all 1-bit signals in one bitset, all bytes in a
byte array, all words in a word array. A settle that sweeps same-width signals
then streams contiguous memory.

- **Pros:** cache-friendly linear scans; each array is homogeneously typed.
- **Cons:** a signal's storage location no longer follows its `SignalId`
  ordinally — needs an id→(bank, offset) map, complicating the JIT's `sx_set`/
  `sx_read` addressing.
- **Verdict:** a later refinement once container sizing exists; the addressing
  indirection should be weighed against the locality gain.

---

## 5. Why enums are the prime candidate

- **Statically tiny.** A variant count fixes the width at compile time; most
  enums land in the 1–4 bit tiers (`Bit`, `Logic`, `Clock`, FSM states, opcodes,
  ready/valid handshake states).
- **Dense codes.** Discriminants are `0..n` with no gaps, so the minimal width is
  also fully utilised — packing loses nothing.
- **Ubiquitous and hot.** Control logic is mostly enums and single bits; they are
  read and written every cycle. Shrinking exactly the values the event wheel
  touches most is where cache pressure eases.
- **Already modelled.** The derived-types / `#[vector]` work means the compiler
  already knows every type's exact width and kind. Container sizing is a
  backend consumer of information the frontend already produces — no new
  language surface.

A concrete illustration: a design of 10 000 mostly-`Logic`/`Bit`/FSM signals
occupies `10000 × 3 × 8 B ≈ 235 KB` today (well past L2), versus `10000 × 3 × 1 B
≈ 29 KB` with byte containers (comfortably in L2), or a few KB with the `event`
bitset — the difference between spilling to L3/DRAM and staying resident.

---

## 6. Interaction with the engines

- **Interpreter (`siox-sim`).** `SignalState<S>` generalises from one `Slot`
  type to a per-signal container. Simplest realisation: a small tagged
  accessor (`read(id) -> u128`, `write(id, u128)`) that dispatches on the
  signal's container size and widens to a common evaluation type. Evaluation
  arithmetic stays in a wide accumulator; only storage is sized.
- **LLVM (`siox-llvm`).** `state_globals` stops emitting three uniform
  `array_type(i64, n)` globals and instead emits per-container-size arrays (or
  packed banks). `sx_set`/`sx_read` gain a sized load/store plus, for sub-byte,
  a shift/mask — patterns LLVM optimises well. The `event` array becoming a
  bitset is a self-contained, high-value change.
- **Waveforms (`siox-wave`).** Unaffected: traces record logical values, which
  are identical regardless of container.
- **Differential harness.** The JIT-vs-interpreter oracle is the natural
  guardrail: any container scheme must produce bit-identical results, so the
  existing differential tests directly validate that packing preserved
  exactness.

---

## 7. Exactness guarantee (restating the principle)

Container sizing must be **observationally invisible**:

- every signal's logical value is preserved exactly (a sized read then widen
  equals the value a full slot would have held);
- delta-cycle fixpoint order and event firing are unchanged (packing must not
  merge two signals' event flags, nor reorder settles);
- waveform output is byte-identical.

The moment a container choice would change any observed value or event, it is
rejected. This is what keeps the optimization aligned with "exact simulation
first": it is a pure storage decision under an unchanged semantic model.

---

## 8. Recommended path

1. **`event` as a bitset.** Self-contained, likely the biggest single win (it is
   one of the hottest arrays and morally 1 bit/signal). Low risk.
2. **Byte-granular `cur`/`old` containers.** Size each signal to `u8`/`u16`/
   `u32`/`u64`/`u128`. Direct addressing, no masking, large footprint drop.
   Validate against the differential harness.
3. **Measure.** Benchmark a small-signal-heavy design (an FSM farm, a wide bus of
   `Logic`) for memory and settle throughput. Decide from data whether to go
   further.
4. **Sub-byte packing of `cur`/`old`** and/or **SoA-by-width**, only if the
   measurements show remaining memory-bound headroom that justifies the
   per-access and addressing complexity.

Everything here is a **backend/simulator** change. No language, IR, or std
change is required, and it can land incrementally behind the current uniform-slot
behaviour (e.g. a `--pack off|bytes|bits` switch mirroring `--slot`).

---

## 9. Open questions

- **Access cost vs footprint crossover.** At what design size does sub-byte
  packing's masking overhead stop paying for itself? Needs measurement, not
  assumption.
- **Alignment for the JIT.** Sub-byte packing plus a future threaded simulator
  raises false-sharing questions; byte/word containers sidestep them.
- **128-bit and beyond.** Wide vectors already use `u128`; do very wide signals
  (>128) want their own strided container, or stay out of scope?
- **`--pack` default.** Ship byte-granular as the default (pure win) and keep
  uniform slots available for debugging, or start opt-in?

---

## 10. Summary

Signals are sized minimally in *logic* but stored uniformly in *64/128-bit
slots*. Enums — fixed, dense, tiny, and hot — are the clearest case for closing
that gap by holding each value in the smallest container that fits (1/2/4/8/…
bits). Byte-granular containers plus an `event` bitset are a near-pure win
(large memory and cache-footprint reduction, no per-access penalty); sub-byte
packing is a measured, optional next step. Crucially, all of it is a **lossless
storage optimization** beneath an unchanged, exact simulation model — the
simulator still simulates exactly; it just carries less weight while doing so.
