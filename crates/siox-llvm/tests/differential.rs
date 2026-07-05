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
