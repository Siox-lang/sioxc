//! Lowering diagnostics, defaults, widths, views, and lint tests.

use super::*;

#[test]
/// Late IR lints point at the signal's declaration, which is the only source
/// location a synthesized driver has.
fn late_ir_lints_point_at_the_signal_declaration() {
    let source = "module m;\n\
            entity L { c: Logic in, looped: unsigned[8] out, latched: unsigned[8] out, forgotten: unsigned[8] out }\n\
            impl L {\n\
              let discarded: unsigned[8];\n\
              looped = looped;\n\
              if c == '1' { latched = 1; }\n\
              discarded = 2;\n\
            }\n\
            entity Top {}\n\
            impl Top {\n\
              let c: Logic = '0';\n\
              let looped: unsigned[8]; let latched: unsigned[8]; let forgotten: unsigned[8];\n\
              let dut: L = { .c = c, .looped = looped, .latched = latched, .forgotten = forgotten };\n\
            }\n";
    let diagnostics = lower_diagnostics(source);
    let cases = [
        (crate::diag::codes::COMBINATIONAL_LOOP, "looped", "looped:"),
        (crate::diag::codes::POSSIBLE_LATCH, "latched", "latched:"),
        (
            crate::diag::codes::UNDRIVEN_OUTPUT,
            "forgotten",
            "forgotten:",
        ),
        (
            crate::diag::codes::UNUSED_SIGNAL,
            "discarded",
            "let discarded:",
        ),
    ];
    for (code, signal, declaration) in cases {
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == Some(code) && diagnostic.message.contains(signal))
            .unwrap_or_else(|| panic!("missing {code} for {declaration}: {diagnostics:#?}"));
        let span = diagnostic
            .primary
            .unwrap_or_else(|| panic!("{code} has no primary span: {diagnostic:#?}"));
        let rendered = &source[span.start as usize..span.end as usize];
        assert!(
            rendered.contains(declaration),
            "{code} points at {rendered:?}, not declaration {declaration:?}"
        );
    }
}

#[test]
/// Late IR errors keep their stable codes and real source spans rather than
/// reporting against generated shapes.
fn late_ir_errors_keep_stable_codes_and_source_spans() {
    let cases = [
            (
                "module m;\nentity E {}\nimpl E { let data: unsigned[8][2] = read<unsigned[8]>(\"__siox_missing_span_fixture__.bin\"); }\n",
                crate::diag::codes::COMPILE_TIME_IO,
                "let data:",
            ),
            (
                "module m;\nfn recurse(v: unsigned[8]) -> unsigned[8] { return recurse(v); }\nentity E { a: unsigned[8] in, y: unsigned[8] out }\nimpl E { y = recurse(a); }\n",
                crate::diag::codes::UNBOUNDED_RECURSION,
                "recurse",
            ),
            (
                "module m;\nentity E { a: unsigned[8] in, y: unsigned[8] out }\nimpl E { y = a after 1; }\n",
                crate::diag::codes::TYPE_MISMATCH,
                "y = a after 1",
            ),
        ];
    for (source, code, source_text) in cases {
        let diagnostics = lower_diagnostics(source);
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == Some(code))
            .unwrap_or_else(|| panic!("missing {code}: {diagnostics:#?}"));
        let span = diagnostic
            .primary
            .unwrap_or_else(|| panic!("{code} has no primary span: {diagnostic:#?}"));
        let rendered = &source[span.start as usize..span.end as usize];
        assert!(
            rendered.contains(source_text),
            "{code} points at {rendered:?}, not {source_text:?}"
        );
    }
}

