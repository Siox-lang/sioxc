//! Aggregate, metavalue, numeric, local-storage, and layout tests.

use super::*;

/// `{ ..base, .z = 7 }` copied one value per *top-level* field, so a field
/// holding a struct read as a scalar (Unknown) and every leaf under it was
/// silently dropped — the spread carried over nothing nested.
/// A match naming every variant needs no `_`, but the Select chain still
/// bottomed out in `Unknown` — so the exhaustive spelling, the one the
/// non-exhaustive lint asks for, produced a design no engine would run.
#[test]
fn exhaustive_match_expression_needs_no_wildcard() {
    let d = lower_src(
        "module m;\n\
             enum Base { A, B, C }\n\
             entity E { sel: Base in, y: unsigned[8] out }\n\
             impl E { y = match sel { Base::A => 1, Base::B => 2, Base::C => 3 }; }\n\
             entity H {}\n\
             impl H { let sel: Base; let y: unsigned[8]; let e: E = { .sel = sel, .y = y }; }",
    );
    /// Whether an expression contains an `Unknown`, which validation rejects.
    fn has_unknown(e: &Expr) -> bool {
        match e {
            Expr::Unknown => true,
            Expr::Select { cond, then, els } => {
                has_unknown(cond) || has_unknown(then) || has_unknown(els)
            }
            Expr::Binary { lhs, rhs, .. } => has_unknown(lhs) || has_unknown(rhs),
            Expr::Unary { rhs, .. } => has_unknown(rhs),
            _ => false,
        }
    }
    let dr = d
        .drivers
        .iter()
        .find(|dr| d.signals[dr.target.0 as usize].path.ends_with(".y"))
        .expect("driver for y");
    assert!(!has_unknown(&dr.expr), "no Unknown left: {:?}", dr.expr);
}

/// `Counter<W = 8>` bound its parameter but `Counter<8>` silently did not:
/// only the named form was read, so the positional one left the parameter
/// unbound and every port kept width 0 — surfacing far downstream as
/// "signal has unknown width (0)" with nothing pointing at the instance.
/// A parameter carried its argument's *family* into the body but not its
/// width, so a nested inline — `signed`'s Ord inside `abs` — read
/// `self'length` as 1 and tested the sign bit with `>> 0`. `abs(-5)`
/// returned 251.
/// A call result is a value of its declared return type, so it wraps to
/// that width. Left unmasked, `neg(x)` on a `signed[8]`-returning function
/// carried a full-width `0 - x`, and a nested inline then tested the wrong
/// bit for the sign. Assigning to a signal masked it anyway, which hid it.
///
/// The operator dispatch that goes with this needs std's `<=>` impl, which
/// this harness's minimal prelude does not have; `fn_return_type_test` in
/// the corpus covers that end.
#[test]
fn a_call_result_carries_its_return_type() {
    let d = lower_src(
        "module m;\n\
             fn neg(v: signed[8]) -> signed[8] { return 0 - v; }\n\
             entity E { s: signed[8] in, lt: unsigned[8] out }\n\
             impl E { lt = if neg(s) < 0 { 1 } else { 0 }; }\n\
             entity H {}\n\
             impl H { let s: signed[8]; let lt: unsigned[8]; \
             let e: E = { .s = s, .lt = lt }; }",
    );
    let text = d.to_ir_string();
    assert!(
        text.contains("and 255"),
        "masked to the return width:\n{text}"
    );
}

#[test]
/// An inlined parameter keeps its argument's width rather than the
/// parameter's declared one.
fn an_inlined_parameter_keeps_its_argument_width() {
    let d = lower_src(
        "module m;\n\
             fn width_of(v: integer) -> integer { return v'length; }\n\
             entity E { s: signed[8] in, y: unsigned[8] out }\n\
             impl E { y = width_of(s); }\n\
             entity H {}\n\
             impl H { let s: signed[8]; let y: unsigned[8]; let e: E = { .s = s, .y = y }; }",
    );
    let dr = d
        .drivers
        .iter()
        .find(|dr| d.signals[dr.target.0 as usize].path.ends_with(".y"))
        .expect("driver for y");
    assert!(
        matches!(dr.expr, Expr::Const(8)),
        "the parameter should report the argument's width, got {:?}",
        dr.expr
    );
}

