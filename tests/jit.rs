//! JIT behaviour tests: drive the compiled (LLVM JIT) engine across the whole
//! expression surface — arithmetic, slices, concat, enum match, struct/array
//! signals, char literals, and sequential (clocked) designs — and assert the
//! settled signal values. The golden values were captured from the delta-cycle
//! interpreter that used to be the differential oracle (before it was removed);
//! the JIT is now the only engine.

use siox::diag::{DiagnosticSink, FileId};
use siox::ir::{Design, SignalId};

fn lower(src: &str) -> Design {
    // uint/int are library types now (not seeded); the sources are
    // self-contained, so declare the vector families locally.
    let src = format!(
        "{src}\nstruct uint : Logic[];\nstruct int : Logic[];\n\
         enum Bit {{ '0', '1' }}\n\
         trait ClockLike {{ fn rising(self) -> Bool; fn falling(self) -> Bool; fn edge(self) -> Bool; }}\n\
         impl ClockLike for Bit {{\n\
           fn rising(self) -> Bool {{ return self::event and self::old == '0' and self == '1'; }}\n\
           fn falling(self) -> Bool {{ return self::event and self::old == '1' and self == '0'; }}\n\
           fn edge(self) -> Bool {{ return self::event; }}\n\
         }}\n"
    );
    let src = src.as_str();
    let mut sink = DiagnosticSink::new();
    let module = siox::syntax::parse_module(FileId(0), src, &mut sink);
    assert_eq!(sink.error_count(), 0, "parse errors:\n{src}");
    let modules = std::slice::from_ref(&module);
    let resolved = siox::resolve::resolve(modules, &mut sink);
    let typed = siox::types::check(modules, &resolved, &mut sink);
    let hier = siox::elab::elaborate(modules, &typed, &mut sink);
    let design = siox::ir::lower(modules, &hier, &mut sink);
    assert_eq!(sink.error_count(), 0, "frontend errors:\n{src}");
    design
}

fn id(design: &Design, path: &str) -> SignalId {
    SignalId(design.signals.iter().position(|s| s.path == path).unwrap() as u32)
}

/// Drive `inputs` on the JIT, settle, and assert each `(signal, value)` in
/// `expect` — the golden post-settle values.
#[track_caller]
fn check(design: &Design, inputs: &[(&str, u64)], expect: &[(&str, u64)]) {
    siox::llvm::with_jit(design, |jit| {
        for &(path, v) in inputs {
            jit.set(id(design, path).0, v);
        }
        jit.settle();
        for &(path, want) in expect {
            let got = jit.read(id(design, path).0);
            assert_eq!(got, want, "signal {path}: jit={got} want={want}");
        }
    });
}