/// Generate loops are unrolled after type checking, so the source-level
/// dead-assignment lint could not see two iterations/loops resolve to the
/// same concrete target.
#[test]
fn generated_dead_assignments_warn_after_specialization() {
    let warning_count = |source: &str| {
        lower_diags(source)
            .iter()
            .filter(|diagnostic| diagnostic.starts_with("Some(\"W-P014\")"))
            .count()
    };
    let overlapping = "module m;
             entity E { y: unsigned[8] out, }
             impl E {
                 let values: unsigned[8][4];
                 process {
                     for i in 0..2 { values[i] = 11; }
                     for i in 2..3 { values[i] = 22; }
                 }
                 y = values[2];
             }";
    assert_eq!(
        warning_count(overlapping),
        1,
        "the generated writes to values[2] overlap"
    );

    let repeated_instance = "module m;
             entity E { y: unsigned[8] out, }
             impl E {
                 let values: unsigned[8][4];
                 process {
                     for i in 0..2 { values[i] = 11; }
                     for i in 2..3 { values[i] = 22; }
                 }
                 y = values[2];
             }
             entity H { a: unsigned[8] out, b: unsigned[8] out, }
             impl H {
                 let first: E = { .y = a };
                 let second: E = { .y = b };
             }";
    assert_eq!(
        warning_count(repeated_instance),
        1,
        "a source warning must not repeat for every instance"
    );

    let direct = "module m;
             entity E { y: unsigned[8] out, }
             impl E { process { y = 1; y = 2; } }";
    assert_eq!(
        warning_count(direct),
        1,
        "the IR lint must not duplicate the frontend warning"
    );

    let selected_block = "module m;
             entity E { y: unsigned[8] out, }
             impl E { process { if true { y = 1; y = 2; } } }";
    assert_eq!(
        warning_count(selected_block),
        1,
        "specializing a block must not repeat its frontend warning"
    );

    let disjoint = "module m;
             entity E { y: unsigned[8] out, }
             impl E {
                 let values: unsigned[8][4];
                 process {
                     for i in 0..1 { values[i] = 11; }
                     for i in 2..3 { values[i] = 22; }
                 }
                 y = values[2];
             }";
    assert_eq!(
        warning_count(disjoint),
        0,
        "disjoint generated targets are independent"
    );
}

#[test]
/// A multi-word integer literal keeps every word; there is no one-word
/// ceiling.
fn integer_literals_keep_all_words() {
    let d = lower_src(
        "module m;
             entity E { y: unsigned[192] out, }
             impl E { y = 1020847100762815390427017310442723737601; }",
    );
    assert!(matches!(
        &d.drivers[0].expr,
        Expr::WideConst(words) if words == &[1, 2, 3]
    ));
}

#[test]
/// Signals retain their kernel-`integer` identity, so signed operators and
/// formatting apply.
fn signals_retain_kernel_integer_identity() {
    let design = lower_src(
        "module m;
             entity E {
                 plain: integer out,
                 constrained: integer<-10..10> out,
                 bits: unsigned[8] out,
             }
             impl E { plain = -8; constrained = -3; bits = 255; }",
    );
    let signal = |suffix: &str| {
        design
            .signals
            .iter()
            .find(|signal| signal.path.ends_with(suffix))
            .unwrap_or_else(|| panic!("no signal {suffix}"))
    };
    assert!(signal(".plain").integer);
    assert!(signal(".constrained").integer);
    assert!(!signal(".bits").integer);
}

#[test]
/// A deep but acyclic derivation chain has no arbitrary depth limit.
fn deep_acyclic_type_derivation_has_no_magic_depth_limit() {
    let mut src = String::from("module m;\nstruct S0(Bit);\n");
    for i in 1..80 {
        src.push_str(&format!("struct S{i}(S{});\n", i - 1));
    }
    src.push_str(
        "entity E { y: S79 out, }
             impl E { y = S79(); }",
    );
    let d = lower_src(&src);
    assert_eq!(d.signals.iter().find(|s| s.path == "E.y").unwrap().width, 1);
}

/// A struct-literal initializer on an entity-level `let` silently powered
/// on at 0 — the testbench interpreter honoured it, hardware lowering did
/// not, so the two engines disagreed about the same declaration.
#[test]
fn struct_literal_initializer_seeds_field_inits() {
    let d = lower_src(
        "module m; struct P { a: unsigned[8], b: unsigned[8] }\n\
             entity E { x: unsigned[8] out, y: unsigned[8] out }\n\
             impl E { let p: P = { .a = 11, .b = 22 }; x = p.a; y = p.b; }\n\
             entity H { x: unsigned[8] out, y: unsigned[8] out, }\n\
             impl H { let d: E = { .x = x, .y = y }; }",
    );
    let init = |suffix: &str| {
        d.signals
            .iter()
            .find(|s| s.path.ends_with(suffix))
            .unwrap_or_else(|| panic!("no signal {suffix}"))
            .init
            .first()
            .copied()
            .unwrap_or(0)
    };
    assert_eq!(init(".p.a"), 11);
    assert_eq!(init(".p.b"), 22);
}

