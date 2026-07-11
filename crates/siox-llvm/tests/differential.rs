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
        "{src}\ntrait Signed {{}}\nstruct uint : Logic[];\nstruct int : Logic[];\nimpl Signed for int {{}}\n"
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
         entity Counter { in clk: Clock; in rst: Logic; in en: Bit; out count: uint[8]; }\n\
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
           let clk: Logic; let rst: Logic; let en: Bit; let count: uint[8];\n\
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
         entity Reg { in clk: Clock; in d: uint[8]; out q: uint[8]; }\n\
         impl Reg {\n\
           let s: uint[8] = 0;\n\
           if clk::rising { s = d; }\n\
           q = s;\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let clk: Logic; let d: uint[8]; let q: uint[8];\n\
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
         entity Fsm { in clk: Clock; in go: Bit; in fin: Bit; out active: Bool; }\n\
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
           let clk: Logic; let go: Bit; let fin: Bit; let active: Bool;\n\
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
