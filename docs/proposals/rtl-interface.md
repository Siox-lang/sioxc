# Vendor-neutral RTL interchange

Status: **proposal**. Nothing here is implemented.

Siox should not treat Verilog, SystemVerilog, or VHDL as its compiler IR.

Those languages are useful output formats because existing FPGA and ASIC tools accept them, but they are source languages with their own syntax, elaboration rules, type systems, and historical semantics.

By the time Siox reaches synthesis-facing RTL, those language concepts should already be gone.

The compiler should instead lower Siox into a vendor-neutral RTL representation that directly describes hardware structure:

- modules;
- ports;
- instances;
- registers;
- memories;
- combinational operations;
- clocks;
- resets;
- enables;
- muxes;
- arithmetic;
- comparisons;
- wiring;
- synthesis metadata.

SystemVerilog or VHDL generation then becomes serialization of that RTL representation rather than translation of Siox semantics into another language.

The intended architecture is:

```text
Siox source
    ↓
typed Siox IR
    ↓
elaboration
    ↓
Siox RTL IR
    ↓
RTL legalization
    ↓
backend
    ├── SystemVerilog
    ├── VHDL
    ├── CIRCT
    ├── Yosys
    └── future vendor interfaces
```

The RTL representation is the compiler boundary.

HDL source is only one possible way to transport that representation into existing synthesis tools.

## Decision

Siox defines a vendor-neutral synthesis-facing RTL IR.

The IR describes elaborated hardware rather than Siox source semantics.

A backend may serialize that IR into a form accepted by an external synthesis tool.

Initially, the primary backend may be SystemVerilog because commercial and open-source synthesis tools commonly accept it.

This does **not** make SystemVerilog the semantic target of Siox.

The relationship is:

```text
Siox semantics
    ↓
Siox RTL
    ↓
SystemVerilog serialization
```

not:

```text
Siox semantics
    ↓
SystemVerilog semantics
```

The distinction matters because no SystemVerilog concept should become part of Siox merely because the initial synthesis backend uses SystemVerilog.

## Why an RTL IR exists

A compiler should not need to reconstruct source-language abstractions after they have already been resolved.

Consider:

```siox
if clk.rising() {
    q = a + b;
}
```

The relevant hardware after elaboration is not an `if` statement in another language.

It is approximately:

```text
register q
    clock = clk
    edge = rising
    next = add(a, b)
```

Likewise:

```siox
let y = if sel { a } else { b };
```

is not fundamentally a conditional source statement.

It is:

```text
y = mux(sel, a, b)
```

And:

```siox
#[pipeline(latency = 2)]
fn calculate(...) {
    ...
}
```

should have already become explicit pipeline registers before RTL serialization begins.

The compiler backend should therefore consume structures such as:

```text
Register
Mux
Add
Multiply
Compare
Memory
Instance
Wire
Port
```

rather than high-level Siox syntax.

## What disappears before RTL

The synthesis-facing RTL IR should contain no unresolved Siox language semantics.

The following belong to earlier compiler stages:

- macro expansion;
- trait resolution;
- operator lookup;
- view resolution;
- overload resolution;
- generic specialization;
- declarative attribute lookup;
- compiler directive processing;
- type inference;
- nominal conversion rules;
- target-dependent constant folding;
- simulation-only reachability;
- delta-cycle source semantics;
- `'old`;
- `'event`;
- user-facing pipeline directives.

For example:

```siox
a + b
```

must no longer mean:

> resolve `Operator<"+", ...>`.

At RTL level it already means:

```text
Add(a, b)
```

with known operand widths and result width.

Similarly:

```siox
#[pipeline(latency = 3)]
fn f(...) { ... }
```

must not survive as an attribute in the RTL IR.

The pipeline transformation has already produced explicit registers and latency metadata.

## What remains

The RTL representation retains only information needed to describe, validate, optimize, serialize, or analyse the resulting digital hardware.

A design may contain concepts such as:

```text
Design
Module
Port
Instance
Net
Register
Memory
Clock
Reset
Constant
Operation
Attribute
SourceLocation
```

The exact implementation representation is not fixed by this proposal.

The semantic categories are.

## Modules

A module represents one elaborated reusable hardware unit.

Conceptually:

```text
Module {
    name
    inputs
    outputs
    nets
    registers
    memories
    instances
    operations
    attributes
}
```

A module does not contain Siox functions, traits, views, or macros.

Those constructs have already contributed to the hardware represented by the module.