/// A concat target has an exact width, so the source must match it — the
/// lowering otherwise just sliced whatever it was given and zero-filled.
#[test]
fn concat_assignment_target_width_must_match() {
    let src = |rhs: &str| {
        format!(
                "module m;\nentity E {{ a: unsigned[8] in, y: unsigned[4] out, z: unsigned[4] out, }}\n\
                 impl E {{ {{y, z}} = {rhs}; }}\n\
                 entity H {{ a: unsigned[8] in, y: unsigned[4] out, z: unsigned[4] out, }}\n\
                 impl H {{ let d: E = {{ .a = a, .y = y, .z = z }}; }}\n"
            )
    };
    let mismatched = lower_diags(&src("a[3..0]"));
    assert!(
        mismatched
            .iter()
            .any(|d| d.contains("concatenation target is 8 bits")),
        "expected a width mismatch, got: {mismatched:?}"
    );
    let exact = lower_diags(&src("a"));
    assert!(
        !exact.iter().any(|d| d.contains("concatenation target")),
        "an exact-width source is fine: {exact:?}"
    );
}

/// Two producers wired to one bus net is the classic miswiring. It must
/// name the conflict (not the missing `Resolve` impl it happens to hit),
/// carry a code, and point at each contributing connection — this is the
/// only guard left now that views carry no coarse endpoint role.
#[test]
fn conflicting_drivers_name_the_conflict_and_its_sites() {
    let src = "module m;\n\
            struct Stream { valid: Bit, data: unsigned[8] }\n\
            view Source for Stream { valid out, data out }\n\
            entity Producer { bus: Stream Source, value: unsigned[8] in }\n\
            impl Producer { bus.valid = '1'; bus.data = value; }\n\
            entity BadLink { a: unsigned[8] in, b: unsigned[8] in }\n\
            impl BadLink {\n\
              let wire: Stream;\n\
              let p1: Producer = { .bus = wire, .value = a };\n\
              let p2: Producer = { .bus = wire, .value = b };\n\
            }\n";
    let mut sink = DiagnosticSink::new();
    let full = format!("{src}\nstruct unsigned(Logic[]);\nstruct signed(Logic[]);\n{CLK_PRELUDE}");
    let module = crate::syntax::parse_module(FileId(0), &full, &mut sink);
    let modules = std::slice::from_ref(&module);
    let resolved = crate::resolve::resolve(modules, &mut sink);
    let typed = crate::types::check(modules, &resolved, &mut sink);
    let hier = crate::elab::elaborate(modules, &resolved, &typed, &mut sink);
    let _ = lower(modules, &resolved, &hier, &mut sink);

    let conflicts: Vec<_> = sink
        .diagnostics()
        .iter()
        .filter(|d| d.code == Some(crate::diag::codes::CONFLICTING_DRIVERS))
        .collect();
    assert!(
        !conflicts.is_empty(),
        "expected a conflicting-drivers error"
    );
    for d in &conflicts {
        assert!(
            d.message.contains("conflicting sources"),
            "should name the conflict, got: {}",
            d.message
        );
        assert!(d.primary.is_some(), "should point at a connection site");
        assert!(!d.labels.is_empty(), "should label the other source(s)");
        assert!(d.help.is_some(), "should say how to fix it");
    }
}

