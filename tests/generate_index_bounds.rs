//! An index that only becomes constant at elaboration is bounds-checked too.
//!
//! `types` reports an out-of-range index (`E-P003`), but only when the index is
//! a literal in the source. An index that becomes constant *later* — a generate
//! loop's unrolled variable, or an entity parameter substituted at elaboration
//! — reached lowering unchecked, and lowering had no complaint to make: a write
//! to an element that does not exist found no signal and was dropped, a read of
//! one clamped to the last element. `v[4] = 1` was a hard error while
//! `for i in 0..4 { v[i] = 1 }` over a 4-element `v` built four assignments and
//! discarded the fifth in silence.
//!
//! The shape that motivated it: ranges are directional, so `2..1` counts down,
//! and a parameterized `for i in 0..(N - 1)` with `N = 0` iterates `0, -1` and
//! drove element -1. That one needed a second fix — `subst_expr` substituted
//! the index as `Expr::Int { text: "-1" }`, which the lexer never produces and
//! `parse_int` cannot read, so the negative iteration folded to nothing at all.
//!
//! The two negative controls matter as much as the positives: a runtime index
//! (`regs[addr]`) has no constant to check and must stay legal, and an in-range
//! generate loop must not be flagged.

use std::process::Command;

/// Compile `src` as far as metadata (no toolchain needed) and return the
/// diagnostics.
fn diagnostics(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_genidx_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(["--emit", "metadata"])
        .arg(&file)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr)
}

/// A four-element array with `body` as the entity's statements.
fn design(body: &str) -> String {
    format!(
        "module m;\n\
         using std::bits::{{unsigned}};\n\
         entity E {{ y: unsigned[16] out }}\n\
         impl E {{\n{body}\n}}\n"
    )
}

#[test]
fn a_generate_loop_that_runs_past_the_end_is_reported() {
    // Written past the end: the fifth assignment used to be dropped.
    let out = diagnostics(
        "write",
        &design("    let v: unsigned[16][4];\n    for k in 0..4 { v[k] = 1; }\n    y = v[0];"),
    );
    assert!(
        out.contains("element 4 is outside `0..3` of this 4-element array"),
        "expected an out-of-range report, got:\n{out}"
    );
    assert!(
        out.contains("E-P003"),
        "should reuse the literal case's code"
    );
}

#[test]
fn a_read_past_the_end_is_reported_not_clamped() {
    // Read past the end: `v[4]` used to come back as `v[3]`, so the design ran
    // and produced a plausible wrong number.
    let out = diagnostics(
        "read",
        &design(
            "    let v: unsigned[16][4];\n\
             \x20   let s: unsigned[16][6];\n\
             \x20   for k in 0..3 { v[k] = 2; }\n\
             \x20   for k in 0..5 { s[k] = v[k]; }\n\
             \x20   y = s[0];",
        ),
    );
    assert!(
        out.contains("element 4 is outside `0..3` of this 4-element array"),
        "a read past the end should be reported, got:\n{out}"
    );
}

#[test]
fn a_parameter_substituted_index_is_reported() {
    // Not a loop at all: the index is a parameter, constant only after
    // elaboration. It used to clamp to the last element.
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               entity E<N: integer> { y: unsigned[16] out }\n\
               impl<N: integer> E<N> {\n\
                   let v: unsigned[16][4];\n\
                   for k in 0..3 { v[k] = 3; }\n\
                   y = v[N];\n\
               }\n\
               #[top] entity Top { y: unsigned[16] out }\n\
               impl Top { let e: E<N = 9> = {}; y = e.y; }\n";
    let out = diagnostics("param", src);
    assert!(
        out.contains("element 9 is outside `0..3` of this 4-element array"),
        "a parameter index should be checked, got:\n{out}"
    );
}