## Ports

Ports have explicit direction and type/layout.

For example:

```text
Port {
    name: "data"
    direction: input
    width: 32
}
```

Directional Siox range information should be retained where it affects interface identity or downstream emission.

For example:

```siox
Bit[7..0]
```

and:

```siox
Bit[0..7]
```

may have the same storage width but should not silently lose their external indexing convention if that information matters to emitted interfaces.

Internal operations may use canonical bit numbering where appropriate.

The boundary between source labels and canonical RTL representation must therefore be explicit.

## Registers

Sequential state should be represented directly.

For example:

```text
Register {
    name: q
    type: i32

    clock: clk
    edge: rising

    reset: rst
    reset_kind: synchronous
    reset_value: 0

    enable: en
    next: value
}
```

A backend can then generate equivalent syntax for its target.

SystemVerilog:

```systemverilog
always_ff @(posedge clk) begin
    if (rst)
        q <= 32'd0;
    else if (en)
        q <= value;
end
```

VHDL:

```vhdl
process(clk)
begin
    if rising_edge(clk) then
        if rst = '1' then
            q <= (others => '0');
        elsif en = '1' then
            q <= value;
        end if;
    end if;
end process;
```

The two backend outputs differ syntactically.

The RTL object does not.

## Combinational operations

Combinational logic should use explicit operations.

For example:

```text
%sum = Add %a, %b
%eq  = Equal %x, %y
%out = Mux %sel, %sum, %fallback
```

Operations should carry enough type information to make backend emission mechanical:

```text
Add {
    lhs
    rhs
    width
    signedness
}
```

Whether signedness is encoded in the operation, type, or both is an implementation detail.

What should not happen is for a backend to rediscover Siox type semantics.

## Memories

Memories should remain first-class RTL objects long enough for downstream synthesis tools to infer appropriate implementation resources.

For example:

```text
Memory {
    width: 32
    depth: 1024

    read_ports: [...]
    write_ports: [...]
}
```

The compiler should not prematurely lower every memory into individual registers and mux trees.

Doing so could prevent downstream tools from inferring:

- block RAM;
- distributed RAM;
- vendor memory primitives;
- optimized read/write structures.

The same principle applies to arithmetic resources such as multipliers.

The IR should preserve useful semantic operations until the point where lowering is necessary.

## Arithmetic operations

An operation such as:

```siox
a * b
```

should remain recognizably a multiplication in the RTL IR:

```text
%result = Mul %a, %b
```

rather than immediately becoming a network of gates.

This leaves synthesis tools free to map it to:

- DSP blocks;
- LUT logic;
- ASIC arithmetic cells;
- optimized multiplier structures.

Siox should define hardware meaning without prematurely deciding technology mapping.

## Clock domains

Clock information must be explicit in the RTL representation.

Source code may infer clocking through constructs such as:

```siox
if clk.rising() {
    ...
}
```

but the RTL IR should no longer require inference.

It should record information such as:

```text
ClockDomain {
    source: clk
    edge: rising
    parent: none
}
```

or, for generated clocks:

```text
ClockDomain {
    source: derived_clk
    parent: clk
    relation: ...
}
```

Registers and relevant interfaces can then reference a concrete clock domain.

This supports:

- CDC analysis;
- synthesis constraints;
- diagnostics;
- backend generation;
- timing metadata.

## Resets

Reset semantics should likewise be explicit.

A backend should not infer from arbitrary source structure whether a reset is:

- synchronous;
- asynchronous;
- active-high;
- active-low.

That information should already exist in the RTL representation.

For example:

```text
Reset {
    signal: rst_n
    polarity: low
    kind: asynchronous
}
```

## Pipeline directives

Compiler directives such as:

```siox
#[pipeline(latency = 3)]
fn transform(...) {
    ...
}
```

operate before RTL emission.

They may analyse:

- data dependencies;
- combinational depth;
- operation costs;
- timing constraints;
- target information.

The result is explicit RTL state.

For example:

```text
%0 = Mul %a, %b
%s0 = Register %0 on %clk

%1 = Add %s0, %c
%s1 = Register %1 on %clk

%2 = Normalize %s1
%s2 = Register %2 on %clk
```

The RTL backend sees registers.

It does not need to understand `#[pipeline]`.

This is an important general rule:

> Compiler directives may transform the RTL that is produced, but backend serializers should not reimplement those directives.

## Declarative attributes

Declarative Siox metadata may survive into RTL where downstream tools require it.

