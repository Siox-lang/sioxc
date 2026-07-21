# siox Three-Phase Roadmap

This document splits siox development into three major phases:

1. **Digital**
2. **Analogue**
3. **Design**

The main rule is:

```text
Digital and analogue define components.
Design connects components.
```

The digital and analogue layers are the **logic/modeling language**. The design layer is the **schematic/netlist/composition language** that can also support graphical design interfaces.

---

## Phase 1 — Digital Language

### Goal

Build the core HDL language and event-driven simulator.

This phase defines the digital semantics of siox: entities, implementations, structs, enums, traits, ports, assignments, event handling, and tests.

### Core Concepts

```text
entity
    hardware boundary / interface declaration

impl
    implementation/body of an entity or type

struct
    ordinary data bundle / digital structure

enum
    finite discrete state/value domain

trait
    compile-time behavior contract

using
    import / type alias

attr
    declaration of metadata attributes usable in #[...]

#[...]
    declared metadata attributes for compiler/tool/backend use

in / out / inout
    digital port direction / permission semantics

::event
    true when a digital/discrete value changed this simulation step

::old
    previous value of a digital/discrete value
```

### Entity Rules

Entity bodies are interface-only.

```siox
entity Counter<W: integer> {
    in clk: Bit;
    in rst: Logic;
    in en: Bit;

    out count: uint[W];
}
```

No `const` fields inside entities.

```siox
entity BadCounter {
    const W: integer;      // invalid
    out count: uint[W];  // invalid because W changes interface shape
}
```

Configuration values that affect shape or behavior go in the entity parameter list:

```siox
entity Counter<W: integer> {
    out count: uint[W];
}
```

### Digital System Attributes

All digital/discrete values get:

```siox
x::event
x::old
```

This includes:

```text
Bit
Logic
Bool
uint[N]
int[N]
enum
structs containing only digital fields
arrays/vectors of digital values
```

Example with an enum:

```siox
enum State {
    Idle,
    Start,
    Shift,
    Done,
}

impl Controller {
    let state: State = State::Idle;

    if state::event {
        changed = '1';
    }

    if state::old == State::Idle & state == State::Start {
        started = '1';
    }
}
```

### Clock/Event Logic

Clock edges can be expressed as derived attributes over `::event` and `::old`.

```siox
trait ClockLike {
    let rising(self);
    let falling(self);
    let edge(self);
}

impl ClockLike for Logic {
    let rising(self) {
        self::event & self::old == '0' & self == '1'
    }

    let falling(self) {
        self::event & self::old == '1' & self == '0'
    }

    let edge(self) {
        self::event
    }
}
```

Usage:

```siox
if clk.rising() {
    q = d;
}
```

The scheduler can infer that this block is event-controlled because the condition depends on `clk::event` through `clk.rising()`.

### Example: Counter

```siox
entity Counter<W: integer> {
    in clk: Bit;
    in rst: Logic;
    in en: Bit;

    out count: uint[W];
}

impl Counter<W: integer> {
    let value: uint[W] = 0;

    if clk.rising() {
        if rst == '1' {
            value = 0;
        } else if en {
            value = value + 1;
        }
    }

    count = value;
}
```

### Phase 1 Deliverables

```text
lexer/parser
AST
name resolution
type checker
entity/impl elaboration
digital event scheduler
combinational assignment semantics
sequential assignment semantics
system attributes: ::event, ::old
traits for derived digital attributes
basic test runner
VCD/FST waveform output
core diagnostics
```

---

## Phase 2 — Analogue / Mixed-Signal Language

### Goal

Add physical domains, continuous equations, derivative semantics, and mixed-signal bridges.

A `domain` marks an analogue terminal type. Regular `struct`s and ordinary datatypes are digital/discrete unless explicitly bridged.

### Core Concepts

```text
domain
    analogue/conservative terminal type

across
    quantity measured between two terminals

through
    conserved quantity flowing through a directed path

let path = a -> b
    create a directed analogue path between compatible domain terminals

path.<across>
    across quantity from a to b

path.<through>
    through quantity from a to b

::ddt
    derivative of an analogue/domain quantity
```

### Domain Rule

If a type is declared with `domain`, it is analogue.

If a type is declared with `struct`, it is ordinary/digital data unless explicitly used through a bridge.

```siox
domain Electrical<A: Analysis> {
    across v: Voltage<A>;
    through i: Current<A>;
}
```

This means:

