//! Differential harness (stage B4 seed): the JIT must agree with the
//! interpreter oracle, signal for signal, on combinational designs.
//!
//! Only built with `--features llvm`; sequential (event-block) designs join
//! once B2.1 emits their codegen.
#![cfg(feature = "llvm")]

use siox_diag::{DiagnosticSink, FileId};
use siox_ir::{Design, SignalId};
use siox_sim::Simulator;

fn lower(src: &str) -> Design {
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
    steps.push(vec![("Counter.rst", 1), ("Counter.en", 1), ("Counter.clk", 0)]);
    steps.push(vec![("Counter.clk", 1)]); // rising edge under reset -> 0
    steps.push(vec![("Counter.rst", 0), ("Counter.clk", 0)]);
    for _ in 0..5 {
        steps.push(vec![("Counter.clk", 1)]); // rising: count++
        steps.push(vec![("Counter.clk", 0)]);
    }
    // Disable and pulse: value should hold across edges.
    steps.push(vec![("Counter.en", 0), ("Counter.clk", 1)]);
    steps.push(vec![("Counter.clk", 0)]);
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
        vec![("Reg.d", 42), ("Reg.clk", 0)],
        vec![("Reg.clk", 1)], // latch 42
        vec![("Reg.clk", 0), ("Reg.d", 99)],
        vec![("Reg.clk", 1)], // latch 99
        vec![("Reg.d", 7)],   // no edge: q holds 99
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
    assert_agree(&d, &[("Mux.sel", 0), ("Mux.a", 111), ("Mux.b", 222)]);
    assert_agree(&d, &[("Mux.sel", 1), ("Mux.a", 111), ("Mux.b", 222)]);
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
        assert_agree(&d, &[("Alu.a", a), ("Alu.b", b)]);
    }
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
        assert_agree(&d, &[("Chain.i", v)]);
    }
}