#[test]
/// Parallel drivers on a type with a `Resolve` impl are legal and must not
/// warn.
fn resolved_parallel_drivers_are_legal_without_a_warning() {
    let diagnostics = lower_diags(
        "module m;\n\
             enum Wire { '0', '1' }\n\
             impl Resolve for Wire {\n\
                 fn resolve(self, rhs: Wire) -> Wire { return self; }\n\
             }\n\
             entity Net { a: Wire in, b: Wire in, y: Wire out }\n\
             impl Net { y = a; }\n\
             impl Net { y = b; }\n",
    );
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.contains("driver") || diagnostic.contains("W-P001") }),
        "a type-defined Resolve fold is intentional, not suspicious: {diagnostics:?}"
    );
}

#[test]
/// Bit patterns lower to the mask and match pair they imply.
fn bit_pattern_masks() {
    // Bare strings are per-bit with `-` as the don't-care.
    assert_eq!(
        crate::syntax::bit_pattern_mask("\"01--\""),
        Some((vec![0b1100], vec![0b0100]))
    );
    assert_eq!(
        crate::syntax::bit_pattern_mask("\"0000_11--\""),
        Some((vec![0b11111100], vec![0b00001100]))
    );
    // Radix prefixes mask a whole group with `?`.
    assert_eq!(
        crate::syntax::bit_pattern_mask("x\"A?\""),
        Some((vec![0xF0], vec![0xA0]))
    );
    assert_eq!(
        crate::syntax::bit_pattern_mask("x\"?3\""),
        Some((vec![0x0F], vec![0x03]))
    );
    assert_eq!(
        crate::syntax::bit_pattern_mask("o\"7?\""),
        Some((vec![0o70], vec![0o70]))
    );
    let wide = format!("\"1{}\"", "-".repeat(128));
    assert_eq!(
        crate::syntax::bit_pattern_mask(&wide),
        Some((vec![0, 0, 1], vec![0, 0, 1]))
    );
    assert_eq!(crate::syntax::bit_pattern_mask("\"2\""), None); // bad binary digit
}

#[test]
/// An applied view flattens the backing struct's fields.
fn applied_view_flattens_its_backing_struct_fields() {
    let d = lower_src(
        "module m;\n\
             struct HandshakeBus { valid: Bit, ready: Bit }\n\
             view Handshake for HandshakeBus { valid out, ready in }\n\
             impl HandshakeBus Handshake { fn assert_valid(self) { self.valid = '1'; } }\n\
             entity Producer { bus: HandshakeBus Handshake, observed: Bit out, }\n\
             impl Producer { bus.assert_valid(); observed = bus.ready; }",
    );
    assert!(d.signals.iter().any(|s| s.path.ends_with(".bus.valid")));
    assert!(d.signals.iter().any(|s| s.path.ends_with(".bus.ready")));
    assert!(matches!(
        d.source_layouts.get("Producer.bus").map(|layout| &layout.kind),
        Some(LayoutKind::Struct {
            name,
            view: Some(view),
            fields,
        }) if name == "HandshakeBus"
            && view == "Handshake@HandshakeBus"
            && fields.iter().map(|field| field.direction.clone()).collect::<Vec<_>>()
                == [Some(LayoutDirection::Out), Some(LayoutDirection::In)]
    ));
}

