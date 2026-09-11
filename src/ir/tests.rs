//! Tests for frontend lowering and the public IR contract.

use super::*;
use crate::diag::FileId;

/// A minimal `ClockLike` impl so self-contained test sources can use the
/// `clk.rising()` edge methods (std provides these for real designs).
const CLK_PRELUDE: &str = "\n\
        enum Bool { false, true }\n\
        enum Bit { '0', '1' }\n\
        enum ULogic { 'U' = 4, 'X' = 3, '0' = 0, '1' = 1, 'Z' = 2, 'W' = 5, 'L' = 6, 'H' = 7, '-' = 8 }\n\
        enum Logic(ULogic);\n\
        impl LogicEncoding for Bit { fn to_bool(self) -> Bool { return self == '1'; } fn is_binary(self) -> Bool { return true; } fn is_high_impedance(self) -> Bool { return false; } fn to_x01(self) -> Bit { return self; } }\n\
        impl LogicEncoding for Logic { fn to_bool(self) -> Bool { return self == '1' or self == 'H'; } fn is_binary(self) -> Bool { return self == '0' or self == '1'; } fn is_high_impedance(self) -> Bool { return self == 'Z'; } fn to_x01(self) -> Logic { if self == '0' or self == 'L' { return '0'; } if self == '1' or self == 'H' { return '1'; } return 'X'; } }\n\
        impl Boolean for Bit { fn as_bool(self) -> Bool { return true; } }\n\
        impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } }\n\
        impl Operator<\"and\", Bool, Bool> for Bool { fn apply(self, rhs: Bool) -> Bool { return self; } }\n\
        impl Operator<\"or\", Bool, Bool> for Bool { fn apply(self, rhs: Bool) -> Bool { return self; } }\n\
        impl Operator<\"not\", Bool, Bool> for Bool { fn apply(self) -> Bool { return self; } }\n\
        trait ClockLike { fn rising(self) -> Bool; fn falling(self) -> Bool; fn edge(self) -> Bool; }\n\
        impl ClockLike for Bit { fn rising(self) -> Bool { return self'event and self'old == '0' and self == '1'; } fn falling(self) -> Bool { return self'event and self'old == '1' and self == '0'; } fn edge(self) -> Bool { return self'event; } }\n";

/// A match naming every variant of its scrutinee is as complete as one
/// ending in `_`, so a signal every arm assigns is not a latch. Only the
/// wildcard half was implemented, so the natural spelling of an exhaustive
/// decode drew an inferred-latch warning whose suggested fix is a
/// redundant `_` arm.
/// An initializer is a signal's power-on value, folded at elaboration. One
/// that reads another signal cannot fold, and was dropped in silence: the
/// signal kept its type's default, so `let i: unsigned[8] = a + 1;` read 0
/// while `i = a + 1;` — a driver, and a different thing — read 201.
#[test]
fn a_non_constant_initializer_is_reported() {
    let count = |src: &str| {
        lower_diags(src)
            .into_iter()
            .filter(|d| d.contains("is not a constant"))
            .count()
    };
    assert_eq!(
        count(
            "module m;\nentity E { y: unsigned[8] out }\n\
                 impl E { let a: unsigned[8] = 200; let i: unsigned[8] = a + 1; y = i; }\n"
        ),
        1,
        "an initializer reading another signal"
    );
    // Driving it is the spelling that means "compute this", and is fine.
    assert_eq!(
        count(
            "module m;\nentity E { y: unsigned[8] out }\n\
                 impl E { let a: unsigned[8] = 200; let i: unsigned[8]; i = a + 1; y = i; }\n"
        ),
        0,
        "the driver spelling is not an initializer"
    );
    // Everything that can fold still seeds without complaint: a literal, a
    // module constant, an arithmetic fold, and a const-evaluable call.
    assert_eq!(
        count(
            "module m;\nconst K: unsigned[8] = 5;\n\
                 fn twice(n: unsigned[8]) -> unsigned[8] { return n * 2; }\n\
                 entity E { y: unsigned[8] out }\n\
                 impl E { let a: unsigned[8] = 200; let b: unsigned[8] = K;\n\
                 let c: unsigned[8] = 3 * 7; let d: unsigned[8] = twice(4);\n\
                 y = a + b + c + d; }\n"
        ),
        0,
        "literals, constants, folds and const calls all seed"
    );

    // The aggregate sites seed inits the same way and dropped a
    // non-constant the same way — and there, no undriven lint reaches a
    // struct leaf or an array element, so nothing was reported at all.
    assert_eq!(
        count(
            "module m;\nstruct P { x: unsigned[8], y: unsigned[8] }\n\
                 entity E { src: unsigned[8] in, y: unsigned[8] out }\n\
                 impl E { let p: P = { .x = 7, .y = src + 1 }; y = p.y; }\n"
        ),
        1,
        "a struct-field initializer reading a signal"
    );
    assert_eq!(
        count(
            "module m;\nentity E { src: unsigned[8] in, y: unsigned[8] out }\n\
                 impl E { let arr: unsigned[8][2] = [9, src + 2]; y = arr[1]; }\n"
        ),
        1,
        "an array-element initializer reading a signal"
    );
    // Constant aggregates keep seeding.
    assert_eq!(
        count(
            "module m;\nconst K: unsigned[8] = 5;\n\
                 struct P { x: unsigned[8], y: unsigned[8] }\n\
                 entity E { y: unsigned[8] out }\n\
                 impl E { let p: P = { .x = K + 2, .y = 3 };\n\
                 let arr: unsigned[8][2] = [1, 3 * 4]; y = p.x + arr[1]; }\n"
        ),
        0,
        "a constant struct literal and array literal still seed"
    );
}

