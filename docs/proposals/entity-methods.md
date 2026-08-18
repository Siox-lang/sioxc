# Public methods on entities

Status: **proposal**. Not implemented; `pub fn` on an entity `impl` is
currently rejected (`E-P024`).

An entity's interface is its ports. Reaching a child instance therefore means
declaring a port for every value that crosses the boundary and wiring each one
at the instantiation, even when those ports only exist to carry one protocol.
This proposes letting an entity expose `pub fn` members whose calls *elaborate
into* those ports, so the protocol is written once beside the state it governs
rather than re-wired at every instantiation.

## What this does and does not claim

It does **not** remove wires. Writing to a child's internal state from outside
requires data and an enable to cross the module boundary; that is true of any
synthesizable HDL and nothing here changes it. The generated ports are real,
appear in the elaborated design, and cost exactly what the hand-written ones
cost.

What it removes is the *hand-declared and hand-wired* port. In the FIFO example
below the port count is unchanged — `push`, `din`, `accepted` still exist after
elaboration — but they are derived from one method signature instead of being
declared in the header, connected at the instantiation, and kept consistent by
the author. The saving is in authoring and in the class of mistake where a
protocol is wired subtly wrong at one of its call sites.

## Motivation

A FIFO whose write port reports whether the write was taken, as it must be
written today:

```siox
entity Fifo {
    clk: Bit in, push: Bit in, din: unsigned[8] in, pop: Bit in,
    accepted: Bit out, dout: unsigned[8] out, level: unsigned[4] out
}

entity Producer {
    clk: Bit in, accepted: Bit in,
    push: Bit out, din: unsigned[8] out, sent: unsigned[8] out
}
impl Producer {
    push = '1';                  // always offering
    din = value;
    if clk.rising() {
        if accepted == '1' { value = value + 11; count = count + 1; }
    }
}
```

The three signals `push`/`din`/`accepted` are one protocol — offer a value,
learn whether it was taken — spread across two entity headers and an
instantiation. Every consumer of `Fifo` repeats it, and a consumer that ties
`push` high while ignoring `accepted` silently drops data.

With the proposal:

```siox
impl Fifo {
    pub fn write(self, value: unsigned[8]) -> Bool { ... }
}

impl Producer {
    if clk.rising() {
        if f.write(value) { value = value + 11; count = count + 1; }
    }
}
```

## Elaboration

A call materialises one port per element of the signature, on the child, and
the matching connection in the parent:

| method element         | materialised port      | direction (child) |
| ---------------------- | ---------------------- | ----------------- |
| the call being made    | enable                 | `in`              |
| each argument          | one port per argument  | `in`              |
| the return value       | one port               | `out`             |

The enable is the condition under which the call occurs: `'1'` for an
unconditional call, otherwise the enclosing condition. The call itself is
combinational — the enable and arguments are driven, and the result is
available in the same cycle — even when the *result* is consumed inside a
clocked block. That split is deliberate and is what makes
`if f.write(v) { ... }` inside `if clk.rising()` mean "offer every cycle, and
register the update only when it was taken".

A method with no return value materialises no result port. A method with no
arguments and no effect materialises only a result port, which is the
accessor case below.

## Precedent

This is Bluespec SystemVerilog's module-method mapping:
`method ActionValue#(Bool) write(Bit#(8) x)` compiles to `write_x` (data),
`write_EN` (enable) and `write_RDY`/result ports. The semantics are settled and
have been shipped in a synthesizable HDL for two decades; this proposal adopts
the mapping without adopting Bluespec's scheduler (see *Open questions*).

## Scope

Three tiers, separable and worth landing in this order.

**1. Associated functions (no `self`).** `pub fn clamp(v: unsigned[8]) -> unsigned[8]`
on an entity `impl`. No instance, no ports, no scheduling — a namespaced
function and nothing more. It already works when private; the current rejection
is broader than its own justification and this tier is a rule fix rather than a
feature.

**2. Pure accessors.** `pub fn is_full(self) -> Bit`, reading instance state
and returning a value. Materialises one output port. This is sugar over a port
the author would otherwise declare, and it composes with the privacy model:
`f.used` is already rejected as private implementation state, so an accessor is
the sanctioned way to expose *derived* state without exposing storage.

**3. Effectful methods.** `pub fn write(self, v: unsigned[8]) -> Bool`. The
method has an effect — it consumes a slot — and that effect is expressed
entirely through the materialised ports. "Entity methods must be pure" is
therefore the wrong constraint; the right one is that every effect crosses the
boundary as a port.

### Restriction for a first version

**One call site per method per instance**, checked at elaboration, with a
diagnostic naming the existing call site.

Two callers would need a mux on the arguments, an OR on the enable, and
arbitration when both fire in one cycle. That is the expensive half of the
design and the only half that needs new machinery. The restriction removes it
entirely — with one caller no contention can exist — and relaxing it later is
purely additive.

## Interaction with existing rules

- **Ports remain the structural interface.** Methods generate ports; they do
  not create a second kind of interface. The entity header stops being the
  whole story, so `--emit metadata` and `tree` should list materialised ports
  to keep the real interface inspectable.
- **Direction is unaffected.** Generated argument ports are `in` on the child,
  the result `out`; the entity body reads and drives them under the ordinary
  rules.
- **Privacy.** A method is the intended way to expose derived state or a
  protocol while `instance.private_state` stays rejected.
- **No `await`.** These methods are combinational and synthesizable. A
  sequence that consumes time is a different feature; that pattern is already
  expressible today by putting `pub fn` with `await` on a *view* over the
  entity's port bundle, which needs nothing new.

## Open questions

1. **Arbitration.** What multiple callers mean. Deferred by the single-caller
   restriction; Bluespec's answer is a scheduler with conflict analysis, which
   is a much larger commitment than the rest of this proposal.
2. **Naming.** What the generated ports are called in IR paths, waveforms and
   the C harness. They must be stable and injective, and they collide with
   user-declared port names unless namespaced.
3. **Depth.** Whether a call may reach beyond a direct child. Parent-to-child
   is the direction instances already flow; anything deeper needs a rule for
   the intermediate levels' ports.
4. **Generic entities.** A method on `Fifo<W>` materialises ports whose widths
   depend on the specialization, so port generation must happen after
   parameter substitution.

## Status of the current rejection

`E-P024` rejects every `pub fn` on an entity `impl`, including the no-`self`
case that tier 1 covers. If this proposal is accepted, the TODO entry should
change from "no defined hierarchy, scheduling, connectivity or synthesis
semantics" to "designed; scoped to a single caller pending an arbitration
rule", since the semantics above are defined and have a shipped precedent.