#[test]
/// A generic trait function reaches an applied view's backing fields.
fn generic_trait_functions_access_applied_view_backing_fields() {
    let d = lower_src(
        "module m;\n\
             trait Readable<T> { fn read(self) -> T; }\n\
             trait Writable<T> { fn write(self, value: T); }\n\
             fn read<Bus: Readable, Value>(bus: Bus) -> Value { return bus.read(); }\n\
             fn write<Bus: Writable, Value>(bus: Bus, value: Value) { bus.write(value); }\n\
             struct Spi { tx: unsigned[8], rx: unsigned[8] }\n\
             view Controller for Spi { tx out, rx in }\n\
             impl Readable<unsigned[8]> for Spi Controller {\n\
               fn read(self) -> unsigned[8] { return self.rx; }\n\
             }\n\
             impl Writable<unsigned[8]> for Spi Controller {\n\
               fn write(self, value: unsigned[8]) { self.tx = value; }\n\
             }\n\
             entity Device {\n\
               bus: Spi Controller, source: unsigned[8] in, sampled: unsigned[8] out,\n\
             }\n\
             impl Device { write(bus, source); sampled = read(bus); }\n\
             entity Link {}\n\
             impl Link {\n\
               let wire: Spi;\n\
               let source: unsigned[8];\n\
               let sampled: unsigned[8];\n\
               let device: Device = { .bus = wire, .source = source, .sampled = sampled };\n\
             }",
    );
    assert!(
        d.validate().is_empty(),
        "a view method must bind `self` to the backing struct fields"
    );
    let sampled = d
        .signals
        .iter()
        .position(|s| s.path.ends_with(".device.sampled"))
        .expect("sampled signal");
    let driver = d
        .drivers
        .iter()
        .find(|driver| driver.target.0 as usize == sampled)
        .expect("sampled driver");
    assert!(
        matches!(driver.expr, Expr::Current(_)),
        "read() should lower to the selected backing signal: {:?}",
        driver.expr
    );
    let tx = d
        .signals
        .iter()
        .position(|s| s.path.ends_with(".device.bus.tx"))
        .expect("view tx signal");
    assert!(
        d.drivers
            .iter()
            .any(|driver| driver.target.0 as usize == tx),
        "generic write() should inline its nested trait method as a driver"
    );
}

#[test]
/// An enum signal with no initializer takes its first variant, so it is
/// always initialized.
fn enum_signal_inits_to_first_variant() {
    // Derived `new()` default: an uninitialized enum signal powers on
    // holding its *first* variant. With a non-zero-based first
    // discriminant, that is a valid member — a bare `0` would not be.
    let d = lower_src(
        "module m;\n\
             enum Phase { Idle = 2, Run = 3, Done = 4 }\n\
             enum Step  { A, B, C }\n\
             entity T {}\n\
             impl T { let p: Phase; let s: Step; }\n",
    );
    let init = |suffix: &str| {
        d.signals
            .iter()
            .find(|s| s.path.ends_with(suffix))
            .unwrap_or_else(|| panic!("no {suffix}"))
            .init
            .first()
            .copied()
            .unwrap_or(0)
    };
    assert_eq!(init(".p"), 2, "Phase defaults to Idle = 2, not 0");
    assert_eq!(init(".s"), 0, "0-based Step still defaults to A = 0");
}

#[test]
/// An explicit initializer overrides the first-variant default.
fn explicit_enum_init_overrides_first_variant() {
    // An explicit `let p = Run` beats the first-variant default.
    let d = lower_src(
        "module m;\n\
             enum Phase { Idle = 2, Run = 3, Done = 4 }\n\
             entity T {}\n\
             impl T { let p: Phase = Phase::Run; }\n",
    );
    let p = d
        .signals
        .iter()
        .find(|s| s.path.ends_with(".p"))
        .expect("no .p");
    assert_eq!(p.init, vec![3], "explicit initializer Run = 3 wins");
}

#[test]
/// `T()` lowers to the type's structural default rather than a call.
fn nullary_constructor_lowers_to_default() {
    // `T()` in expression position is the type's derived default: an enum →
    // its first variant, a numeric/vector → 0. Same rule as the implicit
    // signal init, now writable.
    let d = lower_src(
        "module m;\n\
             enum Phase { Idle = 2, Run = 3 }\n\
             entity E { y: Phase out, z: unsigned[8] out }\n\
             impl E { y = Phase(); z = unsigned[8](); }\n",
    );
    let drv = |suffix: &str| -> u64 {
        let sig = d
            .signals
            .iter()
            .position(|s| s.path.ends_with(suffix))
            .expect("sig");
        let dr = d
            .drivers
            .iter()
            .find(|dr| dr.target.0 as usize == sig)
            .expect("driver");
        match &dr.expr {
            Expr::Const(c) => *c,
            other => panic!("{suffix} not a const: {other:?}"),
        }
    };
    assert_eq!(drv(".y"), 2, "Phase() == first variant Idle = 2");
    assert_eq!(drv(".z"), 0, "unsigned[8]() == 0");
}