#[test]
fn a_descending_range_into_a_negative_index_is_reported() {
    // `0..(N - 1)` with `N = 0` is `0..-1`, and a range counts down when its
    // bounds do. The -1 iteration folded to nothing until `subst_expr` learned
    // to emit a negation rather than an `Int` with a sign in its text.
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               entity E<N: integer> { y: unsigned[16] out }\n\
               impl<N: integer> E<N> {\n\
                   let v: unsigned[16][4];\n\
                   for k in 0..3 { v[k] = 0; }\n\
                   for i in 0..(N - 1) { v[i] = unsigned[16](i + 1); }\n\
                   y = v[0];\n\
               }\n\
               #[top] entity Top { y: unsigned[16] out }\n\
               impl Top { let e: E<N = 0> = {}; y = e.y; }\n";
    let out = diagnostics("negative", src);
    assert!(
        out.contains("element -1 is outside `0..3` of this 4-element array"),
        "a negative unrolled index should be reported, got:\n{out}"
    );
}

#[test]
fn a_bit_index_past_a_packed_vector_is_reported_as_a_bit() {
    // The same hole on a packed vector, and it has to be worded the way the
    // `types` check words it rather than calling a vector an array.
    let out = diagnostics(
        "packed",
        &design(
            "    let x: unsigned[8] = 255;\n\
             \x20   let c: unsigned[16][12];\n\
             \x20   for k in 0..11 { c[k] = unsigned[16](x[k]); }\n\
             \x20   y = c[0];",
        ),
    );
    assert!(
        out.contains("bit 8 is outside `0..7` of this 8-bit vector"),
        "a bit index past a packed vector should be reported, got:\n{out}"
    );
}

#[test]
fn a_clocked_block_inside_a_generate_loop_is_checked_too() {
    // The loop has to be *inside* the clocked block, not around it. With the
    // `for` outermost the substituted body comes back through `lower_stmt` and
    // the first check sees it; nested inside, the sequential path unrolls the
    // loop itself and never returns to `lower_stmt`, so it needs its own
    // check. Writing the outer form here would leave that second hook dead and
    // the test would pass with it deleted.
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               using std::logic::{Bit};\n\
               entity E { clk: Bit in, y: unsigned[16] out }\n\
               impl E {\n\
                   let v: unsigned[16][4];\n\
                   if clk.rising() {\n\
                       for k in 0..5 { v[k] = v[k] + 1; }\n\
                   }\n\
                   y = v[0];\n\
               }\n";
    let out = diagnostics("clocked", src);
    assert!(
        out.contains("element 4 is outside `0..3` of this 4-element array"),
        "the clocked unroll path needs its own check, got:\n{out}"
    );
}

#[test]
fn an_instance_array_that_over_runs_is_named_as_instances() {
    // `let st: Inc[3]` with the loop running to 4 built and wired *five*
    // instances, and the chain through them computed correctly, so the
    // declared size was simply ignored. The wording has to match the literal
    // case, which calls them instances rather than array elements.
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               entity Inc { a: unsigned[16] in, y: unsigned[16] out }\n\
               impl Inc { y = a + 1; }\n\
               entity E { y: unsigned[16] out }\n\
               impl E {\n\
                   let st: Inc[3];\n\
                   let w: unsigned[16][8];\n\
                   w[0] = 10;\n\
                   for i in 0..4 { st[i] = Inc { .a = w[i], .y = w[i+1] }; }\n\
                   y = w[5];\n\
               }\n";
    let out = diagnostics("instarray", src);
    assert!(
        out.contains("instance 3 is outside `0..2` of this 3-instance array"),
        "an over-run instance array should be reported as instances, got:\n{out}"
    );
}

#[test]
fn reading_an_instance_array_slot_that_was_not_elaborated_is_reported() {
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               entity Inc { y: unsigned[8] out }\n\
               impl Inc { y = 1; }\n\
               entity Chain<N: integer> { y: unsigned[8] out }\n\
               impl<N: integer> Chain<N> {\n\
                   let stage: Inc[2];\n\
                   for i in 0..1 {\n\
                       if i < N { stage[i] = Inc {}; }\n\
                   }\n\
                   y = stage[1].y;\n\
               }\n\
               #[top] entity Top { y: unsigned[8] out }\n\
               impl Top { let chain: Chain<N = 1> = { .y = y }; }\n";
    let out = diagnostics("unbuilt_instance_slot", src);
    assert!(
        out.contains("instance `stage[1]` was not elaborated"),
        "an in-range but unbuilt instance slot should be named directly, got:\n{out}"
    );
    assert!(out.contains("E-P022"), "the diagnostic needs a stable code");
    assert!(
        out.contains("instance array `stage` declared here"),
        "the array declaration should be a related source location, got:\n{out}"
    );
    assert!(
        !out.contains("`stage[1].y` has no hardware form"),
        "the generic unsupported-expression diagnostic should be suppressed, got:\n{out}"
    );
}