#[test]
/// An exhaustive match assigns on every path, so it must not be reported as
/// an inferred latch.
fn an_exhaustive_match_is_not_an_inferred_latch() {
    let latches = |src: &str| {
        lower_diags(src)
            .into_iter()
            .filter(|d| d.contains("inferred latch"))
            .count()
    };
    const ENUM: &str = "module m;\nenum State { Idle, Run }\n";

    // Every variant named, every arm assigning `a`.
    assert_eq!(
        latches(&format!(
            "{ENUM}entity E {{ s: State in, a: unsigned[8] out }}\n\
                 impl E {{ match s {{ State::Idle => a = 10, State::Run => a = 20, }} }}\n"
        )),
        0,
        "a match over every variant drives on every path"
    );

    // The same over a character-valued enum.
    assert_eq!(
        latches(
            "module m;\nentity E { b: Bit in, a: unsigned[8] out }\n\
                 impl E { match b { '0' => a = 10, '1' => a = 20, } }\n"
        ),
        0,
        "and over `Bit`, whose variants are character literals"
    );

    // A variant left out is a genuine latch.
    assert_eq!(
        latches(&format!(
            "{ENUM}entity E {{ s: State in, a: unsigned[8] out }}\n\
                 impl E {{ match s {{ State::Idle => a = 10, }} }}\n"
        )),
        1,
        "an unmatched variant still holds the previous value"
    );

    // Exhaustive, but one arm does not assign the signal.
    assert_eq!(
        latches(&format!(
            "{ENUM}entity E {{ s: State in, a: unsigned[8] out, k: unsigned[8] out }}\n\
                 impl E {{ a = 0; match s {{ State::Idle => k = 1, State::Run => a = 2, }} }}\n"
        )),
        1,
        "a signal only one arm assigns is a latch even when the match is complete"
    );
}

/// Lower `src` with the minimal library types the tests need.
fn lower_src(src: &str) -> Design {
    // unsigned/signed are library types (attribute-marked vectors), not seeded.
    let src = format!("{src}\nstruct unsigned(Logic[]);\nstruct signed(Logic[]);\n{CLK_PRELUDE}");
    let src = src.as_str();
    let mut sink = DiagnosticSink::new();
    let module = crate::syntax::parse_module(FileId(0), src, &mut sink);
    assert_eq!(sink.error_count(), 0, "parse errors:\n{src}");
    let modules = std::slice::from_ref(&module);
    let resolved = crate::resolve::resolve(modules, &mut sink);
    let typed = crate::types::check(modules, &resolved, &mut sink);
    let hier = crate::elab::elaborate(modules, &resolved, &typed, &mut sink);
    lower(modules, &resolved, &hier, &mut sink)
}

/// Lower `src` and return the diagnostics it produced.
fn lower_diagnostics(src: &str) -> Vec<crate::diag::Diagnostic> {
    let src = format!("{src}\nstruct unsigned(Logic[]);\nstruct signed(Logic[]);\n{CLK_PRELUDE}");
    let mut sink = DiagnosticSink::new();
    let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
    let modules = std::slice::from_ref(&module);
    let resolved = crate::resolve::resolve(modules, &mut sink);
    let typed = crate::types::check(modules, &resolved, &mut sink);
    let hier = crate::elab::elaborate(modules, &resolved, &typed, &mut sink);
    let _ = lower(modules, &resolved, &hier, &mut sink);
    sink.diagnostics().to_vec()
}

/// Lower `src` and return its diagnostic messages as strings.
fn lower_diags(src: &str) -> Vec<String> {
    lower_diagnostics(src)
        .iter()
        .map(|d| format!("{:?}: {}", d.code, d.message))
        .collect()
}
const COUNTER: &str = "module m;\n\
        entity Counter<W: integer> {\n\
          clk: Bit in,\n\
          rst: Logic in,\n\
          en: Bit in,\n\
          count: unsigned[W] out,\n\
        }\n\
        impl<W: integer> Counter<W> {\n\
          let value: unsigned[W] = 0;\n\
          process update {\n\
            if clk.rising() {\n\
              if rst == '1' {\n\
                value = 0;\n\
              } else if en {\n\
                value = value + 1;\n\
              }\n\
            }\n\
          }\n\
          count = value;\n\
        }\n\
        #[test]\n\
        entity H {}\n\
        impl H {\n\
          let clk: Bit = '0';\n\
          let rst: Logic = '1';\n\
          let en: Bit = '1';\n\
          let count: unsigned[8];\n\
          let dut: Counter<W = 8> = { .clk = clk, .rst = rst, .en = en, .count = count };\n\
        }\n";

mod behavior;
mod control;
mod diagnostics;
mod identity;
mod values;