/// A stimulus step: drive these `(signal, value)` pairs, then settle.
type Step<'a> = &'a [(&'a str, u64)];

/// Run clocked `steps` (each drives its pairs, then settles) on the JIT, and
/// after step `n` assert every `(signal, value)` in `golden[n]`. Exercises
/// sequential state — event blocks carry values across steps.
#[track_caller]
fn check_seq(design: &Design, steps: &[Step], golden: &[&[(&str, u64)]]) {
    assert_eq!(steps.len(), golden.len(), "one golden snapshot per step");
    siox::llvm::with_jit(design, |jit| {
        for (n, step) in steps.iter().enumerate() {
            for &(path, v) in *step {
                jit.set(id(design, path).0, v);
            }
            jit.settle();
            for &(path, want) in golden[n] {
                let got = jit.read(id(design, path).0);
                assert_eq!(got, want, "step {n}: signal {path}: jit={got} want={want}");
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
           if clk.rising() {\n\
             if rst == '1' { value = 0; } else if en { value = value + 1; }\n\
           }\n\
           count = value;\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let clk: Bit; let rst: Logic; let en: Bit; let count: uint[8];\n\
           let dut: Counter = { .clk = clk, .rst = rst, .en = en, .count = count };\n\
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
    let golden: &[&[(&str, u64)]] = &[
        &[("T.clk", 0), ("T.rst", 1), ("T.en", 1), ("T.count", 0), ("T.dut.clk", 0), ("T.dut.rst", 1), ("T.dut.en", 1), ("T.dut.count", 0), ("T.dut.value", 0)],
        &[("T.clk", 1), ("T.rst", 1), ("T.en", 1), ("T.count", 0), ("T.dut.clk", 1), ("T.dut.rst", 1), ("T.dut.en", 1), ("T.dut.count", 0), ("T.dut.value", 0)],
        &[("T.clk", 0), ("T.rst", 0), ("T.en", 1), ("T.count", 0), ("T.dut.clk", 0), ("T.dut.rst", 0), ("T.dut.en", 1), ("T.dut.count", 0), ("T.dut.value", 0)],
        &[("T.clk", 1), ("T.rst", 0), ("T.en", 1), ("T.count", 1), ("T.dut.clk", 1), ("T.dut.rst", 0), ("T.dut.en", 1), ("T.dut.count", 1), ("T.dut.value", 1)],
        &[("T.clk", 0), ("T.rst", 0), ("T.en", 1), ("T.count", 1), ("T.dut.clk", 0), ("T.dut.rst", 0), ("T.dut.en", 1), ("T.dut.count", 1), ("T.dut.value", 1)],
        &[("T.clk", 1), ("T.rst", 0), ("T.en", 1), ("T.count", 2), ("T.dut.clk", 1), ("T.dut.rst", 0), ("T.dut.en", 1), ("T.dut.count", 2), ("T.dut.value", 2)],
        &[("T.clk", 0), ("T.rst", 0), ("T.en", 1), ("T.count", 2), ("T.dut.clk", 0), ("T.dut.rst", 0), ("T.dut.en", 1), ("T.dut.count", 2), ("T.dut.value", 2)],
        &[("T.clk", 1), ("T.rst", 0), ("T.en", 1), ("T.count", 3), ("T.dut.clk", 1), ("T.dut.rst", 0), ("T.dut.en", 1), ("T.dut.count", 3), ("T.dut.value", 3)],
        &[("T.clk", 0), ("T.rst", 0), ("T.en", 1), ("T.count", 3), ("T.dut.clk", 0), ("T.dut.rst", 0), ("T.dut.en", 1), ("T.dut.count", 3), ("T.dut.value", 3)],
        &[("T.clk", 1), ("T.rst", 0), ("T.en", 1), ("T.count", 4), ("T.dut.clk", 1), ("T.dut.rst", 0), ("T.dut.en", 1), ("T.dut.count", 4), ("T.dut.value", 4)],
        &[("T.clk", 0), ("T.rst", 0), ("T.en", 1), ("T.count", 4), ("T.dut.clk", 0), ("T.dut.rst", 0), ("T.dut.en", 1), ("T.dut.count", 4), ("T.dut.value", 4)],
        &[("T.clk", 1), ("T.rst", 0), ("T.en", 1), ("T.count", 5), ("T.dut.clk", 1), ("T.dut.rst", 0), ("T.dut.en", 1), ("T.dut.count", 5), ("T.dut.value", 5)],
        &[("T.clk", 0), ("T.rst", 0), ("T.en", 1), ("T.count", 5), ("T.dut.clk", 0), ("T.dut.rst", 0), ("T.dut.en", 1), ("T.dut.count", 5), ("T.dut.value", 5)],
        &[("T.clk", 1), ("T.rst", 0), ("T.en", 0), ("T.count", 5), ("T.dut.clk", 1), ("T.dut.rst", 0), ("T.dut.en", 0), ("T.dut.count", 5), ("T.dut.value", 5)],
        &[("T.clk", 0), ("T.rst", 0), ("T.en", 0), ("T.count", 5), ("T.dut.clk", 0), ("T.dut.rst", 0), ("T.dut.en", 0), ("T.dut.count", 5), ("T.dut.value", 5)],
    ];
    check_seq(&d, &refs, golden);
}

#[test]
fn register_agrees_across_clock_edges() {
    // A plain D flip-flop: unconditional next-state on the rising edge.
    let d = lower(
        "module m;\n\
         entity Reg { in clk: Bit; in d: uint[8]; out q: uint[8]; }\n\
         impl Reg {\n\
           let s: uint[8] = 0;\n\
           if clk.rising() { s = d; }\n\
           q = s;\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let clk: Bit; let d: uint[8]; let q: uint[8];\n\
           let dut: Reg = { .clk = clk, .d = d, .q = q };\n\
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
    let golden: &[&[(&str, u64)]] = &[
        &[("T.clk", 0), ("T.d", 42), ("T.q", 0), ("T.dut.clk", 0), ("T.dut.d", 42), ("T.dut.q", 0), ("T.dut.s", 0)],
        &[("T.clk", 1), ("T.d", 42), ("T.q", 42), ("T.dut.clk", 1), ("T.dut.d", 42), ("T.dut.q", 42), ("T.dut.s", 42)],
        &[("T.clk", 0), ("T.d", 99), ("T.q", 42), ("T.dut.clk", 0), ("T.dut.d", 99), ("T.dut.q", 42), ("T.dut.s", 42)],
        &[("T.clk", 1), ("T.d", 99), ("T.q", 99), ("T.dut.clk", 1), ("T.dut.d", 99), ("T.dut.q", 99), ("T.dut.s", 99)],
        &[("T.clk", 1), ("T.d", 7), ("T.q", 99), ("T.dut.clk", 1), ("T.dut.d", 7), ("T.dut.q", 99), ("T.dut.s", 99)],
    ];
    check_seq(&d, &refs, golden);
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
           if clk.rising() {\n\
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
           let dut: Fsm = { .clk = clk, .go = go, .fin = fin, .active = active };\n\
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
    let golden: &[&[(&str, u64)]] = &[
        &[("T.clk", 0), ("T.go", 0), ("T.fin", 0), ("T.active", 0), ("T.dut.clk", 0), ("T.dut.go", 0), ("T.dut.fin", 0), ("T.dut.active", 0), ("T.dut.state", 0)],
        &[("T.clk", 1), ("T.go", 1), ("T.fin", 0), ("T.active", 1), ("T.dut.clk", 1), ("T.dut.go", 1), ("T.dut.fin", 0), ("T.dut.active", 1), ("T.dut.state", 1)],
        &[("T.clk", 0), ("T.go", 1), ("T.fin", 0), ("T.active", 1), ("T.dut.clk", 0), ("T.dut.go", 1), ("T.dut.fin", 0), ("T.dut.active", 1), ("T.dut.state", 1)],
        &[("T.clk", 1), ("T.go", 0), ("T.fin", 0), ("T.active", 1), ("T.dut.clk", 1), ("T.dut.go", 0), ("T.dut.fin", 0), ("T.dut.active", 1), ("T.dut.state", 1)],
        &[("T.clk", 0), ("T.go", 0), ("T.fin", 1), ("T.active", 1), ("T.dut.clk", 0), ("T.dut.go", 0), ("T.dut.fin", 1), ("T.dut.active", 1), ("T.dut.state", 1)],
        &[("T.clk", 1), ("T.go", 0), ("T.fin", 1), ("T.active", 0), ("T.dut.clk", 1), ("T.dut.go", 0), ("T.dut.fin", 1), ("T.dut.active", 0), ("T.dut.state", 2)],
        &[("T.clk", 0), ("T.go", 0), ("T.fin", 1), ("T.active", 0), ("T.dut.clk", 0), ("T.dut.go", 0), ("T.dut.fin", 1), ("T.dut.active", 0), ("T.dut.state", 2)],
        &[("T.clk", 1), ("T.go", 0), ("T.fin", 1), ("T.active", 0), ("T.dut.clk", 1), ("T.dut.go", 0), ("T.dut.fin", 1), ("T.dut.active", 0), ("T.dut.state", 0)],
        &[("T.clk", 0), ("T.go", 0), ("T.fin", 1), ("T.active", 0), ("T.dut.clk", 0), ("T.dut.go", 0), ("T.dut.fin", 1), ("T.dut.active", 0), ("T.dut.state", 0)],
    ];
    check_seq(&d, &refs, golden);
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
           let dut: Mux = { .sel = sel, .a = a, .b = b, .y = y };\n\
         }\n",
    );
    check(&d, &[("T.sel", 0), ("T.a", 111), ("T.b", 222)], &[("T.sel", 0), ("T.a", 111), ("T.b", 222), ("T.y", 222), ("T.dut.sel", 0), ("T.dut.a", 111), ("T.dut.b", 222), ("T.dut.y", 222)]);
    check(&d, &[("T.sel", 1), ("T.a", 111), ("T.b", 222)], &[("T.sel", 1), ("T.a", 111), ("T.b", 222), ("T.y", 111), ("T.dut.sel", 1), ("T.dut.a", 111), ("T.dut.b", 222), ("T.dut.y", 111)]);
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
           let dut: Alu = { .a = a, .b = b, .sum = sum, .hi = hi };\n\
         }\n",
    );
    check(&d, &[("T.a", 10), ("T.b", 20)], &[("T.a", 10), ("T.b", 20), ("T.sum", 30), ("T.hi", 0), ("T.dut.a", 10), ("T.dut.b", 20), ("T.dut.sum", 30), ("T.dut.hi", 0)]);
    check(&d, &[("T.a", 200), ("T.b", 100)], &[("T.a", 200), ("T.b", 100), ("T.sum", 44), ("T.hi", 12), ("T.dut.a", 200), ("T.dut.b", 100), ("T.dut.sum", 44), ("T.dut.hi", 12)]);
    check(&d, &[("T.a", 165), ("T.b", 15)], &[("T.a", 165), ("T.b", 15), ("T.sum", 180), ("T.hi", 10), ("T.dut.a", 165), ("T.dut.b", 15), ("T.dut.sum", 180), ("T.dut.hi", 10)]);
    check(&d, &[("T.a", 255), ("T.b", 1)], &[("T.a", 255), ("T.b", 1), ("T.sum", 0), ("T.hi", 15), ("T.dut.a", 255), ("T.dut.b", 1), ("T.dut.sum", 0), ("T.dut.hi", 15)]);
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
           let dut: C = { .a = a, .b = b, .y = y };\n\
         }\n",
    );
    check(&d, &[("T.a", 10), ("T.b", 5)], &[("T.a", 10), ("T.b", 5), ("T.y", 165), ("T.dut.a", 10), ("T.dut.b", 5), ("T.dut.y", 165)]);
    check(&d, &[("T.a", 15), ("T.b", 0)], &[("T.a", 15), ("T.b", 0), ("T.y", 240), ("T.dut.a", 15), ("T.dut.b", 0), ("T.dut.y", 240)]);
    check(&d, &[("T.a", 0), ("T.b", 15)], &[("T.a", 0), ("T.b", 15), ("T.y", 15), ("T.dut.a", 0), ("T.dut.b", 15), ("T.dut.y", 15)]);
    check(&d, &[("T.a", 3), ("T.b", 12)], &[("T.a", 3), ("T.b", 12), ("T.y", 60), ("T.dut.a", 3), ("T.dut.b", 12), ("T.dut.y", 60)]);
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
           let dut: Alu = { .op = op, .a = a, .b = b, .y = y };\n\
         }\n",
    );
    check(&d, &[("T.op", 0), ("T.a", 30), ("T.b", 12)], &[("T.op", 0), ("T.a", 30), ("T.b", 12), ("T.y", 42), ("T.dut.op", 0), ("T.dut.a", 30), ("T.dut.b", 12), ("T.dut.y", 42)]);
    check(&d, &[("T.op", 1), ("T.a", 30), ("T.b", 12)], &[("T.op", 1), ("T.a", 30), ("T.b", 12), ("T.y", 18), ("T.dut.op", 1), ("T.dut.a", 30), ("T.dut.b", 12), ("T.dut.y", 18)]);
    check(&d, &[("T.op", 2), ("T.a", 30), ("T.b", 12)], &[("T.op", 2), ("T.a", 30), ("T.b", 12), ("T.y", 30), ("T.dut.op", 2), ("T.dut.a", 30), ("T.dut.b", 12), ("T.dut.y", 30)]);
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
           let dut: S = { .p = p, .y = y };\n\
         }\n",
    );
    check(&d, &[("T.p.lo", 5), ("T.p.hi", 10)], &[("T.p.lo", 5), ("T.p.hi", 10), ("T.y", 165), ("T.dut.p.lo", 5), ("T.dut.p.hi", 10), ("T.dut.y", 165)]);
    check(&d, &[("T.p.lo", 15), ("T.p.hi", 0)], &[("T.p.lo", 15), ("T.p.hi", 0), ("T.y", 15), ("T.dut.p.lo", 15), ("T.dut.p.hi", 0), ("T.dut.y", 15)]);
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
           let dut: A = { .v = v, .y = y };\n\
         }\n",
    );
    check(&d, &[("T.v[0]", 3), ("T.v[1]", 12)], &[("T.v[0]", 3), ("T.v[1]", 12), ("T.y", 195), ("T.dut.v[0]", 3), ("T.dut.v[1]", 12), ("T.dut.y", 195)]);
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
           let dut: Ch = { .c = c, .is_a = is_a };\n\
         }\n",
    );
    check(&d, &[("T.c", 65)], &[("T.c", 65), ("T.is_a", 1), ("T.dut.c", 65), ("T.dut.is_a", 1)]);
    check(&d, &[("T.c", 66)], &[("T.c", 66), ("T.is_a", 0), ("T.dut.c", 66), ("T.dut.is_a", 0)]);
    check(&d, &[("T.c", 8364)], &[("T.c", 8364), ("T.is_a", 0), ("T.dut.c", 8364), ("T.dut.is_a", 0)]);
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
           let dut: Chain = { .i = i, .o = o };\n\
         }\n",
    );
    check(&d, &[("T.i", 0)], &[("T.i", 0), ("T.o", 2), ("T.dut.i", 0), ("T.dut.o", 2), ("T.dut.x", 1), ("T.dut.y", 2)]);
    check(&d, &[("T.i", 10)], &[("T.i", 10), ("T.o", 12), ("T.dut.i", 10), ("T.dut.o", 12), ("T.dut.x", 11), ("T.dut.y", 12)]);
    check(&d, &[("T.i", 100)], &[("T.i", 100), ("T.o", 102), ("T.dut.i", 100), ("T.dut.o", 102), ("T.dut.x", 101), ("T.dut.y", 102)]);
    check(&d, &[("T.i", 254)], &[("T.i", 254), ("T.o", 0), ("T.dut.i", 254), ("T.dut.o", 0), ("T.dut.x", 255), ("T.dut.y", 0)]);
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
             let inc: Inc = { .x = wires[i], .y = wires[i+1] };\n\
           }\n\
           b = wires[3];\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let a: uint[8]; let b: uint[8];\n\
           let dut: Chain = { .a = a, .b = b };\n\
         }\n",
    );
    check(&d, &[("T.a", 0)], &[("T.a", 0), ("T.b", 3), ("T.dut.a", 0), ("T.dut.b", 3), ("T.dut.wires[0]", 0), ("T.dut.wires[1]", 1), ("T.dut.wires[2]", 2), ("T.dut.wires[3]", 3), ("T.dut.inc_0.x", 0), ("T.dut.inc_0.y", 1), ("T.dut.inc_1.x", 1), ("T.dut.inc_1.y", 2), ("T.dut.inc_2.x", 2), ("T.dut.inc_2.y", 3)]);
    check(&d, &[("T.a", 10)], &[("T.a", 10), ("T.b", 13), ("T.dut.a", 10), ("T.dut.b", 13), ("T.dut.wires[0]", 10), ("T.dut.wires[1]", 11), ("T.dut.wires[2]", 12), ("T.dut.wires[3]", 13), ("T.dut.inc_0.x", 10), ("T.dut.inc_0.y", 11), ("T.dut.inc_1.x", 11), ("T.dut.inc_1.y", 12), ("T.dut.inc_2.x", 12), ("T.dut.inc_2.y", 13)]);
    check(&d, &[("T.a", 42)], &[("T.a", 42), ("T.b", 45), ("T.dut.a", 42), ("T.dut.b", 45), ("T.dut.wires[0]", 42), ("T.dut.wires[1]", 43), ("T.dut.wires[2]", 44), ("T.dut.wires[3]", 45), ("T.dut.inc_0.x", 42), ("T.dut.inc_0.y", 43), ("T.dut.inc_1.x", 43), ("T.dut.inc_1.y", 44), ("T.dut.inc_2.x", 44), ("T.dut.inc_2.y", 45)]);
    check(&d, &[("T.a", 252)], &[("T.a", 252), ("T.b", 255), ("T.dut.a", 252), ("T.dut.b", 255), ("T.dut.wires[0]", 252), ("T.dut.wires[1]", 253), ("T.dut.wires[2]", 254), ("T.dut.wires[3]", 255), ("T.dut.inc_0.x", 252), ("T.dut.inc_0.y", 253), ("T.dut.inc_1.x", 253), ("T.dut.inc_1.y", 254), ("T.dut.inc_2.x", 254), ("T.dut.inc_2.y", 255)]);
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
             let inc: Inc = { .x = wires[i], .y = wires[i+1] };\n\
           }\n\
           b = wires[3];\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let a: uint[8]; let b: uint[8];\n\
           let dut: Chain = { .a = a, .b = b };\n\
         }\n",
    );
    check(&d, &[("T.a", 0)], &[("T.a", 0), ("T.b", 3), ("T.dut.a", 0), ("T.dut.b", 3), ("T.dut.wires[0]", 0), ("T.dut.wires[1]", 1), ("T.dut.wires[2]", 2), ("T.dut.wires[3]", 3), ("T.dut.inc_2.x", 2), ("T.dut.inc_2.y", 3), ("T.dut.inc_1.x", 1), ("T.dut.inc_1.y", 2), ("T.dut.inc_0.x", 0), ("T.dut.inc_0.y", 1)]);
    check(&d, &[("T.a", 10)], &[("T.a", 10), ("T.b", 13), ("T.dut.a", 10), ("T.dut.b", 13), ("T.dut.wires[0]", 10), ("T.dut.wires[1]", 11), ("T.dut.wires[2]", 12), ("T.dut.wires[3]", 13), ("T.dut.inc_2.x", 12), ("T.dut.inc_2.y", 13), ("T.dut.inc_1.x", 11), ("T.dut.inc_1.y", 12), ("T.dut.inc_0.x", 10), ("T.dut.inc_0.y", 11)]);
    check(&d, &[("T.a", 42)], &[("T.a", 42), ("T.b", 45), ("T.dut.a", 42), ("T.dut.b", 45), ("T.dut.wires[0]", 42), ("T.dut.wires[1]", 43), ("T.dut.wires[2]", 44), ("T.dut.wires[3]", 45), ("T.dut.inc_2.x", 44), ("T.dut.inc_2.y", 45), ("T.dut.inc_1.x", 43), ("T.dut.inc_1.y", 44), ("T.dut.inc_0.x", 42), ("T.dut.inc_0.y", 43)]);
    check(&d, &[("T.a", 252)], &[("T.a", 252), ("T.b", 255), ("T.dut.a", 252), ("T.dut.b", 255), ("T.dut.wires[0]", 252), ("T.dut.wires[1]", 253), ("T.dut.wires[2]", 254), ("T.dut.wires[3]", 255), ("T.dut.inc_2.x", 254), ("T.dut.inc_2.y", 255), ("T.dut.inc_1.x", 253), ("T.dut.inc_1.y", 254), ("T.dut.inc_0.x", 252), ("T.dut.inc_0.y", 253)]);
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
           let a: Pad = { .drive = da, .en = ea, .pin = wire, .sensed = sa };\n\
           let b: Pad = { .drive = db, .en = eb, .pin = wire, .sensed = sb };\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let da: Logic; let ea: Logic; let db: Logic; let eb: Logic; let sa: Logic; let sb: Logic;\n\
           let dut: Bus = { .da = da, .ea = ea, .db = db, .eb = eb, .sa = sa, .sb = sb };\n\
         }\n",
    );
    // Logic codes: '0'=0 '1'=1 'Z'=2 'X'=3.
    check(&d, &[("T.ea", 1), ("T.da", 1), ("T.eb", 2), ("T.db", 0)], &[("T.da", 1), ("T.ea", 1), ("T.db", 0), ("T.eb", 2), ("T.sa", 1), ("T.sb", 1), ("T.dut.da", 1), ("T.dut.ea", 1), ("T.dut.db", 0), ("T.dut.eb", 2), ("T.dut.sa", 1), ("T.dut.sb", 1), ("T.dut.wire", 1), ("T.dut.a.drive", 1), ("T.dut.a.en", 1), ("T.dut.a.pin", 0), ("T.dut.a.sensed", 1), ("T.dut.b.drive", 0), ("T.dut.b.en", 2), ("T.dut.b.pin", 0), ("T.dut.b.sensed", 1)]); // A drives 1, B tristate
    check(&d, &[("T.ea", 2), ("T.da", 0), ("T.eb", 1), ("T.db", 0)], &[("T.da", 0), ("T.ea", 2), ("T.db", 0), ("T.eb", 1), ("T.sa", 0), ("T.sb", 0), ("T.dut.da", 0), ("T.dut.ea", 2), ("T.dut.db", 0), ("T.dut.eb", 1), ("T.dut.sa", 0), ("T.dut.sb", 0), ("T.dut.wire", 0), ("T.dut.a.drive", 0), ("T.dut.a.en", 2), ("T.dut.a.pin", 0), ("T.dut.a.sensed", 0), ("T.dut.b.drive", 0), ("T.dut.b.en", 1), ("T.dut.b.pin", 0), ("T.dut.b.sensed", 0)]); // B drives 0, A tristate
    check(&d, &[("T.ea", 1), ("T.da", 1), ("T.eb", 1), ("T.db", 0)], &[("T.da", 1), ("T.ea", 1), ("T.db", 0), ("T.eb", 1), ("T.sa", 3), ("T.sb", 3), ("T.dut.da", 1), ("T.dut.ea", 1), ("T.dut.db", 0), ("T.dut.eb", 1), ("T.dut.sa", 3), ("T.dut.sb", 3), ("T.dut.wire", 3), ("T.dut.a.drive", 1), ("T.dut.a.en", 1), ("T.dut.a.pin", 0), ("T.dut.a.sensed", 3), ("T.dut.b.drive", 0), ("T.dut.b.en", 1), ("T.dut.b.pin", 0), ("T.dut.b.sensed", 3)]); // both drive, disagree -> X
    check(&d, &[("T.ea", 1), ("T.da", 1), ("T.eb", 1), ("T.db", 1)], &[("T.da", 1), ("T.ea", 1), ("T.db", 1), ("T.eb", 1), ("T.sa", 1), ("T.sb", 1), ("T.dut.da", 1), ("T.dut.ea", 1), ("T.dut.db", 1), ("T.dut.eb", 1), ("T.dut.sa", 1), ("T.dut.sb", 1), ("T.dut.wire", 1), ("T.dut.a.drive", 1), ("T.dut.a.en", 1), ("T.dut.a.pin", 0), ("T.dut.a.sensed", 1), ("T.dut.b.drive", 1), ("T.dut.b.en", 1), ("T.dut.b.pin", 0), ("T.dut.b.sensed", 1)]); // both drive 1 -> 1
    check(&d, &[("T.ea", 2), ("T.da", 0), ("T.eb", 2), ("T.db", 0)], &[("T.da", 0), ("T.ea", 2), ("T.db", 0), ("T.eb", 2), ("T.sa", 2), ("T.sb", 2), ("T.dut.da", 0), ("T.dut.ea", 2), ("T.dut.db", 0), ("T.dut.eb", 2), ("T.dut.sa", 2), ("T.dut.sb", 2), ("T.dut.wire", 2), ("T.dut.a.drive", 0), ("T.dut.a.en", 2), ("T.dut.a.pin", 0), ("T.dut.a.sensed", 2), ("T.dut.b.drive", 0), ("T.dut.b.en", 2), ("T.dut.b.pin", 0), ("T.dut.b.sensed", 2)]); // neither drives -> Z
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
           let p: Producer = { .vin = vin, .din = din, .s = wire };\n\
           let c: Consumer = { .s = wire, .got = got };\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let vin: Logic; let din: uint[8]; let got: uint[8];\n\
           let dut: Link = { .vin = vin, .din = din, .got = got };\n\
         }\n",
    );
    check(&d, &[("T.vin", 1), ("T.din", 42)], &[("T.vin", 1), ("T.din", 42), ("T.got", 42), ("T.dut.vin", 1), ("T.dut.din", 42), ("T.dut.got", 42), ("T.dut.wire.valid", 1), ("T.dut.wire.data", 42), ("T.dut.p.vin", 1), ("T.dut.p.din", 42), ("T.dut.p.s.valid", 1), ("T.dut.p.s.data", 42), ("T.dut.c.s.valid", 1), ("T.dut.c.s.data", 42), ("T.dut.c.got", 42)]);
    check(&d, &[("T.vin", 0), ("T.din", 42)], &[("T.vin", 0), ("T.din", 42), ("T.got", 0), ("T.dut.vin", 0), ("T.dut.din", 42), ("T.dut.got", 0), ("T.dut.wire.valid", 0), ("T.dut.wire.data", 42), ("T.dut.p.vin", 0), ("T.dut.p.din", 42), ("T.dut.p.s.valid", 0), ("T.dut.p.s.data", 42), ("T.dut.c.s.valid", 0), ("T.dut.c.s.data", 42), ("T.dut.c.got", 0)]);
    check(&d, &[("T.vin", 1), ("T.din", 200)], &[("T.vin", 1), ("T.din", 200), ("T.got", 200), ("T.dut.vin", 1), ("T.dut.din", 200), ("T.dut.got", 200), ("T.dut.wire.valid", 1), ("T.dut.wire.data", 200), ("T.dut.p.vin", 1), ("T.dut.p.din", 200), ("T.dut.p.s.valid", 1), ("T.dut.p.s.data", 200), ("T.dut.c.s.valid", 1), ("T.dut.c.s.data", 200), ("T.dut.c.got", 200)]);
    check(&d, &[("T.vin", 0), ("T.din", 7)], &[("T.vin", 0), ("T.din", 7), ("T.got", 0), ("T.dut.vin", 0), ("T.dut.din", 7), ("T.dut.got", 0), ("T.dut.wire.valid", 0), ("T.dut.wire.data", 7), ("T.dut.p.vin", 0), ("T.dut.p.din", 7), ("T.dut.p.s.valid", 0), ("T.dut.p.s.data", 7), ("T.dut.c.s.valid", 0), ("T.dut.c.s.data", 7), ("T.dut.c.got", 0)]);
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
           let dut: Dec = { .op = op, .kind = kind };\n\
         }\n",
    );
    check(&d, &[("T.op", 0)], &[("T.op", 0), ("T.kind", 0), ("T.dut.op", 0), ("T.dut.kind", 0)]);
    check(&d, &[("T.op", 1)], &[("T.op", 1), ("T.kind", 0), ("T.dut.op", 1), ("T.dut.kind", 0)]);
    check(&d, &[("T.op", 2)], &[("T.op", 2), ("T.kind", 0), ("T.dut.op", 2), ("T.dut.kind", 0)]);
    check(&d, &[("T.op", 3)], &[("T.op", 3), ("T.kind", 0), ("T.dut.op", 3), ("T.dut.kind", 0)]);
    check(&d, &[("T.op", 4)], &[("T.op", 4), ("T.kind", 1), ("T.dut.op", 4), ("T.dut.kind", 1)]);
    check(&d, &[("T.op", 5)], &[("T.op", 5), ("T.kind", 1), ("T.dut.op", 5), ("T.dut.kind", 1)]);
    check(&d, &[("T.op", 6)], &[("T.op", 6), ("T.kind", 1), ("T.dut.op", 6), ("T.dut.kind", 1)]);
    check(&d, &[("T.op", 7)], &[("T.op", 7), ("T.kind", 1), ("T.dut.op", 7), ("T.dut.kind", 1)]);
    check(&d, &[("T.op", 8)], &[("T.op", 8), ("T.kind", 3), ("T.dut.op", 8), ("T.dut.kind", 3)]);
    check(&d, &[("T.op", 9)], &[("T.op", 9), ("T.kind", 3), ("T.dut.op", 9), ("T.dut.kind", 3)]);
    check(&d, &[("T.op", 10)], &[("T.op", 10), ("T.kind", 2), ("T.dut.op", 10), ("T.dut.kind", 2)]);
    check(&d, &[("T.op", 11)], &[("T.op", 11), ("T.kind", 2), ("T.dut.op", 11), ("T.dut.kind", 2)]);
    check(&d, &[("T.op", 12)], &[("T.op", 12), ("T.kind", 3), ("T.dut.op", 12), ("T.dut.kind", 3)]);
    check(&d, &[("T.op", 13)], &[("T.op", 13), ("T.kind", 3), ("T.dut.op", 13), ("T.dut.kind", 3)]);
    check(&d, &[("T.op", 14)], &[("T.op", 14), ("T.kind", 2), ("T.dut.op", 14), ("T.dut.kind", 2)]);
    check(&d, &[("T.op", 15)], &[("T.op", 15), ("T.kind", 2), ("T.dut.op", 15), ("T.dut.kind", 2)]);
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
           let dut: Split = { .w = w, .hi = hi, .lo = lo };\n\
         }\n",
    );
    check(&d, &[("T.w", 0)], &[("T.w", 0), ("T.hi", 0), ("T.lo", 0), ("T.dut.w", 0), ("T.dut.hi", 0), ("T.dut.lo", 0)]);
    check(&d, &[("T.w", 163)], &[("T.w", 163), ("T.hi", 10), ("T.lo", 3), ("T.dut.w", 163), ("T.dut.hi", 10), ("T.dut.lo", 3)]);
    check(&d, &[("T.w", 255)], &[("T.w", 255), ("T.hi", 15), ("T.lo", 15), ("T.dut.w", 255), ("T.dut.hi", 15), ("T.dut.lo", 15)]);
    check(&d, &[("T.w", 15)], &[("T.w", 15), ("T.hi", 0), ("T.lo", 15), ("T.dut.w", 15), ("T.dut.hi", 0), ("T.dut.lo", 15)]);
    check(&d, &[("T.w", 240)], &[("T.w", 240), ("T.hi", 15), ("T.lo", 0), ("T.dut.w", 240), ("T.dut.hi", 15), ("T.dut.lo", 0)]);
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
           let dut: Chain = { .a = a, .b = b, .mid = mid };\n\
         }\n",
    );
    check(&d, &[("T.a", 0)], &[("T.a", 0), ("T.b", 3), ("T.mid", 2), ("T.dut.a", 0), ("T.dut.b", 3), ("T.dut.mid", 2), ("T.dut.w[0]", 0), ("T.dut.w[1]", 1), ("T.dut.w[2]", 2), ("T.dut.w[3]", 3), ("T.dut.stage[0]", 0), ("T.dut.stage[1]", 0), ("T.dut.stage[2]", 0), ("T.dut.stage[0].x", 0), ("T.dut.stage[0].y", 1), ("T.dut.stage[1].x", 1), ("T.dut.stage[1].y", 2), ("T.dut.stage[2].x", 2), ("T.dut.stage[2].y", 3)]);
    check(&d, &[("T.a", 10)], &[("T.a", 10), ("T.b", 13), ("T.mid", 12), ("T.dut.a", 10), ("T.dut.b", 13), ("T.dut.mid", 12), ("T.dut.w[0]", 10), ("T.dut.w[1]", 11), ("T.dut.w[2]", 12), ("T.dut.w[3]", 13), ("T.dut.stage[0]", 0), ("T.dut.stage[1]", 0), ("T.dut.stage[2]", 0), ("T.dut.stage[0].x", 10), ("T.dut.stage[0].y", 11), ("T.dut.stage[1].x", 11), ("T.dut.stage[1].y", 12), ("T.dut.stage[2].x", 12), ("T.dut.stage[2].y", 13)]);
    check(&d, &[("T.a", 40)], &[("T.a", 40), ("T.b", 43), ("T.mid", 42), ("T.dut.a", 40), ("T.dut.b", 43), ("T.dut.mid", 42), ("T.dut.w[0]", 40), ("T.dut.w[1]", 41), ("T.dut.w[2]", 42), ("T.dut.w[3]", 43), ("T.dut.stage[0]", 0), ("T.dut.stage[1]", 0), ("T.dut.stage[2]", 0), ("T.dut.stage[0].x", 40), ("T.dut.stage[0].y", 41), ("T.dut.stage[1].x", 41), ("T.dut.stage[1].y", 42), ("T.dut.stage[2].x", 42), ("T.dut.stage[2].y", 43)]);
    check(&d, &[("T.a", 250)], &[("T.a", 250), ("T.b", 253), ("T.mid", 252), ("T.dut.a", 250), ("T.dut.b", 253), ("T.dut.mid", 252), ("T.dut.w[0]", 250), ("T.dut.w[1]", 251), ("T.dut.w[2]", 252), ("T.dut.w[3]", 253), ("T.dut.stage[0]", 0), ("T.dut.stage[1]", 0), ("T.dut.stage[2]", 0), ("T.dut.stage[0].x", 250), ("T.dut.stage[0].y", 251), ("T.dut.stage[1].x", 251), ("T.dut.stage[1].y", 252), ("T.dut.stage[2].x", 252), ("T.dut.stage[2].y", 253)]);
}