For example:

```siox
attr keep for probe = true;
```

may become:

```text
Net {
    ...
    attributes: {
        keep: true
    }
}
```

A SystemVerilog backend might emit a corresponding synthesis attribute if the selected backend supports one.

Another backend may map the same metadata differently.

The RTL representation therefore retains semantic metadata without adopting vendor-specific spelling.

For example:

```text
keep = true
```

is preferable internally to:

```text
(* keep = "true" *)
```

because the latter is SystemVerilog/vendor syntax.

## Vendor-specific metadata

Some hardware eventually requires vendor-specific information.

That information should still be represented separately from the core RTL structure.

For example:

```text
attributes {
    vendor.xilinx.foo = ...
}
```

may exist where necessary.

However, common hardware concepts should not be forced through vendor-specific metadata.

Clocks, resets, registers, memories, timing relationships, and basic synthesis intent deserve proper IR representation where possible.

## Simulation semantics

The synthesis-facing RTL IR is not required to represent every Siox simulation feature directly.

Siox supports behavior such as:

- delta cycles;
- resolution;
- `'event`;
- `'old`;
- simulation-only entities;
- foreign calls;
- file access.

The native simulation path may operate on a richer design representation.

Conceptually:

```text
                  elaborated Siox design
                         │
              ┌──────────┴──────────┐
              ▼                     ▼
       simulation lowering      RTL lowering
              │                     │
              ▼                     ▼
      native simulator          Siox RTL IR
```

This avoids forcing synthesis-oriented RTL to represent behavior that cannot exist in the selected synthesis hierarchy.

Resolution or event semantics that do contribute to synthesizable hardware must be lowered explicitly before or during RTL lowering.

## RTL legalization

The RTL IR should be more expressive than the narrowest supported external HDL subset.

A legalization stage converts general RTL into constructs supported by a particular backend.

For example:

```text
Siox RTL
   ↓
SystemVerilog legalization
   ↓
portable SV subset
```

or:

```text
Siox RTL
   ↓
VHDL legalization
   ↓
portable VHDL subset
```

Legalization may include:

- splitting unsupported aggregate operations;
- canonicalizing reset forms;
- flattening unsupported types;
- materializing temporary nets;
- lowering unsupported memory configurations;
- rewriting expressions for tool compatibility.

The important boundary is that legalization changes representation, not Siox semantics.

## SystemVerilog backend

The initial synthesis backend should emit a deliberately small and conservative subset of SystemVerilog.

The goal is not to reproduce Siox abstractions in SystemVerilog.

The goal is to serialize elaborated RTL.

Prefer constructs such as:

```text
module
input
output
logic
assign
always_ff
if
case
basic expressions
instances
memories
```

Avoid depending unnecessarily on complex SystemVerilog features such as:

- classes;
- dynamic arrays;
- interfaces;
- high-level generative metaprogramming;
- complex package behavior;
- language features with inconsistent synthesis support.

The output should resemble RTL assembly more than human-authored SystemVerilog.

This reduces backend complexity and improves portability across synthesis tools.

## VHDL backend

A VHDL backend may serialize the same RTL representation.

For example:

```text
Register(%q, %d, rising %clk)
```

may become:

```systemverilog
always_ff @(posedge clk)
    q <= d;
```

or:

```vhdl
process(clk)
begin
    if rising_edge(clk) then
        q <= d;
    end if;
end process;
```

No Siox semantic transformation is required at this stage.

Both are textual encodings of the same RTL operation.

VHDL support is therefore a backend concern rather than a separate compiler frontend architecture.

## CIRCT backend

CIRCT may be a useful backend or interoperability target.

A Siox RTL operation such as:

```text
%sum = Add %a, %b
```

may map naturally to a CIRCT combinational operation.

Registers, modules, instances, and other RTL constructs likewise have close equivalents in hardware-oriented CIRCT dialects.

However, Siox semantics should not be defined in terms of CIRCT.

The relationship should remain:

```text
Siox semantics
    ↓
Siox elaborated design
    ↓
Siox RTL
    ↓
CIRCT
```

rather than:

```text
Siox semantics = CIRCT semantics
```

This keeps the language independent of one external compiler framework.

## Yosys backend

Open-source flows may benefit from integration with Yosys.

That integration could initially happen through generated Verilog/SystemVerilog.

A later backend may target a lower-level Yosys-compatible representation if doing so provides enough benefit.

Again, this should not affect the Siox language model.

