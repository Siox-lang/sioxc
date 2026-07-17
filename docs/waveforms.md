# Waveforms

`sioxc` records simulation traces as [VCD](https://en.wikipedia.org/wiki/Value_change_dump)
(Value Change Dump) — the standard format every digital waveform viewer reads.
siox does not ship its own viewer; it writes a VCD and you open it in an
existing one.

## Producing a trace

```bash
sioxc sim counter.siox --wave counter.vcd
```

This elaborates the design, runs the first `#[test]` entity, and writes every
signal's value changes to `counter.vcd` with real timestamps (femtoseconds).

## Viewing it

Any VCD viewer works. Two good open-source ones:

- **[Surfer](https://surfer-project.org/)** — modern, fast, written in Rust,
  runs natively or in the browser. `surfer counter.vcd`.
- **[GTKWave](https://gtkwave.sourceforge.net/)** — the long-standing
  workhorse. `gtkwave counter.vcd`.

## How siox values appear

- **Buses** (`uint[8]`, `int[16]`) are binary vectors.
- **Four-value logic** (`Logic`, `Bit`) dumps as native VCD scalar
  states, so high-impedance shows as `z` and unknown as `x`, not as a number.
- **Named enums** — an FSM `State`, `Bool` — dump as VCD `string` variables, so
  the viewer shows `Idle`/`Run`/`Done`/`true`/`false` instead of a raw
  discriminant. (This uses the de-facto VCD string extension that Surfer and
  GTKWave both understand.)
- **Struct and array signals** flatten to one trace per leaf (`p.valid`,
  `regs[2]`).

## Notes

- The timescale is `1fs`; a `10ns` clock period shows as `#10000000` between
  edges.
- Only signals that actually change are re-emitted, so traces stay compact.
- FST (GTKWave's compressed format) for very large designs is a future addition;
  VCD is the current output.