#[test]
/// A positional generic argument binds a value parameter.
fn positional_generic_argument_binds_a_value_parameter() {
    let src = |arg: &str| {
        format!(
            "module m;\n\
                 entity Inc<W: integer> {{ a: unsigned[W] in, y: unsigned[W] out, }}\n\
                 impl<W: integer> Inc<W> {{ y = a + 1; }}\n\
                 entity H {{}}\n\
                 impl H {{ let a: unsigned[4]; let y: unsigned[4]; \
                 let i: Inc<{arg}> = {{ .a = a, .y = y }}; }}"
        )
    };
    for arg in ["W = 4", "4"] {
        let d = lower_src(&src(arg));
        let w = d
            .signals
            .iter()
            .find(|s| s.path.ends_with("i.a"))
            .map(|s| s.width);
        assert_eq!(w, Some(4), "`Inc<{arg}>` should bind W");
    }
}

#[test]
/// A struct spread copies nested leaves, not just top-level fields.
fn spread_copies_nested_leaves() {
    let d = lower_src(
        "module m;\n\
             struct A { x: Bit, y: unsigned[4] }\n\
             struct B { a: A, z: unsigned[4] }\n\
             entity E { oy: unsigned[4] out }\n\
             impl E {\n\
               let base: B;\n\
               base = B { .a = A { .x = '1', .y = 9 }, .z = 2 };\n\
               let upd: B;\n\
               upd = B { ..base, .z = 7 };\n\
               oy = upd.a.y;\n\
             }\n\
             entity H {}\n\
             impl H { let oy: unsigned[4]; let e: E = { .oy = oy }; }",
    );
    let driven = |suffix: &str| {
        d.signals
            .iter()
            .position(|s| s.path.ends_with(suffix))
            .and_then(|i| d.drivers.iter().find(|dr| dr.target.0 as usize == i))
            .is_some()
    };
    assert!(driven(".upd.a.y"), "the spread must carry nested leaves");
    assert!(driven(".upd.a.x"), "every leaf, not just the read one");
    assert!(driven(".upd.z"), "and the explicit override");
}

#[test]
/// A composed struct flattens its nested fields.
fn composed_struct_flattens_nested_fields() {
    // Composition replaced extension (§3.28): a struct field holding
    // another struct flattens to dotted leaf signals.
    let d = lower_src(
        "module m;\n\
             struct Header { valid: Bit, kind: unsigned[4] }\n\
             struct Packet { header: Header, data: unsigned[8] }\n\
             entity E { p: Packet out }\n\
             impl E {}\n\
             entity H {}\n\
             impl H { let p: Packet; let dut: E = { .p = p }; }\n",
    );
    let width = |path: &str| d.signals.iter().find(|x| x.path == path).map(|x| x.width);
    assert_eq!(width("H.dut.p.header.valid"), Some(1), "nested field");
    assert_eq!(width("H.dut.p.header.kind"), Some(4), "nested field");
    assert_eq!(width("H.dut.p.data"), Some(8), "own field");
}

#[test]
/// A derivation that adds no variants is representation-identical to its
/// base.
fn same_variant_enum_derivation_is_representation_identical() {
    // A bodyless derivation keeps the base's width and discriminants.
    let d = lower_src(
        "module m;\n\
             enum Base { A, B, C }\n\
             enum Alias(Base);\n\
             entity E { x: Alias out }\n\
             impl E { x = Alias::B; }\n\
             entity H {}\n\
             impl H { let x: Alias; let dut: E = { .x = x }; }\n",
    );
    let sig = d.signals.iter().find(|s| s.path == "H.dut.x").unwrap();
    assert_eq!(sig.width, 2, "3 variants -> 2 bits, same as base");
}