## Stable RTL interchange format

The RTL representation may eventually have a stable serialized form.

For example:

```text
.srtl
```

or another dedicated extension.

Conceptually:

```text
sioxc design.siox --emit=rtl
```

could produce a machine-readable elaborated RTL artifact.

That artifact could then be consumed by independent tools:

```text
siox-sv
siox-vhdl
siox-circt
siox-formal
siox-lint
siox-graph
vendor adapters
```

This would make the RTL representation a genuine interoperability boundary rather than only an internal compiler data structure.

A stable external format should not be committed to until the internal representation has matured.

Internal evolution should remain cheap during early compiler development.

## Why not compile directly to SystemVerilog AST

One possible implementation would lower Siox directly into a SystemVerilog AST.

That would simplify the first backend but create the wrong architectural dependency.

Siox concepts would then tend to acquire SystemVerilog-shaped implementations:

```text
Siox construct
    ↓
which SystemVerilog construct represents this?
```

The preferred question is:

```text
Siox construct
    ↓
what hardware does this mean?
    ↓
how does this backend serialize that hardware?
```

The latter keeps language semantics independent of output syntax.

## Why not compile directly to VHDL

The same argument applies.

VHDL has excellent hardware semantics and broad tool support, but it is still another HDL source language.

Using it as Siox's primary semantic target would cause Siox lowering to depend unnecessarily on:

- VHDL process structure;
- signal semantics;
- VHDL type rules;
- package rules;
- source-level elaboration constructs.

Those are not necessary once Siox has already elaborated its own design.

## Why not lower directly to gates

The opposite extreme is also undesirable.

Siox could theoretically lower:

```text
Add
Mul
Memory
Mux
```

all the way into:

```text
AND
OR
XOR
LUT
FF
```

or technology-specific primitives.

Doing so would make Siox responsible for much of logic synthesis.

That includes problems such as:

- arithmetic decomposition;
- boolean optimization;
- technology mapping;
- DSP inference;
- RAM inference;
- resource sharing;
- retiming;
- vendor cell selection.

Existing synthesis tools already solve these problems and often have detailed knowledge of their target devices.

Siox should therefore stop at a sufficiently expressive RTL boundary.

The desired level is:

```text
hardware structure explicit
technology mapping undecided
```

## Preserve inference opportunities

RTL lowering must avoid destroying useful synthesis intent.

For example:

```siox
let x = a * b;
```

should generally remain:

```text
Mul(a, b)
```

rather than immediately becoming partial products and adders.

Likewise:

```siox
memory[addr]
```

should remain a memory operation where possible rather than immediately becoming a mux over registers.

This allows downstream synthesis tools to infer:

- DSP blocks;
- block RAM;
- distributed RAM;
- carry chains;
- vendor-optimized structures.

Vendor neutrality does not require discarding semantic information.

It requires representing that information independently of one vendor.

## Source mapping

RTL nodes should preserve source provenance.

For example:

```text
Register stage1
    source = calculator.siox:42
    generated_by = pipeline directive at calculator.siox:31
```

This allows downstream diagnostics to map generated RTL back to Siox source.

That becomes especially important for:

- macros;
- compiler-generated pipelines;
- derived implementations;
- generated resets;
- flattened structures.

A backend error referring only to `stage1_reg_14` is much less useful than one that can identify the Siox construct that produced it.

## Inspectability

Users should be able to inspect the synthesis-facing design.

At minimum, the compiler should eventually support output analogous to:

```text
sioxc --emit=rtl
```

This should show the hardware after:

- macro expansion;
- type checking;
- elaboration;
- compiler directives;
- pipeline transformation;

but before target-specific textual serialization.

That representation becomes the clearest answer to:

> What hardware did Siox actually generate?

This is especially valuable when compiler directives are allowed to perform structural transformations.

## Backend independence

No backend should be allowed to redefine Siox semantics.

Given one elaborated RTL design, a SystemVerilog backend and a VHDL backend should describe equivalent hardware.

Backend differences may exist where external tool capabilities differ, but those differences should be explicit legalization or compatibility issues.

The compiler should diagnose when a backend cannot faithfully represent required RTL.

It should not silently reinterpret the design.

## Potential optimization boundary

The RTL IR also creates a natural location for target-independent hardware optimizations.

Possible passes include:

```text
constant propagation
dead-net elimination
mux simplification
register cleanup
width reduction
common expression elimination
simple pipeline analysis
```

