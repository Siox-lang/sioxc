//! Differential harness (stage B4): the JIT must agree with the interpreter
//! oracle, signal for signal, across the whole expression surface —
//! arithmetic, slices, concat, enum match, struct/array signals, char
//! literals, and sequential (clocked) designs. Only built with
//! `--features llvm`.
#![cfg(all(feature = "llvm", feature = "interp"))]

use siox_diag::{DiagnosticSink, FileId};
use siox_ir::{Design, SignalId};
use siox_sim::Simulator;

fn lower(src: &str) -> Design {
    // uint/int are library types now (not seeded); the differential sources
    // are self-contained, so declare the vector families locally.
    let src = format!(
        "{src}\nstruct uint : Logic[];\nstruct int : Logic[];\n"
    );
    let src = src.as_str();
    let mut sink = DiagnosticSink::new();
    let module = siox_syntax::parse_module(FileId(0), src, &mut sink);
    assert_eq!(sink.error_count(), 0, "parse errors:\n{src}");
    let modules = std::slice::from_ref(&module);
    let resolved = siox_resolve::resolve(modules, &mut sink);
    let typed = siox_types::check(modules, &resolved, &mut sink);
    let hier = siox_elab::elaborate(modules, &typed, &mut sink);
    let design = siox_ir::lower(modules, &hier, &mut sink);
    assert_eq!(sink.error_count(), 0, "frontend errors:\n{src}");
    design
}

fn id(design: &Design, path: &str) -> SignalId {
    SignalId(design.signals.iter().position(|s| s.path == path).unwrap() as u32)
}

/// Drive `inputs` on both engines, settle, and assert every signal matches.
fn assert_agree(design: &Design, inputs: &[(&str, u64)]) {
    // Interpreter (oracle).
    let mut sim: Simulator = Simulator::new(design);
    for &(path, v) in inputs {
        sim.set(id(design, path), v);
    }
    sim.settle();
    let oracle: Vec<u64> =
        (0..design.signals.len()).map(|i| sim.read(SignalId(i as u32))).collect();

    // JIT.
    siox_llvm::with_jit(design, |jit| {
        for &(path, v) in inputs {
            jit.set(id(design, path).0, v);
        }
        jit.settle();
        for (i, want) in oracle.iter().enumerate() {
            let got = jit.read(i as u32);
            assert_eq!(
                got, *want,
                "signal {} ({}) disagrees: jit={got} oracle={want}",
                i, design.signals[i].path
            );
        }
    });
}

