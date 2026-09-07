//! Nested metavalue operands must be hoisted into their own signals rather than
//! deep-copied once per element.
//!
//! `logical_meta` and `any_unknown` each read an operand once per element, so an
//! inlined operand was copied `width` times at every nesting level and the IR
//! grew as `width^depth`. `(a or b) - (a and b)` over 16 elements reached about
//! four million characters of dumped IR, and the nvc metavalue differential
//! sweep OOM-killed the compiler under a 14 GiB cap.
//!
//! This lives here rather than in `ir`'s unit tests because the per-element
//! logic tables come from elaborated `std` source, which the in-module lowering
//! helper does not load.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use siox::ir::{read_set, Design};

fn lower_nested(width: usize) -> Design {
    let pad = "0".repeat(width - 8);
    let source = format!(
        "module m;\n\
         using std::bits::unsigned;\n\
         entity E {{ y: unsigned[{width}] out, }}\n\
         impl E {{\n\
         let a: unsigned[{width}] = \"{pad}0000X100\";\n\
         let b: unsigned[{width}] = \"{pad}000U0101\";\n\
         y = (a or b) - (a and b);\n\
         }}\n"
    );
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/nested.siox", &source),
            Emit::Ir,
        ));
    assert!(
        compilation.succeeded(),
        "lowering {width}-element nested metavalues failed:\n{}",
        compilation.render_diagnostics()
    );
    compilation.design.expect("digital IR")
}

/// Every occurrence of a signal read, so a duplicated operand counts once per
/// copy — exactly what the hoisting is there to remove.
fn reads(design: &Design) -> usize {
    design
        .drivers
        .iter()
        .map(|driver| {
            let mut found = Vec::new();
            read_set(&driver.expr, &mut found);
            found.len()
        })
        .sum()
}

/// Asserted as a shape rather than a golden size, so it survives ordinary
/// changes to the lowering: doubling the element count must not much more than
/// double the IR. It was 4x (quadratic) before the operands were hoisted and is
/// 2x (linear) after, so the 3x bound is what fails if the hoisting stops.
#[test]
fn nested_metavalue_operands_are_hoisted_not_duplicated() {
    let narrow = lower_nested(8);
    let wide = lower_nested(16);

    assert!(
        !narrow.metavalue_temps.is_empty(),
        "the nested operands were left inline, so nothing was hoisted"
    );

    let (n, w) = (reads(&narrow), reads(&wide));
    assert!(
        w < n * 3,
        "doubling the element count more than tripled the IR ({n} -> {w} signal reads): \
         a nested metavalue operand is being copied per element again"
    );
}

#[test]
fn std_logic_tables_finish_as_interned_lookups() {
    let design = lower_nested(8);
    assert!(
        !design.lookup_tables.is_empty(),
        "std-derived logic tables were not interned in the finished design"
    );

    let ir = design.to_ir_string();
    assert!(
        ir.lines().any(|line| line.starts_with("lookup#")),
        "the normalized IR did not define its shared lookup tables:\n{ir}"
    );
    assert!(
        ir.lines()
            .any(|line| line.starts_with("driver ") && line.contains("lookup#")),
        "the normalized driver expressions did not reference a shared lookup:\n{ir}"
    );
}

/// The same bound must hold when several drivers fold through `Resolve`.
///
/// Resolution folding runs its own per-element unroll, and its helpers hold
/// only `&self`, so they could not hoist and each contribution's value and
/// discriminant plane was deep-copied once per element. Two parallel drivers on
/// a resolved vector therefore grew as `width^2` and exhausted a 6 GiB compiler
/// scope at eight elements, while one driver on the same expression stayed
/// linear.
fn lower_resolved(width: usize) -> Design {
    let pad = "0".repeat(width - 8);
    let source = format!(
        "module m;\n\
         using std::bits::unsigned;\n\
         entity E {{ y: unsigned[{width}] out }}\n\
         impl E {{\n\
         let a: unsigned[{width}] = \"{pad}0000X100\";\n\
         let b: unsigned[{width}] = \"{pad}000U0101\";\n\
         y = (a or b) - (a and b);\n\
         y = (a and b) - (a or b);\n\
         }}\n"
    );
    let compilation =
        Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
            SourceInput::memory("/virtual/resolved.siox", &source),
            Emit::Ir,
        ));
    assert!(
        compilation.succeeded(),
        "lowering {width}-element resolved drivers failed:\n{}",
        compilation.render_diagnostics()
    );
    compilation.design.expect("digital IR")
}

#[test]
fn resolved_multi_driver_contributions_are_hoisted_not_duplicated() {
    let narrow = lower_resolved(8);
    let wide = lower_resolved(16);

    assert!(
        !narrow.metavalue_temps.is_empty(),
        "the folded contributions were left inline, so nothing was hoisted"
    );

    let (n, w) = (reads(&narrow), reads(&wide));
    assert!(
        w < n * 3,
        "doubling the element count more than tripled the IR ({n} -> {w} signal reads): \
         a resolved driver contribution is being copied per element again"
    );
}