These optimizations operate on explicit hardware rather than Siox syntax.

Target-specific optimization should generally remain with the downstream synthesis system unless Siox has a strong reason to perform it itself.

## Compiler architecture

A possible long-term compiler structure is:

```text
Siox source
    ↓
lexer / parser
    ↓
AST
    ↓
macro expansion
    ↓
name and type resolution
    ↓
typed semantic IR
    ↓
generic specialization
    ↓
directive processing
    ↓
constant folding
    ↓
elaboration
    ↓
Design IR
    ├── native simulation lowering
    │       ↓
    │   simulator executable
    │
    └── synthesis lowering
            ↓
        Siox RTL IR
            ↓
        optimization
            ↓
        legalization
            ↓
      ┌─────┼─────┬─────┐
      ▼     ▼     ▼     ▼
      SV   VHDL  CIRCT  future
```

The exact number of IRs is an implementation decision.

The important architectural requirement is that synthesis output passes through a vendor-neutral hardware representation before any textual HDL backend.

## Relationship to external tools

The initial practical flow may still be:

```text
Siox
  ↓
Siox RTL
  ↓
SystemVerilog
  ↓
Vivado / Quartus / Yosys
```

That does not undermine the design.

The final SystemVerilog step exists because current tools commonly expose HDL source frontends rather than one shared vendor-neutral RTL interchange frontend.

If future tools accept a suitable structured RTL representation directly, Siox can add such a backend without changing language semantics or earlier compiler stages.

The architecture is therefore designed around the hardware model rather than around current tool limitations.

## Implementation order

A practical implementation path is:

1. Define the minimum synthesis-facing RTL object model.
2. Lower simple combinational Siox designs into it.
3. Add registers, clocks, resets, and enables.
4. Add instances.
5. Add memories.
6. Add source-location tracking.
7. Add declarative RTL metadata.
8. Build a minimal SystemVerilog serializer.
9. Validate output against existing synthesis tools.
10. Add legalization passes where portability requires them.
11. Add `--emit=rtl` for inspection.
12. Add additional backends only when useful.
13. Consider stabilizing an external RTL file format after the internal model has matured.

The initial goal should not be to model every possible FPGA or ASIC construct.

It should be to establish the correct architectural boundary.

## Open questions

- What is the minimum set of first-class RTL operations?
- Should the synthesis RTL use SSA, explicit nets, or a hybrid representation?
- At what stage are aggregates flattened?
- How much source range-direction information survives into internal RTL?
- How are resolved signals represented before synthesis lowering?
- Which declarative attributes survive into RTL?
- How are backend-specific attributes namespaced?
- Should clocks and resets be ordinary signals plus metadata or distinct IR object types?
- How are tri-state and bidirectional signals represented?
- How are asynchronous memories represented?
- Where does memory inference stop and explicit memory lowering begin?
- Should pipeline latency remain explicit metadata after registers have been materialized?
- What information is required for formal verification backends?
- When, if ever, should the serialized RTL format become a stable public interface?
- Should CIRCT be an optional backend or an internal implementation dependency?
- How much optimization belongs in Siox before handing the design to vendor synthesis?

These questions affect representation details but not the central architectural decision.

## Non-goals

- Replacing Vivado, Quartus, Yosys, or ASIC synthesis tools.
- Performing full technology mapping.
- Defining FPGA primitive libraries in the core language.
- Making SystemVerilog part of Siox semantics.
- Making VHDL part of Siox semantics.
- Requiring CIRCT.
- Stabilizing an RTL file format before the IR has matured.
- Reproducing high-level Siox abstractions in generated HDL.
- Producing hand-written-looking SystemVerilog as a primary goal.
- Performing vendor-specific optimization in the core compiler.
- Guaranteeing identical textual output across backends.

## Bottom line

Siox should compile to hardware before it compiles to another HDL.

The central pipeline is:

```text
Siox
    ↓
elaborated hardware
    ↓
vendor-neutral RTL
```

Everything after that is transport:

```text
RTL
 ├── SystemVerilog
 ├── VHDL
 ├── CIRCT
 ├── Yosys
 └── future interfaces
```

This keeps the compiler centered on hardware rather than another source language.

It also prevents SystemVerilog or VHDL limitations from leaking backward into Siox design.

The guiding rule is:

> **Siox lowering answers what hardware the program describes. A backend answers how that hardware is presented to another tool.**

That separation should remain true even if SystemVerilog is initially the only synthesis backend.
