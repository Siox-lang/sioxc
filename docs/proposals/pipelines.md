# Pipelined functions: `#[latched]` and `#[latch]`

Status: **proposal**. Nothing here is implemented. It follows
[Spade](https://spade-lang.org/)'s pipelines, which make stage boundaries part
of the source and check latency at every use, but spells them with two
compiler directives instead of new keywords.

A siox function is a pure expression, inlined where it is called. That is
combinational logic: one clock cycle, however deep the arithmetic. A pipelined
datapath today is written by hand as an entity with one `let` register per
stage per value, every value carried through every stage it crosses, and the
latency known only to the person who wrote it. Change the stage count and
every user's timing silently shifts by a cycle.

## Decision

```siox
#[latched]
fn mac(clk: Bit, a: signed[16], b: signed[16], acc: signed[32]) -> signed[32] {
    #[latch] {                                   // stage 1
        let product: signed[32] = signed[32](sext(a)) * signed[32](sext(b));
    }
    #[latch]                                     // stage 2
    let sum: signed[32] = product + acc;
    return sum;                                  // available after two stages
}

impl Filter {
    #[latched(2)]
    y = mac(clk, x, coefficient, offset);        // y is mac's result two cycles later
}
```

- `#[latched]` on a `fn` makes it a pipeline: its result arrives a fixed number
  of clock cycles after its inputs. The first parameter is the clock; every
  stage register updates on its rising edge.
- `#[latch]` inside a latched function marks one stage. On a block,
  `#[latch] { … }`, the stage is everything in the block; without braces, it is
  the one statement that follows. At the end of a stage every value it
  computed, and every value it carries from earlier, is registered.
- A name always means its value *in the current stage*. A `let` inside a
  `#[latch] { … }` block stays visible after the block, delayed by the stage:
  a stage is a step in time, not a scope.
- The depth is the number of `#[latch]` stages on the way to `return`. Every
  path must have the same depth (`E-P0xx` otherwise). `#[latched(N)]` on the
  declaration states it, and the compiler checks the count matches.
- Every call site states the depth it expects, with the same directive on the
  statement that contains the call: `#[latched(2)] y = mac(…);`. A missing or
  different depth is an error naming the declared one. This is Spade's
  `inst(N)`: changing a pipeline's depth breaks every user at compile time
  instead of shifting their timing.
- A latched function is called only where hardware is built: concurrently in
  an entity implementation, or inside another latched function. Calling one in
  a process, an ordinary function or a testbench is an error.

## Why directives

**No new keywords.** A stage boundary is not new computation; it is an
instruction about *when* computation's results are kept. That is what
directives are for: `#[...]` marks what changes what the compiler emits
(language §3.5), and both markers do exactly that. Removing them turns a
pipeline back into combinational logic.

**The body stays ordinary siox.** Inside a latched function every statement is
a normal statement with its normal meaning; the directives only say where the
registers go. A reader who ignores the `#[...]` lines reads the computation.

**Blocks and single statements.** Most stages are one statement, and
`#[latch]` before it is the lightest marker possible. A stage that needs
several statements takes a block, as a lint directive applies to a block or a
single statement today.

**Depth at the call site.** The latency of a pipeline is part of its
interface, like a port's type. Writing it at each use makes the timing visible
where the result is consumed, and makes a depth change a compile error.

## Stage names and references

`#[latch]` takes two optional named arguments: `name` names the stage, and
`enable` stalls it (below). Other stages can read a named stage's values:

```siox
#[latched(2)]
fn decode(clk: Bit, word: unsigned[32]) -> Op {
    #[latch(name = fetch)]
    let opcode: unsigned[7] = word[6..0];
    #[latch] {
        let rd: unsigned[5] = word[11..7];
        let imm: unsigned[12] = stage(fetch).word[31..20];
    }
    return Op { .opcode = opcode, .rd = rd, .imm = imm };
}
```

- `stage(fetch).x` reads `x` as it is in the named stage, and
  `stage(-1).x` / `stage(+1).x` are relative. Reading a later stage is how a
  result is forwarded backwards (bypass, feedback); the compiler builds the
  wire, and the designer owns the hazard.
- Using a value before the stage that computes it is an error that says how
  many stages early it is ("`imm` is unavailable for another 1 stage").

## Stalls

```siox
#[latched(3)]
fn fetch(clk: Bit, pc: unsigned[32], mem_ready: Bit) -> unsigned[32] {
    #[latch(enable = mem_ready)]                 // this stage only advances when memory is ready
    let address: unsigned[32] = pc;
    …
}
```

- `#[latch(enable = cond)]` enables that stage's registers only while `cond` holds;
  while it is false they keep their value. A stalled stage stalls every stage
  before it, as in Spade.
- `stage'ready` reads whether the current stage will accept new input this
  cycle; `stage'valid` reads whether its contents are real (not a bubble left
  by a stall). Following siox's sigils, these are system attributes of the
  stage.
- Valid bits power on false, because every siox signal starts at its type's
  default (`Bool` is `false`), so a pipeline needs no reset to start with
  bubbles.

## Lowering

A latched call lowers like any inlined function call, with one register block
per stage: for each value live across a stage boundary, a signal updated on
`clk.rising()` (enabled by the stage's condition and the stall chain). In the
IR these are ordinary event-controlled updates, so simulation, waveforms and
the debugger see them as signals named by call site and stage. Nothing new
reaches the runtime.

For synthesis ([rtl-interface.md](rtl-interface.md)), the registers are
explicit RTL registers and no backend needs to understand the directives.

## Not proposed

- **Automatic stage placement.** `#[latched(3)]` on a function with no
  `#[latch]` stages does not ask the compiler to balance the logic into three
  stages; it is an error. Retiming needs timing information siox does not
  have, and synthesis tools already do it.
- **Pipelines with internal state** beyond the stage registers (accumulators,
  counters). Those are entities.
- **Several clocks** in one pipeline. A clock-domain crossing is a
  synchronizer entity, not a stage.

## Open questions

- **The names.** In hardware a *latch* is a level-sensitive storage element,
  transparent while enabled, and siox's `possible_latch` lint warns about
  exactly that. A pipeline stage is an edge-triggered register. Engineers may
  read `#[latch]` as asking for a real latch. Alternatives that keep the
  directive design: `#[pipelined]` / `#[stage]`, or `#[registered]` / `#[reg]`.
- **The clock parameter.** First parameter by position, or named
  (`#[latched(2, clock = clk)]`)? A falling-edge pipeline would need the
  latter.
- **Calls inside expressions.** The call-site directive sits on a statement.
  Should a statement with two latched calls be an error, or should the
  directive list each callee (`#[latched(mac = 2, scale = 1)]`)?
- **Stage references syntax.** `stage(name).x` is Spade's spelling. A siox
  alternative is a system attribute with an argument, `x'stage(label)`, which
  has no precedent yet.
- **Names in the waveform.** How are the inserted registers named, so a stage
  is findable in a VCD?
- **Generic depth.** Should a depth be able to follow a generic parameter
  (`#[latched(W / 8)]`), with call sites stating the same expression?

## References

- Spade: [spade-lang.org](https://spade-lang.org/), and the paper
  [Spade: An Expression-Based HDL With Pipelines](https://arxiv.org/abs/2304.03079)
  (pipeline depth in the head, `reg;` stage separators, `inst(N)` depth
  checking, stage references, availability errors). Stalling with `reg[cond]`,
  `stage.ready` and `stage.valid` comes from later Spade releases.
