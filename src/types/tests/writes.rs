//! Write legality, constants, initializers, and value ranges.

use super::*;

#[test]
/// Writing an input leaf of a bus is rejected like any other input write.
fn rejects_write_to_input_bus_leaf() {
    // Driving an `in` leaf of a bus-mode port (`bus.ready` in the Source
    // view) is a write to an input (spec 3.19) — a clear E-P004.
    let bad = check_src(
        "module m;\n\
             struct S { valid: Bit, ready: Bit, }\n\
             view Source for S { valid out, ready in }\n\
             entity P { bus: S Source }\n\
             impl P { bus.valid = '1'; bus.ready = '1'; }\n",
    );
    assert_eq!(bad, 1, "driving the `in` leaf bus.ready must error");

    // Driving only the `out` leaves is fine.
    let ok = check_src(
        "module m;\n\
             struct S { valid: Bit, ready: Bit, }\n\
             view Source for S { valid out, ready in }\n\
             entity P { bus: S Source, r: Bit out }\n\
             impl P { bus.valid = '1'; r = bus.ready; }\n",
    );
    assert_eq!(ok, 0, "driving out leaves + reading in leaves is fine");
}

#[test]
/// A chain of integer aliases still enforces the underlying value range.
fn chained_integer_aliases_still_enforce_value_ranges() {
    let errors = check_src(
        "module m;\n\
             using Small = integer<0..3>;\n\
             using Alias = Small;\n\
             entity E { ok: Bit out }\n\
             impl E { let value: Alias = 4; ok = '1'; }\n",
    );
    assert_eq!(errors, 1, "a chained alias must not bypass range checks");
}

#[test]
/// Free function parameters and locals keep their declared types.
fn free_function_parameters_and_locals_keep_their_declared_types() {
    let parameter = check_src(
        "module m;\n\
             fn bad(value: Logic) { if value { return; } }\n",
    );
    assert_eq!(
        parameter, 1,
        "a free-function parameter must participate in condition checking"
    );

    let local = check_src(
        "module m;\n\
             fn bad(value: Logic) -> unsigned[8] {\n\
               let copy: Logic = value;\n\
               return copy;\n\
             }\n",
    );
    assert_eq!(
        local, 1,
        "a block-local declaration must participate in return checking"
    );

    let unknown = check_src(
        "module m;\n\
             fn bad() -> Logic { return missing; }\n",
    );
    assert_eq!(
        unknown, 1,
        "an unknown value in a free-function body must not disappear as Ty::Error"
    );
}

#[test]
/// Assigning to a `const` is rejected.
fn assigning_to_a_const_is_rejected() {
    let has = |src: &str| diag_codes(src).iter().any(|c| c.contains("E-P018"));
    // This reached the emitter as "unknown signal `K`" — a message naming
    // something the author had in fact declared.
    assert!(has(
        "module m;\nentity E { y: Bit out, }\nimpl E { const K: Bit = '1'; K = '0'; y = K; }\n"
    ));
    assert!(!has(
        "module m;\nentity E { y: Bit out, }\nimpl E { const K: Bit = '1'; y = K; }\n"
    ));
    // A `let` of the same shape is storage and stays writable.
    assert!(!has(
        "module m;\nentity E { y: Bit out, }\nimpl E { let k: Bit; k = '0'; y = k; }\n"
    ));
}

#[test]
/// Two `let` declarations of one name in a scope are an error.
fn duplicate_let_is_an_error() {
    let has = |src: &str| diag_codes(src).iter().any(|c| c.contains("E-P002"));
    // A scalar silently shadowed; a struct emitted its field locals twice
    // and failed at link with a clang error naming a mangled symbol.
    assert!(has(
        "module m;\nentity E { y: Bit out, }\nimpl E { let a: Bit; let a: Bit; y = a; }\n"
    ));
    assert!(!has(
        "module m;\nentity E { y: Bit out, }\nimpl E { let a: Bit; let b: Bit; y = a and b; }\n"
    ));
}

#[test]
/// A reset written as an edge rather than a level warns (W-P009).
fn edge_detected_reset_warns() {
    let src = "module m;\nentity E { reset: Bit in, q: Bit out, }\n\
                   impl E { if reset.rising() { q = '0'; } }\n";
    assert_eq!(warnings(src, codes::SUSPICIOUS_RESET), 1);

    let level = "module m;\nentity E { clk: Bit in, reset: Bit in, q: Bit out, }\n\
                     impl E { if clk.rising() { if reset { q = '0'; } } }\n";
    assert_eq!(warnings(level, codes::SUSPICIOUS_RESET), 0);
}

#[test]
/// Writing an input port is rejected (E-P004).
fn rejects_write_to_input_port() {
    let errors = check_src(
        "module m;\nentity E { en: Bit in, y: Bit out, }\nimpl E {\n  en = '1';\n  y = en;\n}\n",
    );
    assert_eq!(errors, 1);
}