#[test]
/// A bit string decodes across the full nine-value set.
fn bit_string_decodes_nine_value() {
    // A plain 2-value string is unchanged; a metavalue digit takes its
    // source-defined `LogicEncoding::to_bool` bit rather than a bit of the
    // enum discriminant. X normalizes to a low placeholder in the value
    // plane while the companion retains its exact identity.
    let d = lower_src(
        "module m; entity E { y: unsigned[4] out, z: unsigned[4] out, }\n\
             impl E { y = \"1010\"; z = \"1X10\"; }\n\
             entity T {}\n\
             impl T { let y: unsigned[4]; let z: unsigned[4]; let dut: E = { .y = y, .z = z }; }",
    );
    let s = d.to_ir_string();
    assert!(s.contains("driver T.dut.y = 10"), "2-value unchanged:\n{s}");
    assert!(
        s.contains("driver T.dut.z = 10"),
        "metavalue digit decodes:\n{s}"
    );
}

#[test]
/// A bit-string initializer sets the signal's init pattern.
fn bit_string_initializer_sets_init() {
    // `let v: unsigned[4] = "1010"` seeds the signal init to 10 (was 0 — no
    // string-init arm in const_init_value).
    let d = lower_src(
        "module m; entity E { y: unsigned[4] out, }\n\
             impl E { let v: unsigned[4] = \"1010\"; y = v; }\n\
             entity T {}\n\
             impl T { let y: unsigned[4]; let dut: E = { .y = y }; }",
    );
    let v = d
        .signals
        .iter()
        .find(|s| s.path.ends_with(".v"))
        .expect("no .v");
    assert_eq!(v.init, vec![10], "b\"1010\" -> init 10");
}

#[test]
/// A bit string containing a metavalue creates the companion plane.
fn metavalue_bit_string_creates_companion() {
    // A metavalue init spawns a `$meta` companion recording the X element;
    // a plain 2-value init does not.
    let d = lower_src(
            "module m; entity E { y: unsigned[4] out, z: unsigned[4] out, }\n\
             impl E { let v: unsigned[4] = \"1X10\"; let w: unsigned[4] = \"1010\"; y = v; z = w; }\n\
             entity T {}\n\
             impl T { let y: unsigned[4]; let z: unsigned[4]; let dut: E = { .y = y, .z = z }; }",
        );
    let v = d
        .signals
        .iter()
        .position(|s| s.path.ends_with(".v"))
        .expect("v") as u32;
    let w = d
        .signals
        .iter()
        .position(|s| s.path.ends_with(".w"))
        .expect("w") as u32;
    let cid = *d.meta_of.get(&v).expect("v has a metavalue companion");
    // "1X10": per-element discs, nibble i = element i. pos3=1, pos2=X(3),
    // pos1=1, pos0=0 -> 0x1310. Companion is 4 bits/element wide.
    assert_eq!(
        d.signals[cid as usize].init,
        vec![0x1310],
        "full per-element discs"
    );
    assert_eq!(d.signals[cid as usize].width, 16, "4 bits x 4 elements");
    assert!(d.signals[cid as usize].path.ends_with(".v$meta"));
    assert!(!d.meta_of.contains_key(&w), "clean init gets no companion");
}