```text
Electrical<A>
    analogue terminal type

p -> n
    creates a directed electrical path

path.v
    voltage from p to n

path.i
    current from p to n

path.v::ddt
    derivative of voltage

path.i::ddt
    derivative of current
```

The terminal itself does not store `v` or `i`. The quantities appear when two compatible terminals are combined into a directed path.

### Analysis Domains

Analysis/math representations can be ordinary types implementing traits.

```siox
trait Analysis {
    using Scalar;
}

struct Time;

impl Analysis for Time {
    using Scalar = real;
}

struct Phasor<F: Hertz>;

impl Analysis for Phasor<F: Hertz> {
    using Scalar = complex;
}

using Voltage<A: Analysis> = A::Scalar;
using Current<A: Analysis> = A::Scalar;
```

The same component equations can be lowered differently depending on the analysis domain:

```text
Time:
    x::ddt -> dx/dt

DiscreteTime:
    x::ddt -> finite difference / solver companion model

Phasor<F>:
    x::ddt -> jωx

Laplace:
    x::ddt -> sx

DC:
    x::ddt -> 0
```

### Example: Resistor

```siox
entity Resistor<R: Ohm, A: Analysis> {
    p: Electrical<A>;
    n: Electrical<A>;
}

impl Resistor<R: Ohm, A: Analysis> {
    let path = p -> n;

    path.i = path.v / R;
}
```

### Example: Capacitor

```siox
entity Capacitor<C: Farad, A: Analysis> {
    p: Electrical<A>;
    n: Electrical<A>;
}

impl Capacitor<C: Farad, A: Analysis> {
    let path = p -> n;

    path.i = C * path.v::ddt;
}
```

### Example: Inductor

```siox
entity Inductor<L: Henry, A: Analysis> {
    p: Electrical<A>;
    n: Electrical<A>;
}

impl Inductor<L: Henry, A: Analysis> {
    let path = p -> n;

    path.v = L * path.i::ddt;
}
```

### Other Possible Domains

The same `domain` pattern can define other physical systems.

#### Thermal

```siox
domain Thermal<A: Analysis> {
    across temp: Temperature<A>;
    through q: HeatFlow<A>;
}
```

#### Mechanical Translational

```siox
domain Translational<A: Analysis> {
    across x: Position<A>;
    through f: Force<A>;
}
```

#### Mechanical Rotational

```siox
domain Rotational<A: Analysis> {
    across theta: Angle<A>;
    through tau: Torque<A>;
}
```

#### Hydraulic

```siox
domain Hydraulic<A: Analysis> {
    across p: Pressure<A>;
    through q: VolumeFlow<A>;
}
```

#### Pneumatic

```siox
domain Pneumatic<A: Analysis> {
    across p: Pressure<A>;
    through m: MassFlow<A>;
}
```

#### Magnetic

```siox
domain Magnetic<A: Analysis> {
    across mmf: MagnetomotiveForce<A>;
    through phi: MagneticFlux<A>;
}
```

#### Acoustic

```siox
domain Acoustic<A: Analysis> {
    across p: SoundPressure<A>;
    through u: VolumeVelocity<A>;
}
```

#### Chemical

```siox
domain Chemical<A: Analysis> {
    across c: Concentration<A>;
    through j: MolarFlow<A>;
}
```

#### Electrochemical

```siox
domain Electrochemical<A: Analysis> {
    across mu: ElectrochemicalPotential<A>;
    through j: IonicCurrent<A>;
}
```

### Mixed-Signal Bridges

Analogue and digital values should not silently mix.

Use explicit bridges:

```siox
sample(x)     // analogue -> digital sampled value
hold(x)       // digital -> analogue piecewise-constant value
cross(x, dir) // analogue threshold crossing -> digital event
quantize(...) // analogue value -> digital code
```

Example sampled comparator:

```siox
entity SampledComparator {
    in clk: Bit;

    p: Electrical<Time>;
    n: Electrical<Time>;

    out y: Bit;
}

impl SampledComparator {
    let input = p -> n;

    if clk.rising() {
        if sample(input.v) > 0.0 {
            y = '1';
        } else {
            y = '0';
        }
    }
}
```

### Phase 2 Deliverables

```text
domain parser/type checker
across/through semantics
analogue path elaboration: a -> b
domain quantity system attributes: ::ddt
equation IR
conservation equation generation
analysis-domain trait system
DC lowering
phasor lowering
basic transient solver interface
mixed-signal bridges: sample, hold, cross, quantize
analogue/digital co-simulation scheduling
```

---

## Phase 3 — Design Language

### Goal