#[test]
/// Writing an output port is the normal case and is accepted.
fn writing_output_is_fine() {
    let errors =
        check_src("module m;\nentity E { en: Bit in, y: Bit out, }\nimpl E {\n  y = en;\n}\n");
    assert_eq!(errors, 0);
}

#[test]
/// Writing an input through a field or index is rejected too.
fn rejects_write_to_plain_input_field_or_index() {
    // A field/index of a *plain* `in` port is read-only too.
    let errors = check_src(
        "module m;\nstruct P { pub x: Bit }\nentity E { a: Bit in, p: P in, y: Bit out, }\n\
             impl E {\n  a = '1';\n  p.x = '1';\n  y = a;\n}\n",
    );
    assert_eq!(errors, 2, "bare `a` and field `p.x` are both rejected");
}

#[test]
/// Assigning a `Bool` to a `Bit` port is rejected: they are distinct types.
fn assigning_bool_to_a_bit_port_is_rejected() {
    let errors = check_src(
        "module m;\nentity E { en: Bit in, y: Bit out, }\nimpl E {\n  y = en == en;\n}\n",
    );
    // `en == en` is Bool; `y` is Bit.
    assert_eq!(errors, 1);
}

#[test]
/// An enum assignment takes the enum's type.
fn enum_assignment_uses_the_enum_type() {
    let errors = check_src(
            "module m;\nenum State { Idle, Run }\nentity E { s: State out, }\nimpl E {\n  s = State::Idle;\n}\n",
        );
    assert_eq!(errors, 0);
}

#[test]
/// An initializer of the wrong type is rejected.
fn bad_initializer_type_is_rejected() {
    let errors = check_src(
        "module m;\nentity E { y: Bit out, }\nimpl E {\n  let flag: Bool = 5;\n  y = '0';\n}\n",
    );
    assert_eq!(errors, 1);
}

/// A ranged numeric (spec 3.26) checked its `let` initializer but not an
/// assignment — and the value wraps to the storage width (50 -> 2), so the
/// runtime range assert saw an in-range value and the violation vanished.
#[test]
fn ranged_numeric_assignment_is_checked() {
    let ent = "module m;\nentity E { y: integer<0..10> out, }\nimpl E { y = ";
    assert_eq!(check_src(&format!("{ent}50; }}\n")), 1, "above the range");
    assert_eq!(
        check_src(&format!("{ent}0 - 1; }}\n")),
        1,
        "below the range"
    );
    assert_eq!(check_src(&format!("{ent}7; }}\n")), 0, "inside");
    assert_eq!(check_src(&format!("{ent}10; }}\n")), 0, "the top bound");
    // An impl-level ranged local is covered the same way.
    let local = "module m;\nentity E { y: integer<0..10> out, }\n\
                     impl E { let k: integer<0..10>; k = 99; y = k; }\n";
    assert_eq!(check_src(local), 1);
}

#[test]
/// A view method cannot drive a leaf the view marks as input.
fn a_view_method_cannot_drive_an_input_leaf() {
    let count = |src: &str| {
        let src = format!("{src}{VEC}");
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        sink.diagnostics()
            .iter()
            .filter(|d| d.code == Some(codes::WRITE_TO_INPUT_PORT))
            .count()
    };
    const BUS: &str = "module m;\n\
            struct Stream { valid: Bit, data: unsigned[8], ready: Bit }\n\
            view StreamSource for Stream { valid out, data out, ready in }\n\
            view StreamSink for Stream { valid in, data in, ready out }\n";

    // `ready` is an input for the Source role.
    assert_eq!(
        count(&format!(
            "{BUS}impl Stream StreamSource {{ fn bad(self) {{ self.ready = '1'; }} }}\n"
        )),
        1,
        "a Source method driving `ready`"
    );
    // The Sink role has the opposite polarity, so `valid` is its input.
    assert_eq!(
        count(&format!(
            "{BUS}impl Stream StreamSink {{ fn bad(self) {{ self.valid = '1'; }} }}\n"
        )),
        1,
        "a Sink method driving `valid`"
    );
    // Nested inside control flow, where the walk has to carry the context.
    assert_eq!(
        count(&format!(
            "{BUS}impl Stream StreamSource {{ \
                 fn bad(self, e: Bit) {{ if e == '1' {{ self.ready = '1'; }} }} }}\n"
        )),
        1,
        "and one hidden inside an `if`"
    );

    // The outputs of each role stay writable — this is what methods are for.
    assert_eq!(
        count(&format!(
            "{BUS}impl Stream StreamSource {{ \
                 fn send(self, v: unsigned[8]) {{ self.valid = '1'; self.data = v; }} }}\n"
        )),
        0,
        "a Source may drive `valid` and `data`"
    );
    assert_eq!(
        count(&format!(
            "{BUS}impl Stream StreamSink {{ fn accept(self) {{ self.ready = '1'; }} }}\n"
        )),
        0,
        "and a Sink may drive `ready`"
    );

    // A plain struct carries no directions, so every field is writable.
    assert_eq!(
        count(
            "module m;\nstruct Pair { a: unsigned[8], b: unsigned[8] }\n\
                 impl Pair { fn set(self) { self.a = 1; self.b = 2; } }\n"
        ),
        0,
        "a plain struct method writes any field"
    );

    // The same hole, one target over: a function in an *entity* impl
    // inlines into that entity's body, so the entity's own `in` ports and
    // `const`s are off limits there too. Both were accepted inside a
    // function and rejected written inline.
    const ENT: &str = "module m;\n\
            entity E { a: unsigned[8] in, y: unsigned[8] out }\n";
    assert_eq!(
        count(&format!(
            "{ENT}impl E {{ fn writes() {{ a = 99; }} y = a; }}\n"
        )),
        1,
        "an entity function driving an `in` port"
    );
    assert_eq!(
        count(&format!(
            "{ENT}impl E {{ fn deep(e: Bit) {{ if e == '1' {{ a = 99; }} }} y = a; }}\n"
        )),
        1,
        "and one nested in control flow"
    );
    // Outputs and locals stay writable, or a helper could not do anything.
    assert_eq!(
        count(&format!(
            "{ENT}impl E {{ fn plus(n: unsigned[8]) -> unsigned[8] {{ return n + 1; }}\n\
                 y = E::plus(a); }}\n"
        )),
        0,
        "a helper that reads its argument and returns"
    );
}