#[test]
fn range_pattern_agrees() {
    // Integer-literal and inclusive-range match arms (`0..9`, `100`) over a
    // numeric scrutinee; both engines must agree across the boundaries.
    let d = lower(
        "module m;\n\
         entity E { in a: uint[8]; out y: uint[8]; }\n\
         impl E { y = match a { 0..9 => 1, 10..99 => 2, 100 => 3, _ => 4 }; }\n\
         #[top]\n\
         entity T {}\n\
         impl T { let a: uint[8]; let y: uint[8]; let dut: E = { .a = a, .y = y }; }\n",
    );
    check(&d, &[("T.a", 0)], &[("T.a", 0), ("T.y", 1), ("T.dut.a", 0), ("T.dut.y", 1)]);
    check(&d, &[("T.a", 9)], &[("T.a", 9), ("T.y", 1), ("T.dut.a", 9), ("T.dut.y", 1)]);
    check(&d, &[("T.a", 10)], &[("T.a", 10), ("T.y", 2), ("T.dut.a", 10), ("T.dut.y", 2)]);
    check(&d, &[("T.a", 99)], &[("T.a", 99), ("T.y", 2), ("T.dut.a", 99), ("T.dut.y", 2)]);
    check(&d, &[("T.a", 100)], &[("T.a", 100), ("T.y", 3), ("T.dut.a", 100), ("T.dut.y", 3)]);
    check(&d, &[("T.a", 101)], &[("T.a", 101), ("T.y", 4), ("T.dut.a", 101), ("T.dut.y", 4)]);
    check(&d, &[("T.a", 200)], &[("T.a", 200), ("T.y", 4), ("T.dut.a", 200), ("T.dut.y", 4)]);
}