#[test]
/// A resolved metavalue companion is terminal: companions never gain
/// companions of their own, which is what bounded the `$meta$meta` chain.
fn resolved_metavalue_companion_is_terminal() {
    // Element-wise resolution builds the discriminant plane from both the
    // value and metavalue planes.  That expression must not make the
    // propagation fixed point infer a companion for the companion itself.
    let d = lower_src(
        "module m;\n\
             impl<T: Resolve> Resolve for T[] {\n\
                 fn resolve(self, rhs: T[]) -> T[] { return self; }\n\
             }\n\
             impl Resolve for Logic {\n\
                 fn resolve(self, rhs: Logic) -> Logic {\n\
                     if self == 'Z' { return rhs; }\n\
                     return self;\n\
                 }\n\
             }\n\
             entity Driver { y: unsigned[2] out }\n\
             impl Driver { y = \"X0\"; }\n\
             impl Driver { y = \"01\"; }\n\
             entity T {}\n\
             impl T { let y: unsigned[2]; let d: Driver = { .y = y }; }",
    );

    assert!(
        d.signals
            .iter()
            .any(|signal| signal.path.ends_with("$meta")),
        "the resolved vector still needs one discriminant plane"
    );
    assert!(
        d.signals
            .iter()
            .all(|signal| !signal.path.contains("$meta$meta")),
        "a discriminant plane must never acquire its own companion"
    );
    assert!(
        d.meta_of
            .values()
            .all(|companion| !d.meta_of.contains_key(companion)),
        "the companion relation must have depth one"
    );
}

#[test]
/// Wide metavalue initializers have no element ceiling.
fn wide_metavalue_initializers_have_no_element_limit() {
    let d = lower_src(
        "module m;\n\
             entity T {}\n\
             impl T { let v: unsigned[17] = \"X0000000000000000\"; }\n",
    );
    let v = d
        .signals
        .iter()
        .position(|s| s.path.ends_with(".v"))
        .expect("v signal");
    let cid = *d.meta_of.get(&(v as u32)).expect("wide companion");
    assert_eq!(d.signals[cid as usize].width, 68);
    assert_eq!(
        d.signals[cid as usize].init,
        vec![0, 3],
        "the top element's discriminant crosses the first ABI word"
    );

    let driven = lower_src(
        "module m;\n\
             entity E { v: unsigned[17] out, }\n\
             impl E { v = \"X0000000000000000\"; }\n",
    );
    let cid = *driven.meta_of.values().next().expect("driven companion");
    assert!(driven.drivers.iter().any(|driver| {
        driver.target == SignalId(cid)
            && matches!(&driver.expr, Expr::WideConst(words) if words == &[0, 3])
    }));
}

#[test]
/// A later clean combinational write clears the companion in order, so a
/// stale `X` does not survive in the discriminant plane.
fn clean_combinational_override_clears_metavalue_companion_in_order() {
    let design = lower_src(
        "module m;\n\
             entity E { clear: Bit in, y: unsigned[4] out, }\n\
             impl E {\n\
                 let dirty: unsigned[4] = \"X000\";\n\
                 y = dirty;\n\
                 if clear { y = \"0000\"; }\n\
             }\n",
    );
    let y = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with(".y"))
        .expect("y") as u32;
    let companion = *design
        .meta_of
        .get(&y)
        .unwrap_or_else(|| panic!("y companion missing:\n{}", design.to_ir_string()));
    let value_drivers: Vec<_> = design
        .drivers
        .iter()
        .filter(|driver| driver.target == SignalId(y))
        .collect();
    let meta_drivers: Vec<_> = design
        .drivers
        .iter()
        .filter(|driver| driver.target == SignalId(companion))
        .collect();
    assert_eq!(value_drivers.len(), 2);
    assert_eq!(meta_drivers.len(), 2, "one companion write per value write");
    assert!(meta_drivers[0].cond.is_none());
    assert!(meta_drivers[1].cond.is_some());
    assert!(matches!(meta_drivers[1].expr, Expr::Const(0)));
}

#[test]
/// The same ordering holds for a clocked override.
fn clean_clocked_override_clears_metavalue_companion_in_order() {
    let design = lower_src(
        "module m;\n\
             entity E { clk: Bit in, clear: Bit in, y: unsigned[4] out, }\n\
             impl E {\n\
                 let dirty: unsigned[4] = \"X000\";\n\
                 if clk.rising() {\n\
                     y = dirty;\n\
                     if clear { y = \"0000\"; }\n\
                 }\n\
             }\n",
    );
    let y = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with(".y"))
        .expect("y") as u32;
    let companion = *design.meta_of.get(&y).expect("y companion");
    let block = design.event_blocks.first().expect("clocked block");
    let value_updates: Vec<_> = block
        .updates
        .iter()
        .filter(|update| update.target == SignalId(y))
        .collect();
    let meta_updates: Vec<_> = block
        .updates
        .iter()
        .filter(|update| update.target == SignalId(companion))
        .collect();
    assert_eq!(value_updates.len(), 2);
    assert_eq!(
        meta_updates.len(),
        2,
        "one companion update per value update"
    );
    assert!(meta_updates[0].cond.is_none());
    assert!(meta_updates[1].cond.is_some());
    assert!(matches!(meta_updates[1].expr, Expr::Const(0)));
}