#[test]
/// `T()` on a struct fills every field with its default.
fn nullary_constructor_defaults_struct_fields() {
    // `S()` on a struct defaults each field structurally: an enum field to
    // its first variant, a numeric field to 0 — through a composed struct
    // field as well as a direct one.
    let d = lower_src(
        "module m;\n\
             enum Phase { Idle = 2, Run = 3 }\n\
             struct Header { flag: Bit, ph: Phase }\n\
             struct Packet { header: Header, data: unsigned[8] }\n\
             entity E { o: Packet out }\n\
             impl E { o = Packet::new(); }\n",
    );
    let drv = |suffix: &str| -> u64 {
        let sig = d
            .signals
            .iter()
            .position(|s| s.path.ends_with(suffix))
            .expect("sig");
        let dr = d
            .drivers
            .iter()
            .find(|dr| dr.target.0 as usize == sig)
            .expect("driver");
        match &dr.expr {
            Expr::Const(c) => *c,
            other => panic!("{suffix} not a const: {other:?}"),
        }
    };
    assert_eq!(
        drv(".o.header.ph"),
        2,
        "enum field → first variant Idle = 2"
    );
    assert_eq!(drv(".o.header.flag"), 0, "composed Bit field → 0");
    assert_eq!(drv(".o.data"), 0, "numeric field → 0");
}

#[test]
/// The range attributes read the declared bounds, in the declared direction.
fn range_attributes_read_declared_bounds() {
    // A descending `[7..0]` and an ascending width-only `[8]` expose the
    // VHDL range attributes; direction is preserved.
    let d = lower_src(
        "module m;\n\
             entity E {\n\
               dn: unsigned[7..0] in, up: unsigned[8] in,\n\
               a: unsigned[8] out, b: unsigned[8] out, c: unsigned[8] out, e: unsigned[8] out,\n\
               f: unsigned[8] out, g: unsigned[8] out, h: unsigned[8] out,\n\
             }\n\
             impl E {\n\
               a = dn'left; b = dn'right; c = dn'high; e = dn'low;\n\
               f = dn'ascending; g = dn'length; h = up'ascending;\n\
             }\n",
    );
    let drv = |suffix: &str| -> u64 {
        let sig = d
            .signals
            .iter()
            .position(|s| s.path.ends_with(suffix))
            .expect("sig");
        let dr = d
            .drivers
            .iter()
            .find(|dr| dr.target.0 as usize == sig)
            .expect("driver");
        match &dr.expr {
            Expr::Const(c) => *c,
            other => panic!("{suffix} not const: {other:?}"),
        }
    };
    assert_eq!(drv(".a"), 7, "dn'left");
    assert_eq!(drv(".b"), 0, "dn'right");
    assert_eq!(drv(".c"), 7, "dn'high");
    assert_eq!(drv(".e"), 0, "dn'low");
    assert_eq!(drv(".f"), 0, "dn'ascending (descending → false)");
    assert_eq!(drv(".g"), 8, "dn'length");
    assert_eq!(drv(".h"), 1, "up'ascending (width-only → true)");
}

#[test]
/// An output port nothing drives warns (W-P011).
fn undriven_output_port_warns() {
    // `forgotten` is never assigned; `driven` is. Only the former warns.
    let diags = lower_diags(
        "module m;\n\
             entity E { a: unsigned[8] in, driven: unsigned[8] out, forgotten: unsigned[8] out }\n\
             impl E { driven = a + 1; }\n\
             entity T {}\n\
             impl T { let a: unsigned[8]; let d: unsigned[8]; let f: unsigned[8];\n\
               let dut: E = { .a = a, .driven = d, .forgotten = f }; }\n",
    );
    let undriven: Vec<&String> = diags.iter().filter(|d| d.contains("W-P011")).collect();
    assert_eq!(
        undriven.len(),
        1,
        "one undriven-output warning: {undriven:?}"
    );
    assert!(
        undriven[0].contains("forgotten"),
        "flags forgotten: {undriven:?}"
    );
}