#[test]
fn or_pattern_agrees() {
    // `A | B => ..` matches if any alternative matches (spec 3.22): its
    // condition is the OR of the alternatives'. Both engines must agree.
    let d = lower(
        "module m;\n\
         enum S { A, B, C, D }\n\
         entity E { in s: S; out y: uint[8]; }\n\
         impl E { y = match s { S::A | S::B => 10, S::C => 20, _ => 30 }; }\n\
         #[top]\n\
         entity T {}\n\
         impl T { let s: S; let y: uint[8]; let dut: E = { .s = s, .y = y }; }\n",
    );
    check(&d, &[("T.s", 0)], &[("T.s", 0), ("T.y", 10), ("T.dut.s", 0), ("T.dut.y", 10)]);
    check(&d, &[("T.s", 1)], &[("T.s", 1), ("T.y", 10), ("T.dut.s", 1), ("T.dut.y", 10)]);
    check(&d, &[("T.s", 2)], &[("T.s", 2), ("T.y", 20), ("T.dut.s", 2), ("T.dut.y", 20)]);
    check(&d, &[("T.s", 3)], &[("T.s", 3), ("T.y", 30), ("T.dut.s", 3), ("T.dut.y", 30)]);
}

#[test]
fn match_expression_agrees() {
    // `match` in value position lowers to a first-match Select chain; both
    // engines must agree across every arm and the wildcard default.
    let d = lower(
        "module m;\n\
         enum Op { Add, Sub, Pass }\n\
         entity E { in op: Op; in a: uint[8]; in b: uint[8]; out y: uint[8]; }\n\
         impl E { y = match op { Op::Add => a + b, Op::Sub => a - b, _ => a }; }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let op: Op; let a: uint[8]; let b: uint[8]; let y: uint[8];\n\
           let dut: E = { .op = op, .a = a, .b = b, .y = y };\n\
         }\n",
    );
    check(&d, &[("T.op", 0), ("T.a", 30), ("T.b", 12)], &[("T.op", 0), ("T.a", 30), ("T.b", 12), ("T.y", 42), ("T.dut.op", 0), ("T.dut.a", 30), ("T.dut.b", 12), ("T.dut.y", 42)]);
    check(&d, &[("T.op", 1), ("T.a", 30), ("T.b", 12)], &[("T.op", 1), ("T.a", 30), ("T.b", 12), ("T.y", 18), ("T.dut.op", 1), ("T.dut.a", 30), ("T.dut.b", 12), ("T.dut.y", 18)]);
    check(&d, &[("T.op", 2), ("T.a", 30), ("T.b", 12)], &[("T.op", 2), ("T.a", 30), ("T.b", 12), ("T.y", 30), ("T.dut.op", 2), ("T.dut.a", 30), ("T.dut.b", 12), ("T.dut.y", 30)]);
}