Create a schematic/netlist-oriented layer for system composition and graphical design interfaces.

The design language should be easier to read and write than the full logic/modeling language. It should instantiate components, connect nodes/signals, set parameters, declare simulation setup, and store schematic layout metadata.

### Core Concepts

```text
design
    schematic/netlist/topology container

node
    analogue domain connection point

signal
    digital/discrete connection point

instance
    named component instantiation

param
    design-level constant / alias

sim
    simulation setup block

probe
    selected value to record/plot

#[pos = ...]
    layout metadata for schematic GUI
```

### Layout Metadata

Layout can live in the same file using declared attributes.

```siox
pub attr pos: Point2D for instance, node, signal;
pub attr rot: Angle for instance;
pub attr symbol: string for instance;
pub attr label: string for instance, node, signal;
pub attr color: string for instance, node, signal;
```

Layout attributes are non-semantic metadata.

They may affect:

```text
schematic display
GUI placement
symbol selection
labels
colors
```

They may not affect:

```text
simulation
elaboration
type checking
generated equations
```

### Example: RC Low-Pass Design

```siox
design RcLowPass {
    using std::analysis::Time;

    param A = Time;

    #[pos = {x = 0, y = 0}, label = "VIN"]
    node vin: Electrical<A>;

    #[pos = {x = 200, y = 0}, label = "VOUT"]
    node vout: Electrical<A>;

    #[pos = {x = 200, y = 120}, label = "GND"]
    node gnd: Electrical<A>;

    #[pos = {x = 60, y = 0}, symbol = "voltage_source"]
    V1: VoltageSource<V = 3.3, A = A> vin gnd;

    #[pos = {x = 140, y = 0}, symbol = "resistor_eu"]
    R1: Resistor<R = 10k, A = A> vin vout;

    #[pos = {x = 200, y = 60}, rot = 90.deg, symbol = "capacitor"]
    C1: Capacitor<C = 100n, A = A> vout gnd;

    sim {
        run 10.ms;
        probe vout;
    }
}
```

### Example: Sensor Frontend Design

```siox
design SensorFrontend {
    using std::analysis::Time;

    param A = Time;
    param VREF = 3.3;

    #[pos = {x = 0, y = 0}, label = "TEMP"]
    node temp: Thermal<A>;

    #[pos = {x = 0, y = 120}, label = "AMBIENT"]
    node ambient: Thermal<A>;

    #[pos = {x = 200, y = 0}, label = "VIN"]
    node vin: Electrical<A>;

    #[pos = {x = 360, y = 0}, label = "VFILT"]
    node vfilt: Electrical<A>;

    #[pos = {x = 360, y = 120}, label = "GND"]
    node gnd: Electrical<A>;

    signal clk: Bit;
    signal code: uint[12];

    #[pos = {x = 100, y = 40}, symbol = "thermal_to_voltage"]
    S1: ThermalToVoltage<K = 0.01, A = A> temp ambient vin gnd;

    #[pos = {x = 280, y = 0}, symbol = "resistor_eu"]
    R1: Resistor<R = 10k, A = A> vin vfilt;

    #[pos = {x = 360, y = 60}, rot = 90.deg, symbol = "capacitor"]
    C1: Capacitor<C = 100n, A = A> vfilt gnd;

    #[pos = {x = 520, y = 0}, symbol = "adc"]
    ADC1: IdealAdc<W = 12, VREF = VREF> {
        .clk = clk,
        .p = vfilt,
        .n = gnd,
        .code = code,
    }

    sim {
        tick clk every 10.us;
        run 10.ms;

        probe temp;
        probe vfilt;
        probe code;
    }
}
```

### Phase 3 Deliverables

```text
design/netlist parser
design AST
node/signal declarations
compact component instantiation syntax
named-field component instantiation syntax
layout attributes
schematic-compatible metadata model
design-to-siox elaboration
simulation setup handling
probe/check/assert support
GUI round-trip support
```

---

## Dependency Order

```text
Phase 1: Digital
    needed for clocks, logic, events, tests, scheduler, waveforms

Phase 2: Analogue
    builds on entities, impls, attributes, tests, and simulator infrastructure

Phase 3: Design
    builds on digital + analogue components and provides composition/layout
```

## Final Layering

```text
Digital language
    event-driven HDL modeling

Analogue language
    physical domains and continuous/mixed-signal modeling

Design language
    schematic/netlist composition and GUI-friendly layout
```

The design language should lower into normal siox elaboration. It is a friendlier frontend for composition, not a separate semantic universe.