#[test]
fn an_unreferenced_unbuilt_instance_array_slot_is_legal() {
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               entity Inc { y: unsigned[8] out }\n\
               impl Inc { y = 1; }\n\
               #[top] entity Top { y: unsigned[8] out }\n\
               impl Top {\n\
                   let stage: Inc[2];\n\
                   for i in 0..1 {\n\
                       if i == 0 { stage[i] = Inc {}; }\n\
                   }\n\
                   y = stage[0].y;\n\
               }\n";
    let out = diagnostics("unused_unbuilt_instance_slot", src);
    assert!(
        !out.contains("was not elaborated") && !out.contains("has no hardware form"),
        "an omitted slot is legal when it is not referenced, got:\n{out}"
    );
}

#[test]
fn driving_a_port_of_an_unbuilt_instance_array_slot_is_reported() {
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               entity Cell { a: unsigned[8] in, y: unsigned[8] out }\n\
               impl Cell { y = a; }\n\
               #[top] entity Top { a: unsigned[8] in, y: unsigned[8] out }\n\
               impl Top {\n\
                   let stage: Cell[2];\n\
                   if 1 == 1 { stage[0] = Cell {}; }\n\
                   stage[1].a = a;\n\
                   y = stage[0].y;\n\
               }\n";
    let out = diagnostics("drive_unbuilt_instance_slot", src);
    assert!(
        out.contains("instance `stage[1]` was not elaborated"),
        "driving a port of an absent child should name that child, got:\n{out}"
    );
    assert!(
        !out.contains("cannot be assigned to") && !out.contains("cannot assign to"),
        "the generic assignment-target diagnostic should be suppressed, got:\n{out}"
    );
}

#[test]
fn an_unbound_generic_instance_array_does_not_invent_a_missing_slot() {
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               entity Cell { y: unsigned[8] out }\n\
               impl Cell { y = 1; }\n\
               entity LibraryChain<N: integer> { y: unsigned[8] out }\n\
               impl<N: integer> LibraryChain<N> {\n\
                   let stage: Cell[N];\n\
                   for i in 0..(N - 1) { stage[i] = Cell {}; }\n\
                   y = stage[0].y;\n\
               }\n";
    let out = diagnostics("generic_instance_slot_unknown", src);
    assert!(
        !out.contains("was not elaborated") && !out.contains("has no hardware form"),
        "an uninstantiated generic has no concrete slot facts yet, got:\n{out}"
    );
}

#[test]
fn a_statically_dead_branch_is_not_bounds_checked() {
    // The ordinary shape for a generated chain: the first stage takes the
    // input and every later one takes its predecessor. At `i = 0` the `else`
    // reads `s[-1]`, but `i == 0` folds and that branch is never built, so
    // there is nothing to report.
    //
    // This passed by accident before negative folding was fixed — `s[i - 1]`
    // at `i = 0` produced an unreadable literal and the check skipped it for
    // the wrong reason. It has to hold for the right one now. The clocked
    // wrapper matters: without it the branch statements never re-enter
    // lowering on their own, which is where the context to skip them is lost.
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               using std::logic::{Bit};\n\
               entity A { clk: Bit in, d: unsigned[8] in, q: unsigned[8] out }\n\
               impl A {\n\
                   let s: unsigned[8][3];\n\
                   for i in 0..2 {\n\
                       if clk.rising() {\n\
                           if i == 0 { s[0] = d; } else { s[i] = s[i-1]; }\n\
                       }\n\
                   }\n\
                   q = s[2];\n\
               }\n";
    let out = diagnostics("deadbranch", src);
    assert!(
        !out.contains("E-P003"),
        "an untaken generate branch must not be checked, got:\n{out}"
    );
}