#[test]
fn array_literal_agrees() {
    // `[a, b, c, d]` fills an array signal one element per value; a runtime
    // index reads back a lookup table. Both engines must agree per element.
    let d = lower(
        "module m;\n\
         entity E { in sel: uint[8]; out y: uint[8]; }\n\
         impl E {\n\
           let table: uint[8][4];\n\
           table = [10, 20, 30, 40];\n\
           y = table[sel];\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T { let sel: uint[8]; let y: uint[8]; let dut: E = { .sel = sel, .y = y }; }\n",
    );
    check(&d, &[("T.sel", 0)], &[("T.sel", 0), ("T.y", 10), ("T.dut.sel", 0), ("T.dut.y", 10), ("T.dut.table[0]", 10), ("T.dut.table[1]", 20), ("T.dut.table[2]", 30), ("T.dut.table[3]", 40)]);
    check(&d, &[("T.sel", 1)], &[("T.sel", 1), ("T.y", 20), ("T.dut.sel", 1), ("T.dut.y", 20), ("T.dut.table[0]", 10), ("T.dut.table[1]", 20), ("T.dut.table[2]", 30), ("T.dut.table[3]", 40)]);
    check(&d, &[("T.sel", 2)], &[("T.sel", 2), ("T.y", 30), ("T.dut.sel", 2), ("T.dut.y", 30), ("T.dut.table[0]", 10), ("T.dut.table[1]", 20), ("T.dut.table[2]", 30), ("T.dut.table[3]", 40)]);
    check(&d, &[("T.sel", 3)], &[("T.sel", 3), ("T.y", 40), ("T.dut.sel", 3), ("T.dut.y", 40), ("T.dut.table[0]", 10), ("T.dut.table[1]", 20), ("T.dut.table[2]", 30), ("T.dut.table[3]", 40)]);
}

#[test]
fn positional_connection_agrees() {
    // Positional instance connection `Add { a, b }` binds by port order; both
    // engines must agree with the named form's behavior.
    let d = lower(
        "module m;\n\
         entity Add { in a: uint[8]; in b: uint[8]; out y: uint[8]; }\n\
         impl Add { y = a + b; }\n\
         entity E { in p: uint[8]; in q: uint[8]; out y: uint[8]; }\n\
         impl E { let s: Add = { p, q, y }; }\n\
         #[top]\n\
         entity T {}\n\
         impl T { let p: uint[8]; let q: uint[8]; let y: uint[8]; let dut: E = { .p = p, .q = q, .y = y }; }\n",
    );
    check(&d, &[("T.p", 3), ("T.q", 4)], &[("T.p", 3), ("T.q", 4), ("T.y", 7), ("T.dut.p", 3), ("T.dut.q", 4), ("T.dut.y", 7), ("T.dut.s.a", 3), ("T.dut.s.b", 4), ("T.dut.s.y", 7)]);
    check(&d, &[("T.p", 10), ("T.q", 20)], &[("T.p", 10), ("T.q", 20), ("T.y", 30), ("T.dut.p", 10), ("T.dut.q", 20), ("T.dut.y", 30), ("T.dut.s.a", 10), ("T.dut.s.b", 20), ("T.dut.s.y", 30)]);
    check(&d, &[("T.p", 200), ("T.q", 55)], &[("T.p", 200), ("T.q", 55), ("T.y", 255), ("T.dut.p", 200), ("T.dut.q", 55), ("T.dut.y", 255), ("T.dut.s.a", 200), ("T.dut.s.b", 55), ("T.dut.s.y", 255)]);
}