#[test]
/// An `'old` vector read uses the companion's `'old` value, not its current
/// one.
fn old_vector_read_uses_old_metavalue_companion() {
    let design = lower_src(
        "module m;\n\
             entity E { y: unsigned[4] out, }\n\
             impl E {\n\
                 let v: unsigned[4] = \"X000\";\n\
                 y = v'old;\n\
             }\n",
    );
    let y = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with(".y"))
        .expect("y") as u32;
    let companion = *design.meta_of.get(&y).expect("y companion");
    let meta_driver = design
        .drivers
        .iter()
        .find(|driver| driver.target == SignalId(companion))
        .expect("companion driver");
    assert!(matches!(meta_driver.expr, Expr::Old(_)));
}

#[test]
/// Narrowed arithmetic still scans the full operand for metavalues, so an
/// unknown outside the narrowed range still poisons the result.
fn narrowed_arithmetic_scans_full_operand_for_metavalues() {
    let design = lower_src(
        "module m;\n\
             entity E { y: unsigned[4] out, }\n\
             impl E {\n\
                 let dirty: unsigned[8] = \"X0000000\";\n\
                 let zero: unsigned[8] = 0;\n\
                 y = (dirty + zero)[3..0];\n\
             }\n",
    );
    let y = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with(".y"))
        .expect("y") as u32;
    let companion = *design
        .meta_of
        .get(&y)
        .unwrap_or_else(|| panic!("y companion missing:\n{}", design.to_ir_string()));
    let rendered = render(
        &design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(companion))
            .expect("companion driver")
            .expr,
        &design,
    );
    assert!(
        rendered.contains("[31..28]"),
        "the poison predicate must inspect element 7 of the 8-element operand: {rendered}"
    );
}

/// Constant evaluation is best-effort and must never let host integer
/// overflow abort the compiler. The exact expression remains available to
/// arbitrary-width lowering even when it does not fit the narrow
/// elaboration evaluator.
#[test]
fn overflowing_narrow_constant_evaluation_does_not_panic() {
    let design = lower_src(
        "module m;\n\
             const FAR: integer = 1 << 64;\n\
             entity E { y: unsigned[128] out, }\n\
             impl E { y = FAR; }\n",
    );
    assert_eq!(design.drivers.len(), 1);
}

#[test]
/// Kernel-`integer` operations retain signed semantics through lowering.
fn kernel_integer_operations_retain_signed_semantics() {
    let design = lower_src(
        "module m;\n\
             entity E {\n\
                 a: integer<-16..15> in,\n\
                 b: integer<-16..15> in,\n\
                 lt: Bit out,\n\
                 q: integer<-16..15> out,\n\
                 shr: integer<-16..15> out,\n\
             }\n\
             impl E {\n\
                 lt = if a < b { '1' } else { '0' };\n\
                 q = a / b;\n\
                 shr = a >> 1;\n\
             }\n",
    );
    let driver = |suffix: &str| {
        let target = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(suffix))
            .expect("target signal");
        &design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(target as u32))
            .expect("target driver")
            .expr
    };
    assert!(matches!(
        driver(".lt"),
        Expr::Select { cond, .. }
            if matches!(cond.as_ref(), Expr::Binary { op: BinOp::SLt, .. })
    ));
    assert!(matches!(
        driver(".q"),
        Expr::Binary {
            op: BinOp::SDiv,
            ..
        }
    ));
    assert!(matches!(
        driver(".shr"),
        Expr::Binary {
            op: BinOp::AShr,
            ..
        }
    ));
}