#[test]
/// An internal signal nothing drives warns.
fn undriven_internal_signal_warns() {
    // `dead` (value-less, never assigned) warns; `used` is driven and
    // `konst` has an initializer, so neither does.
    let diags = lower_diags(
            "module m;\n\
             entity E { a: unsigned[8] in, y: unsigned[8] out }\n\
             impl E {\n  let used: unsigned[8];\n  let dead: unsigned[8];\n  let konst: unsigned[8] = 5;\n\
               used = a + 1;\n  y = used + konst;\n }\n\
             entity T {}\n\
             impl T { let a: unsigned[8]; let y: unsigned[8]; let dut: E = { .a = a, .y = y }; }\n",
        );
    let undriven: Vec<&String> = diags
        .iter()
        .filter(|d| d.contains("W-P011") && d.contains("never driven"))
        .collect();
    assert_eq!(undriven.len(), 1, "one undriven signal: {undriven:?}");
    assert!(undriven[0].contains("dead"), "flags dead: {undriven:?}");
}

#[test]
/// An unused internal signal warns, without the test runner's own signals
/// producing false positives.
fn unused_internal_signal_warns_without_runner_false_positives() {
    let diags = lower_diags(
        "module m;\n\
             entity E { a: unsigned[8] in, y: unsigned[8] out }\n\
             impl E { let dead: unsigned[8]; dead = a + 1; y = a; }\n\
             #[test] entity T {}\n\
             impl T { let a: unsigned[8]; let observed: unsigned[8];\n\
               let dut: E = { .a = a, .y = observed }; assert!(observed == a); }\n",
    );
    let unused: Vec<&String> = diags.iter().filter(|d| d.contains("W-P003")).collect();
    assert_eq!(unused.len(), 1, "one unused internal signal: {unused:?}");
    assert!(unused[0].contains("dead"), "flags dead: {unused:?}");
}

#[test]
/// An `if`/`else` mux assigns on both paths, so it is not a latch.
fn if_else_mux_is_not_a_latch() {
    // A signal assigned in both the `if` and the `else` is fully covered —
    // no possible-latch warning — but one assigned only in the `if` is.
    let covered = lower_diags(
        "module m;\n\
             entity M { c: Bit in, a: unsigned[8] in, b: unsigned[8] in, y: unsigned[8] out }\n\
             impl M { if c { y = a; } else { y = b; } }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let c: Bit; let a: unsigned[8]; let b: unsigned[8]; let y: unsigned[8];\n\
               let dut: M = { .c = c, .a = a, .b = b, .y = y };\n\
             }\n",
    );
    assert!(
        !covered.iter().any(|d| d.contains("inferred latch")),
        "if/else mux wrongly flagged: {covered:?}"
    );

    let latch = lower_diags(
        "module m;\n\
             entity M { c: Bit in, a: unsigned[8] in, y: unsigned[8] out }\n\
             impl M { if c { y = a; } }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let c: Bit; let a: unsigned[8]; let y: unsigned[8];\n\
               let dut: M = { .c = c, .a = a, .y = y };\n\
             }\n",
    );
    assert!(
        latch.iter().any(|d| d.contains("inferred latch")),
        "true latch (no else) should warn: {latch:?}"
    );
}

#[test]
/// Assignment widths are strict: a mismatch is reported rather than
/// silently resized.
fn strict_assignment_width_mismatch() {
    // A parameterized width (`unsigned[W]`) the type checker can't see resolves
    // at elaboration; assigning a 16-bit signal to an 8-bit target is then a
    // width mismatch surfaced by IR lowering.
    let bad = lower_diags(
        "module m;\n\
             entity E { b: unsigned[W] in, y: unsigned[8] out }\n\
             impl E { y = b; }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let b: unsigned[16]; let y: unsigned[8];\n\
               let dut: E<W=16> = { .b = b, .y = y };\n\
             }\n",
    );
    assert!(bad.iter().any(|d| d.contains("width mismatch")), "{bad:?}");

    // A matching-width slice of the same signal is fine — the value width
    // (8) equals the target (8).
    let ok = lower_diags(
        "module m;\n\
             entity E { b: unsigned[W] in, y: unsigned[8] out }\n\
             impl E { y = b[7..0]; }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let b: unsigned[16]; let y: unsigned[8];\n\
               let dut: E<W=16> = { .b = b, .y = y };\n\
             }\n",
    );
    assert!(!ok.iter().any(|d| d.contains("width mismatch")), "{ok:?}");

    // Indexing remains scalar even when the vector itself is one element
    // wide. Retained checker types keep it distinct from the vector.
    let one = lower_diags(
        "module m;\n\
             entity E { b: unsigned[1] in, y: Bit out }\n\
             impl E { y = b[0]; }\n",
    );
    assert!(!one.iter().any(|d| d.contains("width mismatch")), "{one:?}");
}