#[test]
fn post_decl_connection_agrees() {
    // Post-declaration wiring `s1.a = x; s2.a = s1.y;` chains two instances
    // through instance-qualified port assignments; both engines must agree.
    let d = lower(
        "module m;\n\
         entity Inc { in a: uint[8]; out y: uint[8]; }\n\
         impl Inc { y = a + 1; }\n\
         entity E { in x: uint[8]; out z: uint[8]; }\n\
         impl E {\n\
           let s1: Inc = {};\n\
           let s2: Inc = {};\n\
           s1.a = x;\n\
           s2.a = s1.y;\n\
           z = s2.y;\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T { let x: uint[8]; let z: uint[8]; let dut: E = { .x = x, .z = z }; }\n",
    );
    check(&d, &[("T.x", 0)], &[("T.x", 0), ("T.z", 2), ("T.dut.x", 0), ("T.dut.z", 2), ("T.dut.s1.a", 0), ("T.dut.s1.y", 1), ("T.dut.s2.a", 1), ("T.dut.s2.y", 2)]);
    check(&d, &[("T.x", 5)], &[("T.x", 5), ("T.z", 7), ("T.dut.x", 5), ("T.dut.z", 7), ("T.dut.s1.a", 5), ("T.dut.s1.y", 6), ("T.dut.s2.a", 6), ("T.dut.s2.y", 7)]);
    check(&d, &[("T.x", 100)], &[("T.x", 100), ("T.z", 102), ("T.dut.x", 100), ("T.dut.z", 102), ("T.dut.s1.a", 100), ("T.dut.s1.y", 101), ("T.dut.s2.a", 101), ("T.dut.s2.y", 102)]);
    check(&d, &[("T.x", 254)], &[("T.x", 254), ("T.z", 0), ("T.dut.x", 254), ("T.dut.z", 0), ("T.dut.s1.a", 254), ("T.dut.s1.y", 255), ("T.dut.s2.a", 255), ("T.dut.s2.y", 0)]);
}

#[test]
fn generate_if_agrees() {
    // A generate-`if` on a parameter conditionally instantiates a sub-entity:
    // `EN > 0` inserts an `Inc` (so `T.dut.s.*` exists), else the input passes
    // through. The design is re-lowered per specialization.
    let cases: &[(u64, &[(u64, &[(&str, u64)])])] = &[
        (1, &[
            (0, &[("T.a", 0), ("T.y", 1), ("T.dut.a", 0), ("T.dut.y", 1), ("T.dut.s.a", 0), ("T.dut.s.y", 1)]),
            (5, &[("T.a", 5), ("T.y", 6), ("T.dut.a", 5), ("T.dut.y", 6), ("T.dut.s.a", 5), ("T.dut.s.y", 6)]),
            (200, &[("T.a", 200), ("T.y", 201), ("T.dut.a", 200), ("T.dut.y", 201), ("T.dut.s.a", 200), ("T.dut.s.y", 201)]),
        ]),
        (0, &[
            (0, &[("T.a", 0), ("T.y", 0), ("T.dut.a", 0), ("T.dut.y", 0)]),
            (5, &[("T.a", 5), ("T.y", 5), ("T.dut.a", 5), ("T.dut.y", 5)]),
            (200, &[("T.a", 200), ("T.y", 200), ("T.dut.a", 200), ("T.dut.y", 200)]),
        ]),
    ];
    for &(en, scenarios) in cases {
        let d = lower(&format!(
            "module m;\n\
             entity Inc {{ in a: uint[8]; out y: uint[8]; }}\n\
             impl Inc {{ y = a + 1; }}\n\
             entity Top<EN: integer> {{ in a: uint[8]; out y: uint[8]; }}\n\
             impl Top<EN: integer> {{\n\
               if EN > 0 {{ let s: Inc = {{ .a = a, .y = y }}; }} else {{ y = a; }}\n\
             }}\n\
             #[top]\n\
             entity T {{}}\n\
             impl T {{ let a: uint[8]; let y: uint[8]; let dut: Top<EN = {en}> = {{ .a = a, .y = y }}; }}\n"
        ));
        for &(a, expect) in scenarios {
            check(&d, &[("T.a", a)], expect);
        }
    }
}

#[test]
fn generate_for_if_chain_agrees() {
    // A generate-`for` with a nested generate-`if` builds a chain of `N` `Inc`
    // stages (out of 3), the rest passing through — instance-array elements
    // created conditionally. Re-lowered per `N`.
    let cases: &[(u64, &[(u64, &[(&str, u64)])])] = &[
        (0, &[
            (0, &[("T.a", 0), ("T.y", 0), ("T.dut.a", 0), ("T.dut.y", 0), ("T.dut.w[0]", 0), ("T.dut.w[1]", 0), ("T.dut.w[2]", 0), ("T.dut.w[3]", 0), ("T.dut.stage[0]", 0), ("T.dut.stage[1]", 0), ("T.dut.stage[2]", 0)]),
            (10, &[("T.a", 10), ("T.y", 10), ("T.dut.a", 10), ("T.dut.y", 10), ("T.dut.w[0]", 10), ("T.dut.w[1]", 10), ("T.dut.w[2]", 10), ("T.dut.w[3]", 10), ("T.dut.stage[0]", 0), ("T.dut.stage[1]", 0), ("T.dut.stage[2]", 0)]),
            (250, &[("T.a", 250), ("T.y", 250), ("T.dut.a", 250), ("T.dut.y", 250), ("T.dut.w[0]", 250), ("T.dut.w[1]", 250), ("T.dut.w[2]", 250), ("T.dut.w[3]", 250), ("T.dut.stage[0]", 0), ("T.dut.stage[1]", 0), ("T.dut.stage[2]", 0)]),
        ]),
        (1, &[
            (0, &[("T.a", 0), ("T.y", 1), ("T.dut.a", 0), ("T.dut.y", 1), ("T.dut.w[0]", 0), ("T.dut.w[1]", 1), ("T.dut.w[2]", 1), ("T.dut.w[3]", 1), ("T.dut.stage[0].a", 0), ("T.dut.stage[0].y", 1)]),
            (10, &[("T.a", 10), ("T.y", 11), ("T.dut.a", 10), ("T.dut.y", 11), ("T.dut.w[0]", 10), ("T.dut.w[1]", 11), ("T.dut.w[2]", 11), ("T.dut.w[3]", 11), ("T.dut.stage[0].a", 10), ("T.dut.stage[0].y", 11)]),
            (250, &[("T.a", 250), ("T.y", 251), ("T.dut.a", 250), ("T.dut.y", 251), ("T.dut.w[0]", 250), ("T.dut.w[1]", 251), ("T.dut.w[2]", 251), ("T.dut.w[3]", 251), ("T.dut.stage[0].a", 250), ("T.dut.stage[0].y", 251)]),
        ]),
        (2, &[
            (0, &[("T.a", 0), ("T.y", 2), ("T.dut.y", 2), ("T.dut.w[1]", 1), ("T.dut.w[2]", 2), ("T.dut.w[3]", 2), ("T.dut.stage[0].y", 1), ("T.dut.stage[1].a", 1), ("T.dut.stage[1].y", 2)]),
            (10, &[("T.a", 10), ("T.y", 12), ("T.dut.y", 12), ("T.dut.w[1]", 11), ("T.dut.w[2]", 12), ("T.dut.w[3]", 12), ("T.dut.stage[0].y", 11), ("T.dut.stage[1].a", 11), ("T.dut.stage[1].y", 12)]),
            (250, &[("T.a", 250), ("T.y", 252), ("T.dut.y", 252), ("T.dut.w[1]", 251), ("T.dut.w[2]", 252), ("T.dut.w[3]", 252), ("T.dut.stage[0].y", 251), ("T.dut.stage[1].a", 251), ("T.dut.stage[1].y", 252)]),
        ]),
        (3, &[
            (0, &[("T.a", 0), ("T.y", 3), ("T.dut.y", 3), ("T.dut.w[3]", 3), ("T.dut.stage[1].y", 2), ("T.dut.stage[2].a", 2), ("T.dut.stage[2].y", 3)]),
            (10, &[("T.a", 10), ("T.y", 13), ("T.dut.y", 13), ("T.dut.w[3]", 13), ("T.dut.stage[1].y", 12), ("T.dut.stage[2].a", 12), ("T.dut.stage[2].y", 13)]),
            (250, &[("T.a", 250), ("T.y", 253), ("T.dut.y", 253), ("T.dut.w[3]", 253), ("T.dut.stage[1].y", 252), ("T.dut.stage[2].a", 252), ("T.dut.stage[2].y", 253)]),
        ]),
    ];
    for &(n, scenarios) in cases {
        let d = lower(&format!(
            "module m;\n\
             entity Inc {{ in a: uint[8]; out y: uint[8]; }}\n\
             impl Inc {{ y = a + 1; }}\n\
             entity Chain<N: integer> {{ in a: uint[8]; out y: uint[8]; }}\n\
             impl Chain<N: integer> {{\n\
               let w: uint[8][4];\n\
               let stage: Inc[3];\n\
               w[0] = a;\n\
               for i in 0..2 {{\n\
                 if i < N {{ stage[i] = Inc {{ .a = w[i], .y = w[i+1] }}; }}\n\
                 else {{ w[i+1] = w[i]; }}\n\
               }}\n\
               y = w[3];\n\
             }}\n\
             #[top]\n\
             entity T {{}}\n\
             impl T {{ let a: uint[8]; let y: uint[8]; let dut: Chain<N = {n}> = {{ .a = a, .y = y }}; }}\n"
        ));
        for &(a, expect) in scenarios {
            check(&d, &[("T.a", a)], expect);
        }
    }
}