#[test]
/// A real-to-integer conversion is signed in direct comparisons.
fn real_to_integer_conversion_is_signed_in_direct_comparisons() {
    let design = lower_src(
        "module m;\n\
             entity E {\n\
                 r: real in,\n\
                 lt: Bit out,\n\
             }\n\
             impl E {\n\
                 lt = if integer(r) < 0 { '1' } else { '0' };\n\
             }\n",
    );
    let lt = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with(".lt"))
        .expect("lt signal");
    let expr = &design
        .drivers
        .iter()
        .find(|driver| driver.target == SignalId(lt as u32))
        .expect("lt driver")
        .expr;
    assert!(matches!(
        expr,
        Expr::Select { cond, .. }
            if matches!(
                cond.as_ref(),
                Expr::Binary {
                    op: BinOp::SLt,
                    lhs,
                    ..
                } if matches!(lhs.as_ref(), Expr::Unary { op: UnOp::RealToInt, .. })
            )
    ));
}

#[test]
/// Assigning to a ranged integer may change the storage width.
fn ranged_integer_assignment_can_change_storage_width() {
    let src = "module m;\n\
             entity E {\n\
                 narrow: integer<-16..15> in,\n\
                 wide: integer<-128..127> out,\n\
             }\n\
             impl E { wide = narrow; }\n";
    let diagnostics = lower_diags(src);
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("width mismatch")),
        "{diagnostics:?}"
    );
    let design = lower_src(src);
    let wide = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with(".wide"))
        .expect("wide integer signal");
    assert!(matches!(
        design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(wide as u32))
            .map(|driver| &driver.expr),
        Some(Expr::Current(_))
    ));
}

#[test]
/// A chain of aliases retains the terminal signal representation.
fn chained_aliases_retain_terminal_signal_representation() {
    let design = lower_src(
        "module m;\n\
             using Small = integer<-16..15>;\n\
             using Alias = Small;\n\
             using Chars = Char[];\n\
             using Text = Chars;\n\
             entity E { x: Alias in, y: Alias out, }\n\
             impl E { let text: Text[3] = \"abc\"; y = x; }\n",
    );
    for suffix in [".x", ".y"] {
        let signal = design
            .signals
            .iter()
            .find(|signal| signal.path.ends_with(suffix))
            .expect("aliased signal");
        assert_eq!(signal.width, 5);
        assert!(signal.integer);
        assert_eq!(signal.range, Some((-16, 15)));
    }
    let text: Vec<_> = design
        .signals
        .iter()
        .filter(|signal| signal.path.contains(".text["))
        .collect();
    assert_eq!(text.len(), 3);
    assert!(text.iter().all(|signal| signal.char));
}

#[test]
/// Foreign integer calls retain their signed ABI types.
fn foreign_integer_calls_retain_signed_abi_types() {
    let design = lower_src(
        "module m;\n\
             using CWord = integer;\n\
             using CInteger = CWord;\n\
             extern \"C\" { pub fn labs(v: CInteger) -> CInteger; }\n\
             entity E {\n\
                 x: integer<-128..127> in,\n\
                 y: integer<-128..127> out,\n\
             }\n\
             impl E { y = labs(x); }\n",
    );
    assert!(matches!(
        design.drivers.first().map(|driver| &driver.expr),
        Some(Expr::CCall {
            integer_args,
            integer_ret: true,
            ..
        }) if integer_args == &[true]
    ));
}

#[test]
/// A hardware block local does not leak out of its block.
fn a_hardware_block_local_does_not_leak_out_of_its_block() {
    let diagnostics = lower_diags(
        "module m;\n\
             entity E { select: Bit in, y: unsigned[8] out }\n\
             impl E {\n\
                 if select == '1' { let temporary: unsigned[8] = 3; y = temporary; }\n\
                 y = temporary;\n\
             }\n",
    );
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("E-P001")
            && diagnostic.contains("no value named `temporary`")));
    assert!(diagnostics
        .iter()
        .all(|diagnostic| !diagnostic.contains("not lowered to hardware")));
}

