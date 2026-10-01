# Pipelined functions: `#[pipeline]`

Status: **proposal**. Nothing here is implemented. It defines the
`#[pipeline]` directive that [rtl-interface.md](rtl-interface.md) uses as an
example, following [Spade](https://spade-lang.org/)'s pipelines, which make
stage boundaries part of the source and check latency at every use.

A siox function is a pure expression, inlined where it is called. That is
combinational logic: one clock cycle, however deep the arithmetic. A pipelined
datapath today is written by hand as an entity with one `let` register per
stage per value, every value carried through every stage it crosses, and the
latency known only to the person who wrote it. Change the stage count and
every user's timing silently shifts by a cycle.

## Decision

```siox
#[pipeline(3)]
fn mac(clk: Bit, a: signed[16], b: signed[16], acc: signed[32]) -> signed[32] {
    let product: signed[32] = signed[32](sext(a)) * signed[32](sext(b));
    reg;
    let sum: signed[32] = product + acc;
    reg * 2;
    return sum;
}

impl Filter {
    #[pipeline(3)]
    y = mac(clk, x, coefficient, offset);   // y is mac's result three cycles later
}
```

- `#[pipeline(N)]` on a `fn` makes it a pipeline of depth `N`: `N` register
  stages between its inputs and its result. The first parameter is the clock;
  every register updates on its rising edge.
- `reg;` inside the body ends a stage. Every value visible above it is
  registered, and below it a name refers to that registered, one-cycle-later
  value. `reg * K;` inserts `K` stages at once.
- The number of `reg` stages on every path to `return` must equal `N`
  (`E-P0xx` otherwise), so the declared depth is checked against the body.
- Every call site states the depth it expects, with the same directive on the
  statement that contains the call: `#[pipeline(3)] y = mac(…);`. A missing or
  different depth is an error naming the declared one. This is Spade's
  `inst(N)`: changing a pipeline's depth breaks every user at compile time
  instead of shifting their timing.
- A pipelined function is called only where hardware is built: concurrently
  in an entity implementation, or inside another pipelined function. Calling
  one in a process, an ordinary function or a testbench is an error.

## Why this shape

**A directive, not a new item kind.** Spade has three unit kinds (`fn`,
`entity`, `pipeline`). siox already has `fn` and `entity`, and a pipeline is a
function whose evaluation is spread over clock cycles. `#[pipeline]` passes the
directive test in [language §3.5](../language.md): removing it changes what the
compiler emits.

**Depth at the call site.** The latency of a pipeline is part of its
interface, like a port's type. Writing it at each use makes the timing
visible where the result is consumed, and makes a depth change a compile error
rather than a simulation surprise.

**`reg` marks stages; the compiler carries values.** The designer decides where
the stage boundaries are; the compiler inserts the registers for every value
that crosses one, so a value used three stages later is delayed exactly three
times without being named three times.

## Stages and references

A stage takes a VHDL-style label, written on the `reg` that starts it:

```siox
#[pipeline(2)]
fn decode(clk: Bit, word: unsigned[32]) -> Op {
    let opcode: unsigned[7] = word[6..0];
    execute: reg;                          // the stage after this is `execute`
    let rd: unsigned[5] = word[11..7];
    reg;
    return Op { .opcode = opcode, .rd = rd, .imm = stage(execute).word[31..20] };
}
```

- A name always means its value *in the current stage*.
- `stage(label).x` reads `x` as it is in the labelled stage. `stage(-1).x`
  and `stage(+1).x` are relative. Reading a later stage is how a result is
  forwarded backwards (bypass, feedback); the compiler builds the wire, and the
  designer owns the hazard.
- Using a value before the stage that computes it is an error that says how
  many stages early it is (Spade's "is unavailable for another 2 stages").

## Stalls

```siox
#[pipeline(3)]
fn fetch(clk: Bit, pc: unsigned[32], mem_ready: Bit) -> unsigned[32] {
    let address: unsigned[32] = pc;
    reg[mem_ready];                       // this stage only advances when memory is ready
    …
}
```

- `reg[cond];` enables that stage's registers only while `cond` holds; while it
  is false they keep their value. A stalled stage stalls every stage before
  it, as in Spade.
- `stage'ready` reads whether the current stage will accept new input this
  cycle; `stage'valid` reads whether its contents are real (not a bubble left
  by a stall). Following siox's sigils, these are system attributes of the
  stage, `'` not `.`.
- Valid bits power on false, because every siox signal starts at its type's
  default (`Bool` is `false`), so a pipeline needs no reset to start with
  bubbles.

## Lowering

A pipelined call lowers like any inlined function call, with one register
block per `reg` boundary: for each value live across the boundary, a signal
updated on `clk.rising()` (enabled by the `reg[cond]` condition and the
stall chain). In the IR these are ordinary event-controlled updates, so
simulation, waveforms and the debugger see them as signals named by call site
and stage (the exact path is an open question below).
Nothing new reaches the runtime.

For synthesis ([rtl-interface.md](rtl-interface.md)), the registers are
explicit RTL registers and the backend never sees `#[pipeline]`.

## Not proposed

- **Automatic stage placement.** `#[pipeline(3)]` with no `reg` statements does
  not ask the compiler to balance the logic into three stages; it is an error.
  Retiming needs timing information siox does not have, and synthesis tools
  already do it.
- **Pipelines with internal state** beyond the stage registers (accumulators,
  counters). Those are entities.
- **Several clocks** in one pipeline. A clock-domain crossing is a
  synchronizer entity, not a stage.

## Open questions

- **The clock parameter.** First parameter by position, or named in the
  directive (`#[pipeline(3, clock = clk)]`)? A falling-edge pipeline would need
  the latter.
- **Calls inside expressions.** The call-site directive sits on a statement.
  Should a statement with two pipelined calls be an error, or should the
  directive list each callee (`#[pipeline(mac = 3, scale = 1)]`)?
- **Stage references syntax.** `stage(label).x` is Spade's spelling. A siox
  alternative is a system attribute with an argument, `x'stage(label)`, which
  has no precedent yet.
- **Names in the waveform.** How are the inserted registers named, so a stage
  is findable in a VCD?
- **Generic depth.** Should a pipeline's depth be able to follow a generic
  parameter (`#[pipeline(W / 8)]`), with call sites stating the same
  expression?

## References

- Spade: [spade-lang.org](https://spade-lang.org/), and the paper
  [Spade: An Expression-Based HDL With Pipelines](https://arxiv.org/abs/2304.03079)
  (pipeline depth in the head, `reg;` stage separators, `inst(N)` depth
  checking, stage references, availability errors). Stalling with `reg[cond]`,
  `stage.ready` and `stage.valid` comes from later Spade releases.