#[test]
/// A combinational cycle with no register warns (W-P010).
fn combinational_loop_lint() {
    // `t = t + a;` is a zero-delay self-cycle -> flagged; a plain chain
    // (`y = x + 1`) is not.
    let diags = lower_diags(
        "module m;\n\
             entity L { a: unsigned[8] in, y: unsigned[8] out }\n\
             impl L { let t: unsigned[8]; t = t + a; y = t; }\n\
             entity Top {}\n\
             impl Top { let a: unsigned[8]; let y: unsigned[8]; let d: L = { .a = a, .y = y }; }\n",
    );
    let loops: Vec<&String> = diags.iter().filter(|d| d.contains("W-P010")).collect();
    assert!(!loops.is_empty(), "self-cycle flagged: {diags:?}");
    assert!(loops.iter().any(|d| d.contains(".t")), "names t: {loops:?}");

    let ok = lower_diags(
        "module m;\n\
             entity C { x: unsigned[8] in, y: unsigned[8] out }\n\
             impl C { y = x + 1; }\n\
             entity Top {}\n\
             impl Top { let x: unsigned[8]; let y: unsigned[8]; let d: C = { .x = x, .y = y }; }\n",
    );
    assert!(
        !ok.iter().any(|d| d.contains("W-P010")),
        "no false positive: {ok:?}"
    );
}

#[test]
/// A branch that does not assign on every path warns as a possible latch.
fn possible_latch_lint() {
    // `y` is only assigned under a condition (inferred latch); `z` has an
    // unconditional default and must not be flagged.
    let diags = lower_diags(
        "module m;\n\
             entity L { c: Logic in, a: unsigned[8] in, y: unsigned[8] out, z: unsigned[8] out }\n\
             impl L { if c == '1' { y = a; } z = a; }\n\
             entity Top {}\n\
             impl Top { let c: Logic; let a: unsigned[8]; let y: unsigned[8]; let z: unsigned[8];\n\
               let d: L = { .c = c, .a = a, .y = y, .z = z }; }\n",
    );
    let latch: Vec<&String> = diags.iter().filter(|d| d.contains("W-P002")).collect();
    assert_eq!(latch.len(), 1, "exactly one latch warning: {diags:?}");
    assert!(latch[0].contains(".y"), "flags y, not z: {latch:?}");
}

#[test]
/// Enum signals carry their variant symbols, so waveforms show names rather
/// than numbers.
fn enum_signals_carry_symbols() {
    // A Logic-typed signal records its enum type, and the design exports the
    // discriminant -> symbol map (with std's char-variant names) so
    // consumers can print `'X'` instead of `3`.
    let d = lower_src(
        "module m;\n\
             enum Logic { '0', '1', 'Z', 'X' }\n\
             enum State { Idle, Run }\n\
             entity E { a: Logic in, s: State out }\n\
             impl E { s = State::Idle; }\n\
             entity Top {}\n\
             impl Top { let a: Logic; let s: State; let e: E = { .a = a, .s = s }; }\n",
    );
    let sig = |p: &str| d.signals.iter().find(|s| s.path == p).unwrap();
    assert_eq!(sig("Top.e.a").enum_type.as_deref(), Some("Logic"));
    assert_eq!(sig("Top.e.s").enum_type.as_deref(), Some("State"));
    assert_eq!(
        d.enum_syms["Logic"].get(&3).map(String::as_str),
        Some("'X'")
    );
    assert_eq!(
        d.enum_syms["State"].get(&0).map(String::as_str),
        Some("Idle")
    );
}