#[test]
/// Hardware block locals allocate no signals and leave no `Unknown` in the
/// IR.
fn hardware_block_locals_do_not_allocate_signals_or_leave_unknown_ir() {
    let design = lower_src(
        "module m;\n\
             entity E { select: Bit in, a: unsigned[8] in, y: unsigned[8] out }\n\
             impl E {\n\
                 if select == '1' {\n\
                     let temporary: unsigned[8] = a;\n\
                     temporary = temporary + 1;\n\
                     y = temporary;\n\
                 } else { y = 0; }\n\
             }\n",
    );
    assert!(design
        .signals
        .iter()
        .all(|signal| !signal.path.ends_with(".temporary")));
    assert!(!design.to_ir_string().contains("Unknown"));
}

#[test]
/// Nested runtime access on a block local stays storage-free.
fn nested_runtime_access_on_a_block_local_stays_storage_free() {
    let source = "module m;\n\
             entity E {\n\
                 enable: Bit in, row: integer in, col: integer in,\n\
                 a: unsigned[8] in, y: unsigned[8] out\n\
             }\n\
             impl E {\n\
                 if enable == '1' {\n\
                     let matrix: unsigned[8][2][2] = [[1, 2], [3, 4]];\n\
                     matrix[row][col] = a;\n\
                     y = matrix[row][col];\n\
                 } else { y = 0; }\n\
             }\n";
    let diagnostics = lower_diags(source);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.contains("E-P017")),
        "{diagnostics:#?}"
    );
    let design = lower_src(source);
    assert!(design
        .signals
        .iter()
        .all(|signal| !signal.path.contains("matrix")));
    assert!(!design.to_ir_string().contains("Unknown"));
    assert!(design.validate().is_empty(), "{:#?}", design.validate());
}

#[test]
/// A runtime packed index on a block local stays storage-free.
fn runtime_packed_index_on_a_block_local_stays_storage_free() {
    let source = "module m; using std::bits::unsigned; using std::logic::{Bit, Logic};
             entity E {
               enable: Bit in, index: unsigned[5] in, y: Logic out
             }
             impl E {
               if enable == '1' {
                 let word: unsigned[15..8] = 0;
                 word[index] = '1';
                 y = word[index];
               } else { y = '0'; }
             }";
    let diagnostics = lower_diags(source);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.contains("E-P017")),
        "{diagnostics:#?}"
    );
    let design = lower_src(source);
    assert!(design
        .signals
        .iter()
        .all(|signal| !signal.path.ends_with(".word")));
    assert!(!design.to_ir_string().contains("Unknown"));
    assert!(design.validate().is_empty(), "{:#?}", design.validate());
}

#[test]
/// Nested generic type arguments preserve the recursive layout.
fn nested_generic_type_arguments_preserve_recursive_layout() {
    let design = lower_src(
        "module m;\n\
             struct Box<T> { value: T }\n\
             struct Pair<T, U> { left: T, right: U }\n\
             entity Pass<T> { input: T in, output: T out }\n\
             impl<T> Pass<T> { output = input; }\n\
             entity E {\n\
                 nested: Box<Box<unsigned[8]>> in,\n\
                 named: Pair<U = Box<unsigned[16]>, T = Box<Box<unsigned[8]>>> in,\n\
                 y: unsigned[8] out,\n\
             }\n\
             impl E {\n\
                 let passed: Box<Box<unsigned[8]>>;\n\
                 let pass: Pass<Box<Box<unsigned[8]>>> = {\n\
                     .input = nested, .output = passed,\n\
                 };\n\
                 y = passed.value.value;\n\
             }\n",
    );
    let leaf = design
        .signals
        .iter()
        .find(|signal| signal.path.ends_with(".nested.value.value"))
        .expect("the nested generic field should flatten to one leaf");
    assert_eq!(leaf.width, 8);
    let named_left = design
        .signals
        .iter()
        .find(|signal| signal.path.ends_with(".named.left.value.value"))
        .expect("the named T argument should bind independently of order");
    let named_right = design
        .signals
        .iter()
        .find(|signal| signal.path.ends_with(".named.right.value"))
        .expect("the named U argument should bind independently of order");
    assert_eq!(named_left.width, 8);
    assert_eq!(named_right.width, 16);
    assert!(design.signals.iter().any(|signal| {
        signal.path.contains(".pass.")
            && signal.path.ends_with(".input.value.value")
            && signal.width == 8
    }));
    assert!(!design.to_ir_string().contains("Unknown"));
}