/// The `const` half of the same hole: a `const` declared in an impl is
/// fixed at elaboration, and assigning to it is `E-P018` written inline.
/// Inside a function of that impl it was accepted.
#[test]
fn an_impl_function_cannot_assign_to_a_const() {
    let count = |src: &str| {
        let src = format!("{src}{VEC}");
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        sink.diagnostics()
            .iter()
            .filter(|d| d.code == Some(codes::INVALID_ASSIGN_TARGET))
            .count()
    };
    assert_eq!(
        count(
            "module m;\nentity E { y: unsigned[8] out }\n\
                 impl E { const K: unsigned[8] = 5; fn bad() { K = 1; } y = K; }\n"
        ),
        1,
        "a function assigning to its impl's `const`"
    );
    assert_eq!(
        count(
            "module m;\nentity E { y: unsigned[8] out }\n\
                 impl E { const K: unsigned[8] = 5; fn ok() -> unsigned[8] { return K; } \
                 y = E::ok(); }\n"
        ),
        0,
        "reading it is fine"
    );
    // A parameter shadows the impl-level name it repeats, so this writes
    // its own argument and not the `const`. Inheriting the restrictions
    // without this exclusion rejected it.
    assert_eq!(
        count(
            "module m;\nentity E { y: unsigned[8] out }\n\
                 impl E { const K: unsigned[8] = 5; \
                 fn shadow(K: unsigned[8]) -> unsigned[8] { K = K + 1; return K; } \
                 y = E::shadow(1); }\n"
        ),
        0,
        "a parameter named `K` is not the `const`"
    );
}

/// The ranged-integer bounds shared the same hole: `y = 20` on an
/// `integer<0..7>` is a compile-time error written inline, and inside a
/// function of the same impl it was not checked at all, because the body
/// walk was handed an empty bounds map along with its empty directions.
#[test]
fn an_impl_function_checks_ranged_assignments() {
    let count = |src: &str| {
        let src = format!("{src}{VEC}");
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        sink.diagnostics()
            .iter()
            .filter(|d| d.message.contains("outside the range"))
            .count()
    };
    assert_eq!(
        count(
            "module m;\nentity E { y: integer<0..7> out }\n\
                 impl E { fn drive() { y = 20; } }\n"
        ),
        1,
        "an out-of-range constant inside a function"
    );
    assert_eq!(
        count(
            "module m;\nentity E { y: integer<0..7> out }\n\
                 impl E { fn drive(e: Bit) { if e == '1' { y = 20; } } }\n"
        ),
        1,
        "and one nested in control flow"
    );
    assert_eq!(
        count(
            "module m;\nentity E { y: integer<0..7> out }\n\
                 impl E { fn drive() { y = 5; } }\n"
        ),
        0,
        "a value inside the range is fine"
    );
    // Again the shadowing rule: the parameter has its own type.
    assert_eq!(
        count(
            "module m;\nentity E { y: integer<0..7> out }\n\
                 impl E { fn wide(y: unsigned[8]) -> unsigned[8] { y = 20; return y; } }\n"
        ),
        0,
        "a parameter named `y` is not the ranged port"
    );
}