#[test]
fn a_folded_slice_bound_past_the_top_is_reported() {
    // `x[i + 8..i]` on an 8-bit `x` is a nine-bit slice. Unchecked, the bits
    // that do not exist read as 0 and the design produces a plausible number:
    // with `x = 255` the two iterations gave 255 and 127, summing to 382,
    // which is exactly what the misread computes and says nothing about being
    // wrong.
    let out = diagnostics(
        "slicetop",
        &design(
            "    let x: unsigned[8] = 255;\n\
             \x20   let v: unsigned[16][2];\n\
             \x20   for i in 0..1 { v[i] = unsigned[16](x[i+8..i]); }\n\
             \x20   y = v[0] + v[1];",
        ),
    );
    assert!(
        out.contains("bit 8 is outside `0..7` of this 8-bit vector"),
        "a folded slice bound past the top should be reported, got:\n{out}"
    );
}

#[test]
fn a_folded_negative_slice_bound_is_reported_with_its_source() {
    // Going the other way the bound was cast to `u32` and wrapped, surfacing
    // much later as the internal "slice bounds lo 4294967295 > hi 1" — no
    // code, no span, and a number that is not in the program. The literal
    // spelling `x[1..-1]` has always said "bit -1 is outside `0..7`", and the
    // folded one has to say the same thing.
    let out = diagnostics(
        "sliceneg",
        &design(
            "    let x: unsigned[8] = 255;\n\
             \x20   let v: unsigned[16][3];\n\
             \x20   for i in 0..2 { v[i] = unsigned[16](x[i+1..i-1]); }\n\
             \x20   y = v[0];",
        ),
    );
    assert!(
        out.contains("bit -1 is outside `0..7` of this 8-bit vector"),
        "a folded negative slice bound should name the bit, got:\n{out}"
    );
    assert!(
        !out.contains("4294967295"),
        "and must not leak a wrapped u32, got:\n{out}"
    );
}

#[test]
fn an_in_range_folded_slice_is_left_alone() {
    // Both controls: a moving in-range window, and a partial range whose
    // omitted bound comes from the vector and so cannot be wrong.
    let out = diagnostics(
        "sliceok",
        &design(
            "    let x: unsigned[8] = 255;\n\
             \x20   let v: unsigned[16][2];\n\
             \x20   let w: unsigned[16][2];\n\
             \x20   for i in 0..1 { v[i] = unsigned[16](x[i+3..i]); }\n\
             \x20   for i in 0..1 { w[i] = unsigned[16](x[..i]); }\n\
             \x20   y = v[0] + v[1] + w[0] + w[1];",
        ),
    );
    assert!(
        !out.contains("E-P003"),
        "an in-range slice must not be flagged, got:\n{out}"
    );
}

#[test]
fn a_runtime_index_is_not_bounds_checked() {
    // The negative control that matters most: `regs[addr]` has no constant to
    // check, and a check that fired here would break every memory in the
    // corpus.
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               entity Mem { addr: unsigned[2] in, y: unsigned[16] out }\n\
               impl Mem {\n\
                   let regs: unsigned[16][4];\n\
                   for k in 0..3 { regs[k] = unsigned[16](k * 11); }\n\
                   y = regs[addr];\n\
               }\n\
               #[top] entity Top { a: unsigned[2] in, y: unsigned[16] out }\n\
               impl Top { let m: Mem = { .addr = a, .y = y }; }\n";
    let out = diagnostics("runtime", src);
    assert!(
        !out.contains("E-P003"),
        "a runtime index must stay legal, got:\n{out}"
    );
}

#[test]
fn an_in_range_generate_loop_is_left_alone() {
    // Including a descending declared range, whose valid indices are 7..0 —
    // taking the length instead of the declared bounds would reject it.
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               using std::logic::{Bit};\n\
               entity E { y: unsigned[16] out }\n\
               impl E {\n\
                   let v: unsigned[16][4];\n\
                   let d: Bit[7..0];\n\
                   for k in 0..3 { v[k] = unsigned[16](k); }\n\
                   for k in 0..7 { d[k] = '1'; }\n\
                   y = v[3];\n\
               }\n";
    let out = diagnostics("inrange", src);
    assert!(
        !out.contains("E-P003"),
        "an in-range loop must not be flagged, got:\n{out}"
    );
}