#[test]
/// The design persists recursive concrete source layouts for its values.
fn design_persists_recursive_concrete_source_layouts() {
    let design = lower_src(
        "module m;\n\
             struct Header { flag: Bit, code: unsigned[7..0] }\n\
             struct Packet<T> { header: Header, payload: T }\n\
             entity E {\n\
                 packets: Packet<unsigned[16]>[3..1] in,\n\
                 count: integer<-3..4> in,\n\
             }\n\
             impl E {}\n",
    );

    let packets = design
        .source_layouts
        .get("E.packets")
        .expect("the aggregate root keeps a layout despite having no signal");
    assert_eq!(
        packets.index_range(),
        Some(LayoutRange { left: 3, right: 1 })
    );
    assert_eq!(packets.bit_width(), Some(75));
    assert_eq!(packets.leaf_count(), Some(9));
    let LayoutKind::Array { element, .. } = &packets.kind else {
        panic!("packets should remain a source array: {packets:#?}");
    };
    let LayoutKind::Struct { name, fields, .. } = &element.kind else {
        panic!("the array element should retain Packet: {element:#?}");
    };
    assert_eq!(name, "Packet");
    assert_eq!(
        fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["header", "payload"]
    );
    assert!(matches!(
        fields[1].layout.kind,
        LayoutKind::Packed {
            width: 16,
            range: Some(LayoutRange { left: 0, right: 15 }),
            ..
        }
    ));

    let code = design
        .source_layouts
        .get("E.packets[3].header.code")
        .expect("every flattened leaf also has its own concrete layout");
    assert!(matches!(
        code.kind,
        LayoutKind::Packed {
            width: 8,
            range: Some(LayoutRange { left: 7, right: 0 }),
            ..
        }
    ));

    let count = design
        .source_layouts
        .get("E.count")
        .expect("ranged integer");
    assert!(matches!(
        count.kind,
        LayoutKind::Scalar {
            width: 4,
            domain: ScalarDomain::Integer,
            value_range: Some((-3, 4)),
            ..
        }
    ));
}

#[test]
/// Testbench locals persist layouts without becoming hardware signals.
fn testbench_locals_persist_layouts_without_becoming_hardware_signals() {
    let design = lower_src(
        "module m;\n\
             struct Pair<T> { left: T, right: T }\n\
             #[test] entity T {}\n\
             impl T {\n\
                 let pairs: Pair<unsigned[8]>[2..1] = [\n\
                     { .left = 1, .right = 2 },\n\
                     { .left = 3, .right = 4 },\n\
                 ];\n\
             }\n",
    );

    let root = design
        .source_layouts
        .get("T.pairs")
        .expect("testbench local should retain its concrete layout");
    assert_eq!(root.bit_width(), Some(32));
    assert_eq!(root.leaf_count(), Some(4));
    assert!(matches!(
        root.kind,
        LayoutKind::Array {
            range: Some(LayoutRange { left: 2, right: 1 }),
            ..
        }
    ));
    assert!(matches!(
        design
            .source_layouts
            .get("T.pairs[2].left")
            .map(|layout| &layout.kind),
        Some(LayoutKind::Packed { width: 8, .. })
    ));
    assert!(design
        .signals
        .iter()
        .all(|signal| !signal.path.contains("pairs")));
}