/// A stimulus step: drive these `(signal, value)` pairs, then settle.
type Step<'a> = &'a [(&'a str, u64)];

/// Run the same sequence of drive-then-settle steps on both engines, and after
/// *each* step assert every signal agrees. Exercises sequential state (event
/// blocks carry values across steps).
fn assert_agree_seq(design: &Design, steps: &[Step]) {
    let mut sim: Simulator = Simulator::new(design);
    siox_llvm::with_jit(design, |jit| {
        for (n, step) in steps.iter().enumerate() {
            for &(path, v) in *step {
                let s = id(design, path);
                sim.set(s, v);
                jit.set(s.0, v);
            }
            sim.settle();
            jit.settle();
            for i in 0..design.signals.len() {
                let want = sim.read(SignalId(i as u32));
                let got = jit.read(i as u32);
                assert_eq!(
                    got, want,
                    "step {n}: signal {i} ({}) disagrees: jit={got} oracle={want}",
                    design.signals[i].path
                );
            }
        }
    });
}

#[test]
fn counter_agrees_across_clock_edges() {
    let d = lower(
        "module m;\n\
         entity Counter { in clk: Bit; in rst: Logic; in en: Bit; out count: uint[8]; }\n\
         impl Counter {\n\
           let value: uint[8] = 0;\n\
           if clk::rising {\n\
             if rst == '1' { value = 0; } else if en { value = value + 1; }\n\
           }\n\
           count = value;\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let clk: Bit; let rst: Logic; let en: Bit; let count: uint[8];\n\
           let dut = Counter { .clk, .rst, .en, .count };\n\
         }\n",
    );
    // Reset high for one edge, then count several enabled cycles, then hold.
    let mut steps: Vec<Vec<(&str, u64)>> = Vec::new();
    steps.push(vec![("T.rst", 1), ("T.en", 1), ("T.clk", 0)]);
    steps.push(vec![("T.clk", 1)]); // rising edge under reset -> 0
    steps.push(vec![("T.rst", 0), ("T.clk", 0)]);
    for _ in 0..5 {
        steps.push(vec![("T.clk", 1)]); // rising: count++
        steps.push(vec![("T.clk", 0)]);
    }
    // Disable and pulse: value should hold across edges.
    steps.push(vec![("T.en", 0), ("T.clk", 1)]);
    steps.push(vec![("T.clk", 0)]);
    let refs: Vec<Step> = steps.iter().map(|s| s.as_slice()).collect();
    assert_agree_seq(&d, &refs);
}

#[test]
fn register_agrees_across_clock_edges() {
    // A plain D flip-flop: unconditional next-state on the rising edge.
    let d = lower(
        "module m;\n\
         entity Reg { in clk: Bit; in d: uint[8]; out q: uint[8]; }\n\
         impl Reg {\n\
           let s: uint[8] = 0;\n\
           if clk::rising { s = d; }\n\
           q = s;\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let clk: Bit; let d: uint[8]; let q: uint[8];\n\
           let dut = Reg { .clk, .d, .q };\n\
         }\n",
    );
    let steps: Vec<Vec<(&str, u64)>> = vec![
        vec![("T.d", 42), ("T.clk", 0)],
        vec![("T.clk", 1)], // latch 42
        vec![("T.clk", 0), ("T.d", 99)],
        vec![("T.clk", 1)], // latch 99
        vec![("T.d", 7)],   // no edge: q holds 99
    ];
    let refs: Vec<Step> = steps.iter().map(|s| s.as_slice()).collect();
    assert_agree_seq(&d, &refs);
}

#[test]
fn fsm_agrees_across_clock_edges() {
    // Enum-state machine: exercises an enum-typed sequential signal, `match`
    // in an event block, and enum comparison — all at once.
    let d = lower(
        "module m;\n\
         enum State { Idle, Run, Done }\n\
         entity Fsm { in clk: Bit; in go: Bit; in fin: Bit; out active: Bool; }\n\
         impl Fsm {\n\
           let state: State = State::Idle;\n\
           if clk::rising {\n\
             match state {\n\
               State::Idle => { if go { state = State::Run; } }\n\
               State::Run => { if fin { state = State::Done; } }\n\
               _ => { state = State::Idle; }\n\
             }\n\
           }\n\
           active = state == State::Run;\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let clk: Bit; let go: Bit; let fin: Bit; let active: Bool;\n\
           let dut = Fsm { .clk, .go, .fin, .active };\n\
         }\n",
    );
    let steps: Vec<Vec<(&str, u64)>> = vec![
        vec![("T.go", 0), ("T.fin", 0), ("T.clk", 0)],
        vec![("T.go", 1), ("T.clk", 1)], // Idle -> Run
        vec![("T.clk", 0)],
        vec![("T.go", 0), ("T.clk", 1)], // Run (fin=0, stays)
        vec![("T.clk", 0), ("T.fin", 1)],
        vec![("T.clk", 1)], // Run -> Done
        vec![("T.clk", 0)],
        vec![("T.clk", 1)], // Done -> Idle
        vec![("T.clk", 0)],
    ];
    let refs: Vec<Step> = steps.iter().map(|s| s.as_slice()).collect();
    assert_agree_seq(&d, &refs);
}

#[test]
fn mux_agrees() {
    let d = lower(
        "module m;\n\
         entity Mux { in sel: Bit; in a: uint[8]; in b: uint[8]; out y: uint[8]; }\n\
         impl Mux {\n\
           y = b;\n\
           if sel { y = a; }\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let sel: Bit; let a: uint[8]; let b: uint[8]; let y: uint[8];\n\
           let dut = Mux { .sel, .a, .b, .y };\n\
         }\n",
    );
    assert_agree(&d, &[("T.sel", 0), ("T.a", 111), ("T.b", 222)]);
    assert_agree(&d, &[("T.sel", 1), ("T.a", 111), ("T.b", 222)]);
}

#[test]
fn arithmetic_and_slice_agree() {
    let d = lower(
        "module m;\n\
         entity Alu { in a: uint[8]; in b: uint[8]; out sum: uint[8]; out hi: uint[4]; }\n\
         impl Alu {\n\
           sum = a + b;\n\
           hi = a[7..4];\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let a: uint[8]; let b: uint[8]; let sum: uint[8]; let hi: uint[4];\n\
           let dut = Alu { .a, .b, .sum, .hi };\n\
         }\n",
    );
    for (a, b) in [(10u64, 20u64), (200, 100), (0xA5, 0x0F), (255, 1)] {
        assert_agree(&d, &[("T.a", a), ("T.b", b)]);
    }
}

#[test]
fn concat_agrees() {
    // `{a, b}` -> shift/add tree in the IR.
    let d = lower(
        "module m;\n\
         entity C { in a: uint[4]; in b: uint[4]; out y: uint[8]; }\n\
         impl C { y = {a, b}; }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let a: uint[4]; let b: uint[4]; let y: uint[8];\n\
           let dut = C { .a, .b, .y };\n\
         }\n",
    );
    for (a, b) in [(0xA, 0x5), (0xF, 0x0), (0x0, 0xF), (0x3, 0xC)] {
        assert_agree(&d, &[("T.a", a), ("T.b", b)]);
    }
}

#[test]
fn enum_match_agrees() {
    // `match op` -> first-match comparison chain over discriminants.
    let d = lower(
        "module m;\n\
         enum Op { Add, Sub, Pass }\n\
         entity Alu { in op: Op; in a: uint[8]; in b: uint[8]; out y: uint[8]; }\n\
         impl Alu {\n\
           match op {\n\
             Op::Add => { y = a + b; }\n\
             Op::Sub => { y = a - b; }\n\
             _ => { y = a; }\n\
           }\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let op: Op; let a: uint[8]; let b: uint[8]; let y: uint[8];\n\
           let dut = Alu { .op, .a, .b, .y };\n\
         }\n",
    );
    for op in 0..3u64 {
        assert_agree(&d, &[("T.op", op), ("T.a", 30), ("T.b", 12)]);
    }
}

#[test]
fn struct_field_agrees() {
    // Struct fields flatten to per-field signals (`S.p.lo`, `S.p.hi`).
    let d = lower(
        "module m;\n\
         struct P { lo: uint[4], hi: uint[4] }\n\
         entity S { in p: P; out y: uint[8]; }\n\
         impl S { y = {p.hi, p.lo}; }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let p: P; let y: uint[8];\n\
           let dut = S { .p, .y };\n\
         }\n",
    );
    assert_agree(&d, &[("T.p.lo", 0x5), ("T.p.hi", 0xA)]);
    assert_agree(&d, &[("T.p.lo", 0xF), ("T.p.hi", 0x0)]);
}

#[test]
fn array_element_agrees() {
    // Array elements flatten to `A.v[0]`, `A.v[1]`.
    let d = lower(
        "module m;\n\
         entity A { in v: uint[4][2]; out y: uint[8]; }\n\
         impl A { y = {v[1], v[0]}; }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let v: uint[4][2]; let y: uint[8];\n\
           let dut = A { .v, .y };\n\
         }\n",
    );
    assert_agree(&d, &[("T.v[0]", 0x3), ("T.v[1]", 0xC)]);
}

#[test]
fn char_compare_agrees() {
    // A Char literal reads through the context type as a Unicode code point;
    // both engines evaluate the same lowered constant.
    let d = lower(
        "module m;\n\
         entity Ch { in c: Char; out is_a: Bool; }\n\
         impl Ch { is_a = c == 'A'; }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let c: Char; let is_a: Bool;\n\
           let dut = Ch { .c, .is_a };\n\
         }\n",
    );
    assert_agree(&d, &[("T.c", 'A' as u64)]);
    assert_agree(&d, &[("T.c", 'B' as u64)]);
    assert_agree(&d, &[("T.c", 0x20AC)]); // euro sign
}

#[test]
fn combinational_chain_agrees() {
    let d = lower(
        "module m;\n\
         entity Chain { in i: uint[8]; out o: uint[8]; }\n\
         impl Chain {\n\
           let x: uint[8];\n\
           let y: uint[8];\n\
           o = y;\n\
           y = x + 1;\n\
           x = i + 1;\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let i: uint[8]; let o: uint[8];\n\
           let dut = Chain { .i, .o };\n\
         }\n",
    );
    for v in [0u64, 10, 100, 254] {
        assert_agree(&d, &[("T.i", v)]);
    }
}

#[test]
fn generate_loop_chain_agrees() {
    // A generate loop unrolls three incrementer instances wired head-to-tail
    // through a flattened wire array (`wires[i] -> wires[i+1]`). Both engines
    // must see the same lowered instance graph and agree signal-for-signal.
    let d = lower(
        "module m;\n\
         entity Inc { in x: uint[8]; out y: uint[8]; }\n\
         impl Inc { y = x + 1; }\n\
         entity Chain { in a: uint[8]; out b: uint[8]; }\n\
         impl Chain {\n\
           let wires: uint[8][4];\n\
           wires[0] = a;\n\
           for i in 0..2 {\n\
             let inc = Inc { .x = wires[i], .y = wires[i+1] };\n\
           }\n\
           b = wires[3];\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let a: uint[8]; let b: uint[8];\n\
           let dut = Chain { .a, .b };\n\
         }\n",
    );
    for v in [0u64, 10, 42, 252] {
        assert_agree(&d, &[("T.a", v)]);
    }
}

#[test]
fn generate_loop_descending_agrees() {
    // The same three-incrementer chain built by a *descending* generate loop
    // (`2..0` -> 2,1,0). Range endpoints are inclusive and directional, so this
    // instantiates the identical stage set in reverse iteration order; the
    // lowered design — and thus both engines — must match the ascending build.
    let d = lower(
        "module m;\n\
         entity Inc { in x: uint[8]; out y: uint[8]; }\n\
         impl Inc { y = x + 1; }\n\
         entity Chain { in a: uint[8]; out b: uint[8]; }\n\
         impl Chain {\n\
           let wires: uint[8][4];\n\
           wires[0] = a;\n\
           for i in 2..0 {\n\
             let inc = Inc { .x = wires[i], .y = wires[i+1] };\n\
           }\n\
           b = wires[3];\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let a: uint[8]; let b: uint[8];\n\
           let dut = Chain { .a, .b };\n\
         }\n",
    );
    for v in [0u64, 10, 42, 252] {
        assert_agree(&d, &[("T.a", v)]);
    }
}

#[test]
fn inout_tristate_bus_agrees() {
    // Two bidirectional pads share one net. `inout` ports alias the net, so the
    // two drivers fold through `impl Resolve for Logic`: a driven level beats
    // 'Z', disagreement is 'X'. The JIT must match the interpreter oracle across
    // drive/tristate/contention combinations.
    let d = lower(
        "module m;\n\
         enum Logic { '0', '1', 'Z', 'X' }\n\
         trait Resolve { fn resolve(self, rhs: Logic) -> Logic; }\n\
         impl Resolve for Logic {\n\
           fn resolve(self, rhs: Logic) -> Logic {\n\
             if self == 'Z' { return rhs; }\n\
             if rhs == 'Z' { return self; }\n\
             if self == rhs { return self; }\n\
             return 'X';\n\
           }\n\
         }\n\
         entity Pad { in drive: Logic; in en: Logic; inout pin: Logic; out sensed: Logic; }\n\
         impl Pad { pin = if en == '1' { drive } else { 'Z' }; sensed = pin; }\n\
         entity Bus { in da: Logic; in ea: Logic; in db: Logic; in eb: Logic; out sa: Logic; out sb: Logic; }\n\
         impl Bus {\n\
           let wire: Logic;\n\
           let a = Pad { .drive = da, .en = ea, .pin = wire, .sensed = sa };\n\
           let b = Pad { .drive = db, .en = eb, .pin = wire, .sensed = sb };\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let da: Logic; let ea: Logic; let db: Logic; let eb: Logic; let sa: Logic; let sb: Logic;\n\
           let dut = Bus { .da, .ea, .db, .eb, .sa, .sb };\n\
         }\n",
    );
    // Logic codes: '0'=0 '1'=1 'Z'=2 'X'=3.
    for (ea, da, eb, db) in [
        (1u64, 1u64, 2u64, 0u64), // A drives 1, B tristate
        (2, 0, 1, 0),             // B drives 0, A tristate
        (1, 1, 1, 0),             // both drive, disagree -> X
        (1, 1, 1, 1),             // both drive 1 -> 1
        (2, 0, 2, 0),             // neither drives -> Z
    ] {
        assert_agree(&d, &[("T.ea", ea), ("T.da", da), ("T.eb", eb), ("T.db", db)]);
    }
}

#[test]
fn struct_port_across_instances_agrees() {
    // A struct-typed port bundles valid+data across an instance boundary; each
    // field wires independently (producer -> net -> consumer). The JIT must
    // match the interpreter oracle on the resulting flattened signals.
    let d = lower(
        "module m;\n\
         enum Logic { '0', '1' }\n\
         struct Stream { valid: Logic, data: uint[8] }\n\
         entity Producer { in vin: Logic; in din: uint[8]; out s: Stream; }\n\
         impl Producer { s.valid = vin; s.data = din; }\n\
         entity Consumer { in s: Stream; out got: uint[8]; }\n\
         impl Consumer { got = if s.valid == '1' { s.data } else { 0 }; }\n\
         entity Link { in vin: Logic; in din: uint[8]; out got: uint[8]; }\n\
         impl Link {\n\
           let wire: Stream;\n\
           let p = Producer { .vin, .din, .s = wire };\n\
           let c = Consumer { .s = wire, .got };\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let vin: Logic; let din: uint[8]; let got: uint[8];\n\
           let dut = Link { .vin, .din, .got };\n\
         }\n",
    );
    for (vin, din) in [(1u64, 42u64), (0, 42), (1, 200), (0, 7)] {
        assert_agree(&d, &[("T.vin", vin), ("T.din", din)]);
    }
}

#[test]
fn bit_pattern_match_agrees() {
    // `match` over bit patterns with `?` don't-cares (spec 3.22): each arm
    // lowers to `(scrut & mask) == value` with first-match priority. Both
    // engines must classify every opcode identically.
    let d = lower(
        "module m;\n\
         entity Dec { in op: uint[4]; out kind: uint[2]; }\n\
         impl Dec {\n\
           match op {\n\
             b\"00??\" => { kind = 0; }\n\
             b\"01??\" => { kind = 1; }\n\
             b\"1?1?\" => { kind = 2; }\n\
             _ => { kind = 3; }\n\
           }\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let op: uint[4]; let kind: uint[2];\n\
           let dut = Dec { .op, .kind };\n\
         }\n",
    );
    for op in 0u64..16 {
        assert_agree(&d, &[("T.op", op)]);
    }
}

#[test]
fn concat_assignment_target_agrees() {
    // `{hi, lo} = w` unpacks MSB-first: each part takes its width's slice of
    // the RHS, in combinational and clocked forms alike.
    let d = lower(
        "module m;\n\
         entity Split { in w: uint[8]; out hi: uint[4]; out lo: uint[4]; }\n\
         impl Split { {hi, lo} = w; }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let w: uint[8]; let hi: uint[4]; let lo: uint[4];\n\
           let dut = Split { .w, .hi, .lo };\n\
         }\n",
    );
    for w in [0u64, 0xA3, 0xFF, 0x0F, 0xF0] {
        assert_agree(&d, &[("T.w", w)]);
    }
}

#[test]
fn instance_array_agrees() {
    // An array of instances (`let stage: Inc[3]`) built element-wise in a
    // generate loop, wired head-to-tail, with an element's output read from
    // outside the loop (`stage[1].y`). Both engines must match.
    let d = lower(
        "module m;\n\
         entity Inc { in x: uint[8]; out y: uint[8]; }\n\
         impl Inc { y = x + 1; }\n\
         entity Chain { in a: uint[8]; out b: uint[8]; out mid: uint[8]; }\n\
         impl Chain {\n\
           let w: uint[8][4];\n\
           w[0] = a;\n\
           let stage: Inc[3];\n\
           for i in 0..2 { stage[i] = Inc { .x = w[i], .y = w[i+1] }; }\n\
           b = w[3];\n\
           mid = stage[1].y;\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let a: uint[8]; let b: uint[8]; let mid: uint[8];\n\
           let dut = Chain { .a, .b, .mid };\n\
         }\n",
    );
    for a in [0u64, 10, 40, 250] {
        assert_agree(&d, &[("T.a", a)]);
    }
}

#[test]
fn generic_entity_agrees() {
    // A generic entity `Buf<T>` specializes its `T`-typed ports and internal
    // state to the type argument (`Buf<uint[8]>`), so signals get the concrete
    // width. Both engines must agree.
    let d = lower(
        "module m;\n\
         entity Buf<T> { in a: T; in b: T; out y: T; }\n\
         impl Buf<T> {\n\
           let s: T;\n\
           s = a + b;\n\
           y = s;\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let a: uint[8]; let b: uint[8]; let y: uint[8];\n\
           let dut = Buf<uint[8]> { .a, .b, .y };\n\
         }\n",
    );
    assert_eq!(d.signals[id(&d, "T.dut.s").0 as usize].width, 8, "Buf<uint[8]>.s is 8-bit");
    for (a, b) in [(10u64, 20u64), (200, 100), (255, 1)] {
        assert_agree(&d, &[("T.a", a), ("T.b", b)]);
    }
}

#[test]
fn generic_struct_agrees() {
    // A generic struct (`Pair<uint[8]>`) substitutes its type parameter into
    // the field types, so `p.a`/`p.b` are 8-bit signals and arithmetic wraps.
    let d = lower(
        "module m;\n\
         struct Pair<T> { a: T, b: T, }\n\
         entity E { in x: uint[8]; out sum: uint[8]; }\n\
         impl E {\n\
           let p: Pair<uint[8]>;\n\
           p.a = x; p.b = x;\n\
           sum = p.a + p.b;\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T { let x: uint[8]; let sum: uint[8]; let dut = E { .x, .sum }; }\n",
    );
    for x in [10u64, 100, 200, 255] {
        assert_agree(&d, &[("T.x", x)]);
    }
    assert_eq!(d.signals[id(&d, "T.dut.p.a").0 as usize].width, 8, "Pair<uint[8]>.a is 8-bit");
}

#[test]
fn bus_mode_agrees() {
    // A directional bus view (spec 3.19): `impl out Stream::Source` /
    // `impl in Stream::Sink` give each leaf a per-field direction, so
    // valid/data flow Source->Sink and ready flows Sink->Source across the
    // shared net. Both engines must agree.
    let d = lower(
        "module m;\n\
         struct Stream { valid: Bit, ready: Bit, data: uint[8], }\n\
         impl out Stream::Source { out valid; out data; in ready; }\n\
         impl in Stream::Sink { in valid; in data; out ready; }\n\
         entity Producer { bus: out Stream::Source; in d: uint[8]; out canpush: Bit; }\n\
         impl Producer { bus.valid = '1'; bus.data = d; canpush = bus.ready; }\n\
         entity Consumer { bus: in Stream::Sink; in accept: Bit; out got: uint[8]; }\n\
         impl Consumer { bus.ready = accept; got = bus.data; }\n\
         #[top]\n\
         entity T { in d: uint[8]; in accept: Bit; out got: uint[8]; out canpush: Bit; }\n\
         impl T {\n\
           let link: Stream;\n\
           let p = Producer { .bus = link, .d, .canpush };\n\
           let c = Consumer { .bus = link, .accept, .got };\n\
         }\n",
    );
    for (dv, acc) in [(77u64, 1u64), (200, 0), (0, 1), (255, 0)] {
        assert_agree(&d, &[("T.d", dv), ("T.accept", acc)]);
    }
}

#[test]
fn derived_vector_width_agrees() {
    // `struct Byte : Logic[8]` inherits width 8 from its base array, so a signal
    // of type `Byte` masks arithmetic at 2^8 — both engines must agree (a bug
    // would leave it width 0 = unmasked).
    let d = lower(
        "module m;\n\
         struct Byte : Logic[8];\n\
         entity A { in a: Byte; in b: Byte; out s: Byte; }\n\
         impl A { s = a + b; }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let a: Byte; let b: Byte; let s: Byte;\n\
           let dut = A { .a, .b, .s };\n\
         }\n",
    );
    // s is 8-bit: 200+100 = 300 -> 44, 255+1 -> 0.
    for (a, b) in [(200u64, 100u64), (255, 1), (10, 20), (128, 128)] {
        assert_agree(&d, &[("T.a", a), ("T.b", b)]);
    }
    assert_eq!(d.signals[id(&d, "T.dut.s").0 as usize].width, 8, "Byte signal must be width 8");
}

#[test]
fn transitive_vector_family_width_agrees() {
    // A type deriving from *another* vector family (`struct Byte : uint[8]`)
    // is itself a vector, inheriting width 8 (transitive recognition). `uint`
    // is appended by the `lower` harness.
    let d = lower(
        "module m;\n\
         struct Byte : uint[8];\n\
         entity A { in a: Byte; in b: Byte; out s: Byte; }\n\
         impl A { s = a + b; }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let a: Byte; let b: Byte; let s: Byte;\n\
           let dut = A { .a, .b, .s };\n\
         }\n",
    );
    assert_eq!(d.signals[id(&d, "T.dut.s").0 as usize].width, 8, "Byte : uint[8] must be width 8");
    for (a, b) in [(200u64, 100u64), (255, 1)] {
        assert_agree(&d, &[("T.a", a), ("T.b", b)]);
    }
}

#[test]
fn method_call_agrees() {
    // Method calls (`recv.method(args)`, spec 3.20) inline during IR lowering,
    // so both engines see the same primitive tree. Covers a nullary
    // value-returning method (`p.sum()`), one taking an argument and branching
    // (`p.bigger(lim)`), and operator dispatch inside the body (`self.x + ..`).
    let d = lower(
        "module m;\n\
         struct Pt { x: uint[8], y: uint[8], }\n\
         impl Pt {\n\
           fn sum(self) -> uint[8] { return self.x + self.y; }\n\
           fn bigger(self, lim: uint[8]) -> uint[8] {\n\
             if self.x > lim { return self.x; }\n\
             return lim;\n\
           }\n\
         }\n\
         entity D { in px: uint[8]; in py: uint[8]; in lim: uint[8]; out s: uint[8]; out bg: uint[8]; }\n\
         impl D {\n\
           let p: Pt;\n\
           p.x = px;\n\
           p.y = py;\n\
           s = p.sum();\n\
           bg = p.bigger(lim);\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let px: uint[8]; let py: uint[8]; let lim: uint[8]; let s: uint[8]; let bg: uint[8];\n\
           let dut = D { .px, .py, .lim, .s, .bg };\n\
         }\n",
    );
    for (px, py, lim) in [(10u64, 20u64, 15u64), (200, 100, 50), (5, 5, 250), (0, 0, 0)] {
        assert_agree(&d, &[("T.px", px), ("T.py", py), ("T.lim", lim)]);
    }
}

#[test]
fn statement_method_agrees() {
    // A method used as a statement (`s.send(v)`) inlines its body as drivers on
    // the receiver's flattened fields, substituting `self` -> receiver and the
    // parameter -> argument. The two branches cover both cases (no latch).
    let d = lower(
        "module m;\n\
         struct Stream { valid: Logic, data: uint[8], }\n\
         impl Stream {\n\
           fn send(self, v: uint[8]) { self.valid = '1'; self.data = v; }\n\
           fn clear(self) { self.valid = '0'; self.data = 0; }\n\
         }\n\
         entity D { in go: Bit; in x: uint[8]; out ov: Logic; out od: uint[8]; }\n\
         impl D {\n\
           let s: Stream;\n\
           if go { s.send(x); } else { s.clear(); }\n\
           ov = s.valid;\n\
           od = s.data;\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let go: Bit; let x: uint[8]; let ov: Logic; let od: uint[8];\n\
           let dut = D { .go, .x, .ov, .od };\n\
         }\n",
    );
    for (go, x) in [(1u64, 42u64), (0, 42), (1, 255), (0, 0), (1, 0)] {
        assert_agree(&d, &[("T.go", go), ("T.x", x)]);
    }
}

#[test]
fn struct_inout_bus_agrees() {
    // A struct-typed `inout` port (`bus: Bus`) shared between two pads: each
    // leaf (`bus.hi`, `bus.lo`) aliases the corresponding leaf of the shared
    // net, so the two pads' drivers fold per-leaf through `Resolve`. The JIT
    // must match the interpreter oracle across drive/tristate/contention.
    let d = lower(
        "module m;\n\
         enum Logic { '0', '1', 'Z', 'X' }\n\
         trait Resolve { fn resolve(self, rhs: Logic) -> Logic; }\n\
         impl Resolve for Logic {\n\
           fn resolve(self, rhs: Logic) -> Logic {\n\
             if self == 'Z' { return rhs; }\n\
             if rhs == 'Z' { return self; }\n\
             if self == rhs { return self; }\n\
             return 'X';\n\
           }\n\
         }\n\
         struct Bus { hi: Logic, lo: Logic, }\n\
         entity Pad { in drive: Logic; in en: Logic; inout bus: Bus; out shi: Logic; out slo: Logic; }\n\
         impl Pad {\n\
           bus.hi = if en == '1' { drive } else { 'Z' };\n\
           bus.lo = if en == '1' { drive } else { 'Z' };\n\
           shi = bus.hi;\n\
           slo = bus.lo;\n\
         }\n\
         entity Wired { in da: Logic; in ea: Logic; in db: Logic; in eb: Logic;\n\
                        out ha: Logic; out la: Logic; out hb: Logic; out lb: Logic; }\n\
         impl Wired {\n\
           let wire: Bus;\n\
           let a = Pad { .drive = da, .en = ea, .bus = wire, .shi = ha, .slo = la };\n\
           let b = Pad { .drive = db, .en = eb, .bus = wire, .shi = hb, .slo = lb };\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let da: Logic; let ea: Logic; let db: Logic; let eb: Logic;\n\
           let ha: Logic; let la: Logic; let hb: Logic; let lb: Logic;\n\
           let dut = Wired { .da, .ea, .db, .eb, .ha, .la, .hb, .lb };\n\
         }\n",
    );
    // Logic codes: '0'=0 '1'=1 'Z'=2 'X'=3.
    for (ea, da, eb, db) in [
        (1u64, 1u64, 2u64, 0u64), // A drives 1, B tristate
        (2, 0, 1, 0),             // B drives 0, A tristate
        (1, 1, 1, 0),             // both drive, disagree -> X
        (1, 1, 1, 1),             // both drive 1 -> 1
        (2, 0, 2, 0),             // neither drives -> Z
    ] {
        assert_agree(&d, &[("T.ea", ea), ("T.da", da), ("T.eb", eb), ("T.db", db)]);
    }
}