/// The last of the three things a function body was denied: types. It was
/// walked with an empty symbol table, so the strict assignment-width rule
/// had nothing to compare and never fired inside a method. A method's own
/// parameters are declared right there, and typing them is enough for the
/// rule to work on them.
#[test]
fn an_impl_function_checks_widths_of_its_own_parameters() {
    let count = |src: &str| {
        let src = format!("{src}{VEC}");
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        sink.diagnostics()
            .iter()
            .filter(|d| d.message.contains("without an explicit conversion"))
            .count()
    };
    assert_eq!(
        count(
            "module m;\nstruct S { }\n\
                 impl S { fn f(self, wide: unsigned[16], narrow: unsigned[8]) \
                 { narrow = wide; } }\n"
        ),
        1,
        "a 16-bit parameter assigned into an 8-bit one"
    );
    assert_eq!(
        count(
            "module m;\nstruct S { }\n\
                 impl S { fn f(self, a: unsigned[8], b: unsigned[8]) { b = a; } }\n"
        ),
        0,
        "matching widths are fine"
    );
    assert_eq!(
        count(
            "module m;\nstruct S { }\n\
                 impl S { fn f(self, wide: unsigned[16], narrow: unsigned[8]) \
                 { narrow = unsigned[8](wide); } }\n"
        ),
        0,
        "an explicit conversion is the way through"
    );

    // The target that matters: a struct *field*. `type_of` returned
    // `Ty::Error` for any data field access, which suppresses every check
    // that consults it, so this truncated 0x1234 into eight bits in
    // silence — through a view method, which does inline.
    const BUS: &str = "module m;\nstruct Inner { v: unsigned[8] }\n\
            struct Bus { data: unsigned[8], flag: Bit, inner: Inner }\n";
    assert_eq!(
        count(&format!(
            "{BUS}impl Bus {{ fn load(self, wide: unsigned[16]) {{ self.data = wide; }} }}\n"
        )),
        1,
        "a wide parameter into a narrow field"
    );
    assert_eq!(
        count(&format!(
            "{BUS}view BusOut for Bus {{ data out, flag out, inner out }}\n\
                 impl Bus BusOut {{ fn load(self, wide: unsigned[16]) {{ self.data = wide; }} }}\n"
        )),
        1,
        "and the same through a view, where `self` is the backing struct"
    );
    assert_eq!(
        count(&format!(
            "{BUS}impl Bus {{ fn deep(self, wide: unsigned[16]) {{ self.inner.v = wide; }} }}\n"
        )),
        1,
        "a nested field types too"
    );

    // Legitimate field writes must stay legal, or every method breaks.
    assert_eq!(
        count(&format!(
            "{BUS}impl Bus {{ fn ok(self, v: unsigned[8]) {{ self.data = v; }}\n\
                 fn conv(self, w: unsigned[16]) {{ self.data = unsigned[8](w); }}\n\
                 fn nest(self, v: unsigned[8]) {{ self.inner.v = v; }}\n\
                 fn lit(self) {{ self.data = 200; self.flag = '1'; }} }}\n"
        )),
        0,
        "matching widths, conversions, nesting and literals are all fine"
    );
}

/// Assigning one target twice unconditionally makes the first dead — the
/// later driver overrides within a context, so it silently did nothing.
#[test]
fn dead_assignment_warns_but_defaults_do_not() {
    let dead =
        "module m;\nentity E { y: unsigned[8] out, }\nimpl E {\n  process { y = 1; y = 2; }\n}\n";
    assert_eq!(warnings(dead, codes::DEAD_ASSIGNMENT), 1);

    // A conditional override is the normal `default then override` shape.
    let guarded = "module m;\nentity E { c: Bit in, y: unsigned[8] out, }\nimpl E {\n  process { y = 1; if c == '1' { y = 2; } }\n}\n";
    assert_eq!(warnings(guarded, codes::DEAD_ASSIGNMENT), 0);

    // Distinct targets are unrelated.
    let distinct = "module m;\nentity E { y: unsigned[8] out, z: unsigned[8] out, }\nimpl E {\n  y = 1;\n  z = 2;\n}\n";
    assert_eq!(warnings(distinct, codes::DEAD_ASSIGNMENT), 0);

    // Testbench stimulus settles after each connected-signal write. The
    // first half of a clock pulse is observable even with no `await`
    // between these statements, so neither it nor an unrolled repetition
    // belongs to the driver-context lint.
    let stimulus = "module m;\n#[test] entity T {}\nimpl T {\n  let clk: Bit = '0';\n  process { clk = '1'; clk = '0'; for i in 0..1 { clk = '1'; clk = '0'; } }\n}\n";
    assert_eq!(warnings(stimulus, codes::DEAD_ASSIGNMENT), 0);
}