#[test]
fn generic_entity_agrees() {
    // A generic entity `Buf<T>` specializes its `T`-typed ports and internal
    // state to the type argument (`Buf<uint[8]>`), so signals get the concrete
    // width. Both engines must agree.
    //
    // The top entity is deliberately named `T` — the same spelling as `Buf`'s
    // type parameter. `let s: T` must resolve to the *parameter* (`uint[8]`, a
    // signal), not the entity `T` (which would make `s` a recursive instance
    // and loop forever). Keep the name collision: it guards that regression.
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
           let dut: Buf<uint[8]> = { .a = a, .b = b, .y = y };\n\
         }\n",
    );
    assert_eq!(d.signals[id(&d, "T.dut.s").0 as usize].width, 8, "Buf<uint[8]>.s is 8-bit");
    check(&d, &[("T.a", 10), ("T.b", 20)], &[("T.a", 10), ("T.b", 20), ("T.y", 30), ("T.dut.a", 10), ("T.dut.b", 20), ("T.dut.y", 30), ("T.dut.s", 30)]);
    check(&d, &[("T.a", 200), ("T.b", 100)], &[("T.a", 200), ("T.b", 100), ("T.y", 44), ("T.dut.a", 200), ("T.dut.b", 100), ("T.dut.y", 44), ("T.dut.s", 44)]);
    check(&d, &[("T.a", 255), ("T.b", 1)], &[("T.a", 255), ("T.b", 1), ("T.y", 0), ("T.dut.a", 255), ("T.dut.b", 1), ("T.dut.y", 0), ("T.dut.s", 0)]);
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
         impl T { let x: uint[8]; let sum: uint[8]; let dut: E = { .x = x, .sum = sum }; }\n",
    );
    check(&d, &[("T.x", 10)], &[("T.x", 10), ("T.sum", 20), ("T.dut.x", 10), ("T.dut.sum", 20), ("T.dut.p.a", 10), ("T.dut.p.b", 10)]);
    check(&d, &[("T.x", 100)], &[("T.x", 100), ("T.sum", 200), ("T.dut.x", 100), ("T.dut.sum", 200), ("T.dut.p.a", 100), ("T.dut.p.b", 100)]);
    check(&d, &[("T.x", 200)], &[("T.x", 200), ("T.sum", 144), ("T.dut.x", 200), ("T.dut.sum", 144), ("T.dut.p.a", 200), ("T.dut.p.b", 200)]);
    check(&d, &[("T.x", 255)], &[("T.x", 255), ("T.sum", 254), ("T.dut.x", 255), ("T.dut.sum", 254), ("T.dut.p.a", 255), ("T.dut.p.b", 255)]);
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
           let p: Producer = { .bus = link, .d = d, .canpush = canpush };\n\
           let c: Consumer = { .bus = link, .accept = accept, .got = got };\n\
         }\n",
    );
    check(&d, &[("T.d", 77), ("T.accept", 1)], &[("T.d", 77), ("T.accept", 1), ("T.got", 77), ("T.canpush", 1), ("T.link.valid", 1), ("T.link.ready", 1), ("T.link.data", 77), ("T.p.bus.valid", 1), ("T.p.bus.ready", 1), ("T.p.bus.data", 77), ("T.p.d", 77), ("T.p.canpush", 1), ("T.c.bus.valid", 1), ("T.c.bus.ready", 1), ("T.c.bus.data", 77), ("T.c.accept", 1), ("T.c.got", 77)]);
    check(&d, &[("T.d", 200), ("T.accept", 0)], &[("T.d", 200), ("T.accept", 0), ("T.got", 200), ("T.canpush", 0), ("T.link.valid", 1), ("T.link.ready", 0), ("T.link.data", 200), ("T.p.bus.valid", 1), ("T.p.bus.ready", 0), ("T.p.bus.data", 200), ("T.p.d", 200), ("T.p.canpush", 0), ("T.c.bus.valid", 1), ("T.c.bus.ready", 0), ("T.c.bus.data", 200), ("T.c.accept", 0), ("T.c.got", 200)]);
    check(&d, &[("T.d", 0), ("T.accept", 1)], &[("T.d", 0), ("T.accept", 1), ("T.got", 0), ("T.canpush", 1), ("T.link.valid", 1), ("T.link.ready", 1), ("T.link.data", 0), ("T.p.bus.valid", 1), ("T.p.bus.ready", 1), ("T.p.bus.data", 0), ("T.p.d", 0), ("T.p.canpush", 1), ("T.c.bus.valid", 1), ("T.c.bus.ready", 1), ("T.c.bus.data", 0), ("T.c.accept", 1), ("T.c.got", 0)]);
    check(&d, &[("T.d", 255), ("T.accept", 0)], &[("T.d", 255), ("T.accept", 0), ("T.got", 255), ("T.canpush", 0), ("T.link.valid", 1), ("T.link.ready", 0), ("T.link.data", 255), ("T.p.bus.valid", 1), ("T.p.bus.ready", 0), ("T.p.bus.data", 255), ("T.p.d", 255), ("T.p.canpush", 0), ("T.c.bus.valid", 1), ("T.c.bus.ready", 0), ("T.c.bus.data", 255), ("T.c.accept", 0), ("T.c.got", 255)]);
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
           let dut: A = { .a = a, .b = b, .s = s };\n\
         }\n",
    );
    // s is 8-bit: 200+100 = 300 -> 44, 255+1 -> 0.
    check(&d, &[("T.a", 200), ("T.b", 100)], &[("T.a", 200), ("T.b", 100), ("T.s", 44), ("T.dut.a", 200), ("T.dut.b", 100), ("T.dut.s", 44)]);
    check(&d, &[("T.a", 255), ("T.b", 1)], &[("T.a", 255), ("T.b", 1), ("T.s", 0), ("T.dut.a", 255), ("T.dut.b", 1), ("T.dut.s", 0)]);
    check(&d, &[("T.a", 10), ("T.b", 20)], &[("T.a", 10), ("T.b", 20), ("T.s", 30), ("T.dut.a", 10), ("T.dut.b", 20), ("T.dut.s", 30)]);
    check(&d, &[("T.a", 128), ("T.b", 128)], &[("T.a", 128), ("T.b", 128), ("T.s", 0), ("T.dut.a", 128), ("T.dut.b", 128), ("T.dut.s", 0)]);
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
           let dut: A = { .a = a, .b = b, .s = s };\n\
         }\n",
    );
    assert_eq!(d.signals[id(&d, "T.dut.s").0 as usize].width, 8, "Byte : uint[8] must be width 8");
    check(&d, &[("T.a", 200), ("T.b", 100)], &[("T.a", 200), ("T.b", 100), ("T.s", 44), ("T.dut.a", 200), ("T.dut.b", 100), ("T.dut.s", 44)]);
    check(&d, &[("T.a", 255), ("T.b", 1)], &[("T.a", 255), ("T.b", 1), ("T.s", 0), ("T.dut.a", 255), ("T.dut.b", 1), ("T.dut.s", 0)]);
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
           let dut: D = { .px = px, .py = py, .lim = lim, .s = s, .bg = bg };\n\
         }\n",
    );
    check(&d, &[("T.px", 10), ("T.py", 20), ("T.lim", 15)], &[("T.px", 10), ("T.py", 20), ("T.lim", 15), ("T.s", 30), ("T.bg", 15), ("T.dut.px", 10), ("T.dut.py", 20), ("T.dut.lim", 15), ("T.dut.s", 30), ("T.dut.bg", 15), ("T.dut.p.x", 10), ("T.dut.p.y", 20)]);
    check(&d, &[("T.px", 200), ("T.py", 100), ("T.lim", 50)], &[("T.px", 200), ("T.py", 100), ("T.lim", 50), ("T.s", 44), ("T.bg", 200), ("T.dut.px", 200), ("T.dut.py", 100), ("T.dut.lim", 50), ("T.dut.s", 44), ("T.dut.bg", 200), ("T.dut.p.x", 200), ("T.dut.p.y", 100)]);
    check(&d, &[("T.px", 5), ("T.py", 5), ("T.lim", 250)], &[("T.px", 5), ("T.py", 5), ("T.lim", 250), ("T.s", 10), ("T.bg", 250), ("T.dut.px", 5), ("T.dut.py", 5), ("T.dut.lim", 250), ("T.dut.s", 10), ("T.dut.bg", 250), ("T.dut.p.x", 5), ("T.dut.p.y", 5)]);
    check(&d, &[("T.px", 0), ("T.py", 0), ("T.lim", 0)], &[("T.px", 0), ("T.py", 0), ("T.lim", 0), ("T.s", 0), ("T.bg", 0), ("T.dut.px", 0), ("T.dut.py", 0), ("T.dut.lim", 0), ("T.dut.s", 0), ("T.dut.bg", 0), ("T.dut.p.x", 0), ("T.dut.p.y", 0)]);
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
           let dut: D = { .go = go, .x = x, .ov = ov, .od = od };\n\
         }\n",
    );
    check(&d, &[("T.go", 1), ("T.x", 42)], &[("T.go", 1), ("T.x", 42), ("T.ov", 1), ("T.od", 42), ("T.dut.go", 1), ("T.dut.x", 42), ("T.dut.ov", 1), ("T.dut.od", 42), ("T.dut.s.valid", 1), ("T.dut.s.data", 42)]);
    check(&d, &[("T.go", 0), ("T.x", 42)], &[("T.go", 0), ("T.x", 42), ("T.ov", 0), ("T.od", 0), ("T.dut.go", 0), ("T.dut.x", 42), ("T.dut.ov", 0), ("T.dut.od", 0), ("T.dut.s.valid", 0), ("T.dut.s.data", 0)]);
    check(&d, &[("T.go", 1), ("T.x", 255)], &[("T.go", 1), ("T.x", 255), ("T.ov", 1), ("T.od", 255), ("T.dut.go", 1), ("T.dut.x", 255), ("T.dut.ov", 1), ("T.dut.od", 255), ("T.dut.s.valid", 1), ("T.dut.s.data", 255)]);
    check(&d, &[("T.go", 0), ("T.x", 0)], &[("T.go", 0), ("T.x", 0), ("T.ov", 0), ("T.od", 0), ("T.dut.go", 0), ("T.dut.x", 0), ("T.dut.ov", 0), ("T.dut.od", 0), ("T.dut.s.valid", 0), ("T.dut.s.data", 0)]);
    check(&d, &[("T.go", 1), ("T.x", 0)], &[("T.go", 1), ("T.x", 0), ("T.ov", 1), ("T.od", 0), ("T.dut.go", 1), ("T.dut.x", 0), ("T.dut.ov", 1), ("T.dut.od", 0), ("T.dut.s.valid", 1), ("T.dut.s.data", 0)]);
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
           let a: Pad = { .drive = da, .en = ea, .bus = wire, .shi = ha, .slo = la };\n\
           let b: Pad = { .drive = db, .en = eb, .bus = wire, .shi = hb, .slo = lb };\n\
         }\n\
         #[top]\n\
         entity T {}\n\
         impl T {\n\
           let da: Logic; let ea: Logic; let db: Logic; let eb: Logic;\n\
           let ha: Logic; let la: Logic; let hb: Logic; let lb: Logic;\n\
           let dut: Wired = { .da = da, .ea = ea, .db = db, .eb = eb, .ha = ha, .la = la, .hb = hb, .lb = lb };\n\
         }\n",
    );
    // Logic codes: '0'=0 '1'=1 'Z'=2 'X'=3.
    check(&d, &[("T.ea", 1), ("T.da", 1), ("T.eb", 2), ("T.db", 0)], &[("T.da", 1), ("T.ea", 1), ("T.db", 0), ("T.eb", 2), ("T.ha", 1), ("T.la", 1), ("T.hb", 1), ("T.lb", 1), ("T.dut.da", 1), ("T.dut.ea", 1), ("T.dut.db", 0), ("T.dut.eb", 2), ("T.dut.ha", 1), ("T.dut.la", 1), ("T.dut.hb", 1), ("T.dut.lb", 1), ("T.dut.wire.hi", 1), ("T.dut.wire.lo", 1), ("T.dut.a.drive", 1), ("T.dut.a.en", 1), ("T.dut.a.bus.hi", 0), ("T.dut.a.bus.lo", 0), ("T.dut.a.shi", 1), ("T.dut.a.slo", 1), ("T.dut.b.drive", 0), ("T.dut.b.en", 2), ("T.dut.b.bus.hi", 0), ("T.dut.b.bus.lo", 0), ("T.dut.b.shi", 1), ("T.dut.b.slo", 1)]); // A drives 1, B tristate
    check(&d, &[("T.ea", 2), ("T.da", 0), ("T.eb", 1), ("T.db", 0)], &[("T.da", 0), ("T.ea", 2), ("T.db", 0), ("T.eb", 1), ("T.ha", 0), ("T.la", 0), ("T.hb", 0), ("T.lb", 0), ("T.dut.da", 0), ("T.dut.ea", 2), ("T.dut.db", 0), ("T.dut.eb", 1), ("T.dut.ha", 0), ("T.dut.la", 0), ("T.dut.hb", 0), ("T.dut.lb", 0), ("T.dut.wire.hi", 0), ("T.dut.wire.lo", 0), ("T.dut.a.drive", 0), ("T.dut.a.en", 2), ("T.dut.a.bus.hi", 0), ("T.dut.a.bus.lo", 0), ("T.dut.a.shi", 0), ("T.dut.a.slo", 0), ("T.dut.b.drive", 0), ("T.dut.b.en", 1), ("T.dut.b.bus.hi", 0), ("T.dut.b.bus.lo", 0), ("T.dut.b.shi", 0), ("T.dut.b.slo", 0)]);
    check(&d, &[("T.ea", 1), ("T.da", 1), ("T.eb", 1), ("T.db", 0)], &[("T.da", 1), ("T.ea", 1), ("T.db", 0), ("T.eb", 1), ("T.ha", 3), ("T.la", 3), ("T.hb", 3), ("T.lb", 3), ("T.dut.da", 1), ("T.dut.ea", 1), ("T.dut.db", 0), ("T.dut.eb", 1), ("T.dut.ha", 3), ("T.dut.la", 3), ("T.dut.hb", 3), ("T.dut.lb", 3), ("T.dut.wire.hi", 3), ("T.dut.wire.lo", 3), ("T.dut.a.drive", 1), ("T.dut.a.en", 1), ("T.dut.a.bus.hi", 0), ("T.dut.a.bus.lo", 0), ("T.dut.a.shi", 3), ("T.dut.a.slo", 3), ("T.dut.b.drive", 0), ("T.dut.b.en", 1), ("T.dut.b.bus.hi", 0), ("T.dut.b.bus.lo", 0), ("T.dut.b.shi", 3), ("T.dut.b.slo", 3)]);
    check(&d, &[("T.ea", 1), ("T.da", 1), ("T.eb", 1), ("T.db", 1)], &[("T.da", 1), ("T.ea", 1), ("T.db", 1), ("T.eb", 1), ("T.ha", 1), ("T.la", 1), ("T.hb", 1), ("T.lb", 1), ("T.dut.da", 1), ("T.dut.ea", 1), ("T.dut.db", 1), ("T.dut.eb", 1), ("T.dut.ha", 1), ("T.dut.la", 1), ("T.dut.hb", 1), ("T.dut.lb", 1), ("T.dut.wire.hi", 1), ("T.dut.wire.lo", 1), ("T.dut.a.drive", 1), ("T.dut.a.en", 1), ("T.dut.a.bus.hi", 0), ("T.dut.a.bus.lo", 0), ("T.dut.a.shi", 1), ("T.dut.a.slo", 1), ("T.dut.b.drive", 1), ("T.dut.b.en", 1), ("T.dut.b.bus.hi", 0), ("T.dut.b.bus.lo", 0), ("T.dut.b.shi", 1), ("T.dut.b.slo", 1)]);
    check(&d, &[("T.ea", 2), ("T.da", 0), ("T.eb", 2), ("T.db", 0)], &[("T.da", 0), ("T.ea", 2), ("T.db", 0), ("T.eb", 2), ("T.ha", 2), ("T.la", 2), ("T.hb", 2), ("T.lb", 2), ("T.dut.da", 0), ("T.dut.ea", 2), ("T.dut.db", 0), ("T.dut.eb", 2), ("T.dut.ha", 2), ("T.dut.la", 2), ("T.dut.hb", 2), ("T.dut.lb", 2), ("T.dut.wire.hi", 2), ("T.dut.wire.lo", 2), ("T.dut.a.drive", 0), ("T.dut.a.en", 2), ("T.dut.a.bus.hi", 0), ("T.dut.a.bus.lo", 0), ("T.dut.a.shi", 2), ("T.dut.a.slo", 2), ("T.dut.b.drive", 0), ("T.dut.b.en", 2), ("T.dut.b.bus.hi", 0), ("T.dut.b.bus.lo", 0), ("T.dut.b.shi", 2), ("T.dut.b.slo", 2)]);
}
