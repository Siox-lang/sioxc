//! Edge lowering and priority-condition tests.

use super::*;

#[test]
/// `clk.rising()` lowers to `Event`/`Old`/`Current` rather than a dedicated
/// edge node.
fn rising_lowers_to_event_old_current() {
    let d = lower_src(COUNTER);
    let rendered = d.to_ir_string();
    // clk.rising() expands into the explicit Event/Old/Current form. The
    // logic literals are resolved to their std positions ('0' -> 0,
    // '1' -> 1), so the IR carries plain constants, no raw chars.
    assert!(rendered.contains("Event(H.dut.clk)"));
    assert!(rendered.contains("Old(H.dut.clk) == 0"));
    assert!(rendered.contains("H.dut.clk == 1"));
    // The combinational driver and the next-state updates are present.
    assert!(rendered.contains("driver H.dut.count = H.dut.value"));
    assert!(rendered.contains("next H.dut.value = 0"));
}

#[test]
/// Priority conditions accumulate down a chain, so a later driver's guard
/// includes the negation of the earlier ones.
fn priority_conditions_accumulate() {
    let d = lower_src(COUNTER);
    let u = &d.event_blocks[0].updates;
    // First update guarded by rst == '1'.
    assert!(matches!(
        &u[0].cond,
        Some(Expr::Binary { op: BinOp::Eq, .. })
    ));
    // Second guarded by the negation AND en.
    assert!(matches!(
        &u[1].cond,
        Some(Expr::Binary { op: BinOp::And, .. })
    ));
}
