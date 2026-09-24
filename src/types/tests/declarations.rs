//! Structs, enums, newtypes, views, traits, and type layouts.

use super::*;

/// A bus port types as its *view*, which owns no fields, so the field
/// check found no struct behind it and returned silently — every field
/// access through a bus went unchecked, whether the entity was
/// instantiated or not.
#[test]
fn a_bus_port_checks_fields_against_its_backing_struct() {
    let errors = check_src(
        "module m;\n\
             struct S { a: Bit, b: Bit }\n\
             view V for S { a out, b in }\n\
             entity E { bus: S V, q: Bit out }\n\
             impl E { q = bus.nosuch; }\n",
    );
    assert_eq!(errors, 1, "a missing field behind a view is reported");
}

/// The fields the view does declare still resolve, and so do methods on
/// the backing struct — both reach the check as field nodes.
#[test]
fn a_bus_port_accepts_real_fields_and_struct_methods() {
    let errors = check_src(
        "module m;\n\
             struct S { a: Bit, b: Bit }\n\
             view V for S { a out, b in }\n\
             impl S { pub fn helper(self) -> Bit { return self.a; } }\n\
             entity E { bus: S V, q: Bit out, r: Bit out }\n\
             impl E { q = bus.a; r = bus.helper(); }\n",
    );
    assert_eq!(errors, 0, "declared fields and methods still resolve");
}

#[test]
/// Expression types are recorded for every node, so later stages can look
/// one up by span.
fn typed_records_expression_types() {
    let src = format!(
        "module m;\nentity E {{ a: unsigned[8] in, y: Logic out, }}\n\
             impl E {{ y = a[0]; }}\n{VEC}"
    );
    let mut sink = DiagnosticSink::new();
    let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
    let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
    let typed = check(std::slice::from_ref(&module), &resolved, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let logic = resolved
        .defs()
        .iter()
        .position(|def| def.name == "Logic")
        .map(|index| Ty::Named(crate::resolve::DefId(index as u32)))
        .expect("Logic definition");
    assert!(
        typed.expr_types().values().any(|ty| *ty == logic),
        "the indexed vector element type is retained"
    );
}

#[test]
/// Trait impls overload by backing struct, so two views over different
/// structs may both implement one trait.
fn views_overload_by_backing_struct_in_trait_impls() {
    let errors = check_src(
        "module m;\n\
             trait Send<T> { fn send(self, value: T); }\n\
             struct Stream { data: Bit, ready: Bit }\n\
             struct Queue { data: Bit, full: Bit }\n\
             view Source for Stream { data out, ready in }\n\
             view Source for Queue { data out, full in }\n\
             impl Send<Bit> for Stream Source {\n\
               fn send(self, value: Bit) { self.data = value; }\n\
             }\n\
             impl Send<Bit> for Queue Source {\n\
               fn send(self, value: Bit) { self.data = value; }\n\
             }\n\
             entity StreamProducer { bus: Stream Source }\n\
             entity QueueProducer { bus: Queue Source }\n",
    );
    assert_eq!(errors, 0, "the view/backing pair is the nominal identity");
}

#[test]
/// A chain of struct aliases still validates literals against the real
/// fields.
fn chained_struct_aliases_still_validate_literals() {
    let errors = check_src(
        "module m;\n\
             struct S { a: Bit }\n\
             using A = S;\n\
             using B = A;\n\
             entity E { ok: Bit out }\n\
             impl E { let x: B = { .nosuch = '1' }; ok = '1'; }\n",
    );
    assert!(errors > 0, "a chained alias must not bypass struct checks");
}

#[test]
/// Struct literal fields are checked wherever the literal appears, not only
/// in a `let`.
fn struct_literal_fields_are_checked_in_every_value_context() {
    let errors = check_src(
        "module m;\n\
             struct Packet { pub data: unsigned[8] }\n\
             fn consume(packet: Packet) {}\n\
             fn make_bad(r: real) -> Packet { return { .data = r }; }\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let r: real = 1.5;\n\
               let packet: Packet;\n\
               packet = { .data = r };\n\
               consume({ .data = r });\n\
               y = '0';\n\
             }\n",
    );
    assert_eq!(
        errors, 3,
        "assignment, return, and call arguments all supply struct-field context"
    );
}

#[test]
/// An array alias preserves the declared index bounds rather than
/// renormalizing them.
fn array_aliases_preserve_declared_index_bounds() {
    let errors = check_src(
        "module m;\n\
             using Window = Logic[15..8];\n\
             entity E { y: Logic out }\n\
             impl E { let data: Window; y = data[0]; }\n",
    );
    assert_eq!(
        errors, 1,
        "an alias must not turn a ranged array into an unchecked zero-based array"
    );
}

#[test]
/// A vector newtype reports its real family in diagnostics, not the
/// underlying array.
fn vector_names_its_real_family() {
    // A known family displays by name; anonymous vectors fall back to unsigned.
    let int8 = Ty::Array {
        elem: Box::new(Ty::Named(crate::resolve::DefId(0))),
        family: Some("signed".to_string()),
        len: 8,
    };
    assert_eq!(ty_name(&int8), "signed[8]");
    let byte = Ty::Array {
        elem: Box::new(Ty::Named(crate::resolve::DefId(0))),
        family: Some("Byte".to_string()),
        len: 0,
    };
    assert_eq!(ty_name(&byte), "Byte");
    let anon = Ty::Array {
        elem: Box::new(Ty::Named(crate::resolve::DefId(0))),
        family: Some("unsigned".to_string()),
        len: 4,
    };
    assert_eq!(ty_name(&anon), "unsigned[4]");
    // Width still ignores the family: unsigned[8] and signed[8] stay compatible.
    assert!(compatible(
        &int8,
        &Ty::Array {
            elem: Box::new(Ty::Named(crate::resolve::DefId(0))),
            family: Some("unsigned".to_string()),
            len: 8
        }
    ));
}

#[test]
/// Struct literal field names are checked against the declaration.
fn struct_literal_field_names_are_checked() {
    let has = |src: &str, code: &str| diag_codes(src).iter().any(|c| c.contains(code));
    let base = "module m;\nstruct S { pub a: Bit, pub b: Bit }\nentity E { y: Bit out, }\nimpl E { let s: S = LIT; y = s.a; }\n";
    // A misspelled name was dropped whole and the literal still checked.
    assert!(has(
        &base.replace("LIT", "{ .a = '1', .zz = '0' }"),
        "E-P003"
    ));
    assert!(!has(
        &base.replace("LIT", "{ .a = '1', .b = '0' }"),
        "E-P003"
    ));
    // Omitting one is legal (it defaults) but worth saying out loud.
    assert!(has(&base.replace("LIT", "{ .a = '1' }"), "W-P016"));
    assert!(!has(
        &base.replace("LIT", "{ .a = '1', .b = '0' }"),
        "W-P016"
    ));
    // A spread supplies the rest, so nothing is left implicit.
    let with_base = "module m;\nstruct S { pub a: Bit, pub b: Bit }\nentity E { y: Bit out, }\nimpl E { let p: S = { .a = '1', .b = '0' }; let s: S = { ..p, .a = '0' }; y = s.a; }\n";
    assert!(!has(with_base, "W-P016"));
}

#[test]
/// A struct containing itself has no finite layout and is rejected.
fn a_struct_containing_itself_is_rejected() {
    let bad = |src: &str| diag_codes(src).iter().any(|c| c.contains("E-P003"));
    // Elaboration flattens a struct into leaf signals, so each of these
    // recursed until the process aborted — with typecheck reporting
    // nothing at all. Four lines was enough.
    assert!(bad(
        "module m;\nstruct S { f: S }\nentity E { y: Bit out, }\nimpl E { let v: S; y = '0'; }\n"
    ));
    assert!(bad(
            "module m;\nstruct A { f: B }\nstruct B { f: A }\nentity E { y: Bit out, }\nimpl E { let v: A; y = '0'; }\n"
        ));
    // Through an array element, which is just as infinite.
    assert!(bad(
            "module m;\nstruct A { f: A[2] }\nentity E { y: Bit out, }\nimpl E { let v: A; y = '0'; }\n"
        ));
    // Ordinary nesting, and the same struct used twice, stay legal.
    assert!(!bad(
        "module m;\nstruct I { x: Bit }\nstruct O { a: I, b: I }\n\
             entity E { y: Bit out, }\nimpl E { let v: O; y = v.a.x; }\n"
    ));
}

#[test]
/// It preserves its declared element type rather than the base array's.
fn nominal_array_newtype_preserves_its_declared_element_type() {
    let errors = check_src(
        "module m;\n\
             enum Cell { Off, On }\n\
             struct Cells(Cell[]);\n\
             entity E { a: Cells[4] in, y: Cell out }\n\
             impl E { y = a[0]; }\n",
    );
    assert_eq!(errors, 0);
}

#[test]
/// `self` in a method signature is the impl target.
fn self_in_method_signatures_is_the_impl_target() {
    let methods = "impl Left {\n\
                         pub fn choose(self, rhs: Self) -> Self { return rhs; }\n\
                         pub fn identity(value: Self) -> Self { return value; }\n\
                       }\n";
    let header = "module m;\nstruct Left { a: Bit }\nstruct Right { b: Bit }\n";
    assert_eq!(
        check_src(&format!(
            "{header}{methods}entity E {{ a: Left in, b: Left in, }}\n\
                 impl E {{ let x: Left = a.choose(b); let y: Left = Left::identity(x); }}\n"
        )),
        0,
        "`Self` parameters and returns rebind at method call sites"
    );
    assert_eq!(
        check_src(&format!(
            "{header}{methods}entity E {{ a: Left in, b: Right in, }}\n\
                 impl E {{ let bad: Left = a.choose(b); }}\n"
        )),
        1,
        "a `Self` parameter rejects a different nominal type"
    );
}

#[test]
/// `self` in an index contract is likewise the impl target.
fn self_in_index_contracts_is_the_impl_target() {
    let contracts = "module m;\n\
             struct Box { pub value: integer }\n\
             impl Index<Self, Self> for Box {\n\
               fn index(self, index: Self) -> Self { return index; }\n\
             }\n\
             impl IndexAssign<Self, Self> for Box {\n\
               fn index_assign(self, index: Self, value: Self) {}\n\
             }\n";
    let errors = check_src(&format!(
        "{}{}",
        contracts,
        "\
             #[test] entity T {}\n\
             impl T {\n\
               let left: Box = Box { .value = 1 };\n\
               let right: Box = Box { .value = 2 };\n\
               let selected: Box = left[right];\n\
               left[right] = selected;\n\
             }\n"
    ));
    assert_eq!(
        errors, 0,
        "`Self` selects the owner for Index input/output and IndexAssign values"
    );

    assert_eq!(
        check_src(&format!(
            "{}{}",
            contracts,
            "\
                 struct Other { pub value: integer }\n\
                 #[test] entity T {}\n\
                 impl T {\n\
                   let container: Box = Box { .value = 1 };\n\
                   let other: Other = Other { .value = 2 };\n\
                   let invalid: Box = container[other];\n\
                 }\n",
        )),
        1,
        "`Self` does not accept an unrelated index type"
    );

    assert_eq!(
        check_src(&format!(
            "{}{}",
            contracts,
            "\
                 struct Other { pub value: integer }\n\
                 #[test] entity T {}\n\
                 impl T {\n\
                   let container: Box = Box { .value = 1 };\n\
                   let index: Box = Box { .value = 2 };\n\
                   let other: Other = Other { .value = 3 };\n\
                   container[index] = other;\
                 }\n",
        )),
        1,
        "`Self` does not accept an unrelated assigned value type"
    );
}

#[test]
/// A type whose layout cannot be represented is rejected before lowering,
/// where the failure would be harder to explain.
fn unrepresentable_type_layouts_are_rejected_before_lowering() {
    let range = "module m;\n\
            entity E { y: Logic[-9223372036854775807..9223372036854775807] out }\n\
            impl E {}\n";
    assert_eq!(check_src(range), 1, "the range length exceeds u32");

    let width = "module m;\n\
            entity E { y: unsigned[4294967296] out }\n\
            impl E {}\n";
    assert_eq!(check_src(width), 1, "the width exceeds u32");

    let negative = "module m;\n\
            entity E { y: unsigned[-1] out }\n\
            impl E {}\n";
    assert_eq!(check_src(negative), 1, "negative widths are invalid");
}

/// Two variants with the same explicit value are indistinguishable at
/// runtime — `S::A == S::B` is true, and a waveform cannot tell them apart.
#[test]
fn colliding_enum_discriminants_are_reported() {
    assert_eq!(check_src("module m;\nenum S { A = 5, B = 5 }\n"), 1);
    assert_eq!(check_src("module m;\nenum S { A = 5, B = 6 }\n"), 0);
    // Implicit numbering cannot collide.
    assert_eq!(check_src("module m;\nenum S { A, B, C }\n"), 0);
}

/// A repeated field in a struct/connection literal silently kept one of
/// the values; and a type in a diagnostic must be named, not described as
/// "a named type".
#[test]
fn duplicate_literal_field_and_named_type_rendering() {
    let dup = "module m;\nstruct P { pub a: Bit, pub b: Bit }\nentity E { y: Bit out, }\n\
                   impl E { let q: P = { .a = '1', .a = '0' }; y = q.a; }\n";
    assert_eq!(check_src(dup), 1);

    let ok = "module m;\nstruct P { pub a: Bit, pub b: Bit }\nentity E { y: Bit out, }\n\
                  impl E { let q: P = { .a = '1', .b = '0' }; y = q.a; }\n";
    assert_eq!(check_src(ok), 0);

    // The bound diagnostic names the offending type.
    let bound = "module m;\nfn f<T: Operator>(a: T) -> T { return a; }\n\
                     struct Q { z: Bit }\nentity E { y: Bit out, }\n\
                     impl E { let q: Q; y = f(q).z; }\n";
    let mut sink = DiagnosticSink::new();
    let src = format!("{bound}{VEC}");
    let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
    let modules = std::slice::from_ref(&module);
    let resolved = crate::resolve::resolve(modules, &mut sink);
    check(modules, &resolved, &mut sink);
    assert!(
        sink.diagnostics()
            .iter()
            .any(|d| d.message.contains("`Q` does not satisfy")),
        "should name `Q`: {:?}",
        sink.diagnostics()
            .iter()
            .map(|d| &d.message)
            .collect::<Vec<_>>()
    );
}

#[test]
/// Struct literal values are checked against each field's declared type.
fn struct_literal_values_use_their_field_types() {
    let direct = check_src(
        "module m;\n\
             struct Packet { pub data: unsigned[8], pub valid: Bit }\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let logic: Logic = 'X';\n\
               let packet: Packet = { .data = logic, .valid = '1' };\n\
               y = packet.valid;\n\
             }\n",
    );
    assert_eq!(direct, 1, "a named field keeps its declared type");

    let nested = check_src(
        "module m;\n\
             struct Inner { pub data: unsigned[8] }\n\
             struct Outer { pub inner: Inner, pub valid: Bit }\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let logic: Logic = 'X';\n\
               let value: Outer = { .inner = { .data = logic }, .valid = '1' };\n\
               y = value.valid;\n\
             }\n",
    );
    assert_eq!(nested, 1, "nested literals are checked recursively");

    let wrong_type = check_src(
        "module m;\n\
             struct A { pub a: Bit }\n\
             struct B { pub b: Bit }\n\
             entity E { y: Bit out }\n\
             impl E { let value: A = B { .b = '1' }; y = value.a; }\n",
    );
    assert_eq!(
        wrong_type, 1,
        "an explicitly typed construction must match its destination"
    );

    let wrong_spread = check_src(
        "module m;\n\
             struct Packet { pub data: unsigned[8], pub valid: Bit }\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let logic: Logic = 'X';\n\
               let packet: Packet = { ..logic, .valid = '1' };\n\
               y = packet.valid;\n\
             }\n",
    );
    assert_eq!(wrong_spread, 1, "a spread must have the struct's type");

    let extra_positional = check_src(
        "module m;\n\
             struct Pair { pub a: Bit, pub b: Bit }\n\
             entity E { y: Bit out }\n\
             impl E { let pair: Pair = { '0', '1', '0' }; y = pair.a; }\n",
    );
    assert_eq!(
        extra_positional, 1,
        "extra positional values cannot be silently dropped"
    );

    let unknown_field_once = check_src(
        "module m;\n\
             struct Packet { pub data: unsigned[8] }\n\
             entity E { y: Bit out }\n\
             impl E { let packet: Packet = { .missing = 1 }; y = '1'; }\n",
    );
    assert_eq!(
        unknown_field_once, 1,
        "the struct literal checker must run exactly once"
    );
}

/// An unknown field or method lowered to `Unknown`: the driver silently
/// carried no value, and `if clk.typo()` produced an unknown *condition*,
/// quietly turning a clocked block combinational.
#[test]
fn unknown_field_and_method_are_reported() {
    let st =
            "module m;\nstruct P { pub a: Bit }\nimpl P { pub fn get(self) -> Bit { return self.a; } }\n\
                  entity E { y: Bit out }\nimpl E { let p: P; y = ";
    assert_eq!(
        check_src(&format!("{st}p.nosuch; }}\n")),
        1,
        "unknown field"
    );
    assert_eq!(check_src(&format!("{st}p.a; }}\n")), 0, "real field");
    // A method name reaches the field check through the call's callee.
    assert_eq!(
        check_src(&format!("{st}p.get(); }}\n")),
        0,
        "method, not a field"
    );
    assert_eq!(
        check_src(&format!("{st}p.nomethod(); }}\n")),
        1,
        "unknown method"
    );

    // A newtype's fields are its base's, so they count as present.
    let derived = "module m;\nstruct A { pub x: Bit }\nstruct B(A);\n\
                       entity E { o: Bit out }\nimpl E { let b: B; o = b.x; }\n";
    assert_eq!(check_src(derived), 0, "newtype field");
}

/// Spec 3.20 calls a trait a compile-time contract, but a partial impl
/// used to pass. A method the trait gives a default body is optional —
/// that is how the compiler-recognized traits (`Operator`, `Prefix`,
/// `Suffix`) allow an empty impl.
#[test]
fn trait_impl_must_provide_the_required_methods() {
    let base =
        "module m;\ntrait Tr { fn f(self) -> Bit; fn g(self) -> Bit; }\nstruct S { x: Bit }\n";
    assert_eq!(
        check_src(&format!(
            "{base}impl Tr for S {{ fn f(self) -> Bit {{ return self.x; }} }}\n"
        )),
        1,
        "`g` is missing"
    );
    assert_eq!(
        check_src(&format!(
            "{base}impl Tr for S {{ fn f(self) -> Bit {{ return self.x; }} \
                 fn g(self) -> Bit {{ return self.x; }} }}\n"
        )),
        0,
        "complete"
    );
    // A defaulted method is optional.
    let defaulted = "module m;\ntrait D { fn f(self) -> Bit { return '0'; } }\n\
                         struct S { x: Bit }\nimpl D for S {}\n";
    assert_eq!(check_src(defaulted), 0);
}

/// A derived enum shares its base's variant *definitions*, so `Mid::B` and
/// `Base::B` resolve to the same def. Typing the value from the declaring
/// enum made a newtype's own variants unassignable to it — `m = Mid::B`
/// was rejected as "cannot assign Base to Mid". The name written at the
/// use site decides.
#[test]
fn newtype_variant_has_the_named_enums_type() {
    let src = |body: &str| {
        format!("module m;\nenum Base {{ A, B }}\nenum Mid(Base);\nenum Top(Mid);\nentity E {{ m: Mid out, t: Top out, }}\nimpl E {{ {body} }}\n")
    };
    assert_eq!(
        check_src(&src("m = Mid::B; t = Top::A;")),
        0,
        "one and two hops"
    );
    // Still distinct types: the base's own variant needs a conversion.
    assert_eq!(
        check_src(&src("m = Base::B; t = Top::A;")),
        1,
        "a newtype is not its base"
    );
    assert_eq!(
        check_src(&src("m = Mid(Base::B); t = Top::A;")),
        0,
        "conversion is explicit"
    );
}

#[test]
/// Signal widths have no global word limit, so a very wide signal checks
/// like any other.
fn signal_width_has_no_global_word_limit() {
    let at = |w: u32| {
        check_src(&format!(
            "module m;\nentity E {{ y: unsigned[{w}] out, }}\nimpl E {{ y = 1; }}\n"
        ))
    };
    assert_eq!(at(129), 0);
    assert_eq!(at(512), 0);
    assert_eq!(at(4096), 0);
}

/// The literal-fits-width bounds are computed in `i64`, which saturates
/// right where bit vectors get interesting: `1i64 << 63` is `i64::MIN`, so
/// `- 1` overflowed at width 63 and the negation overflowed at width 64.
/// Any `a == 5` on a signal that wide panicked the compiler.
#[test]
fn literal_width_bounds_do_not_overflow_at_the_top_widths() {
    let cmp = |w: u32, v: &str| {
        check_src(&format!(
                "module m;\nentity E {{ a: unsigned[{w}] in, y: unsigned[8] out, }}\nimpl E {{ if a == {v} {{ y = 1; }} }}\n"
            ))
    };
    // Reaching these at all is the regression: they used to panic. `1`
    // is representable at every width, including 1 bit.
    for w in 1..=64 {
        assert_eq!(cmp(w, "1"), 0, "width {w} accepts a literal that fits");
    }
    // The bound is still a bound.
    assert_eq!(cmp(8, "255"), 0, "the largest 8-bit value fits");
    assert_eq!(cmp(8, "256"), 1, "one past it does not");
    assert_eq!(cmp(63, "9223372036854775807"), 0, "i64::MAX fits 63 bits");
}

#[test]
/// A `process` is only valid in an inherent entity impl (E-P027).
fn process_is_only_valid_on_an_inherent_entity_impl() {
    assert_eq!(
        check_src("module m;\nentity E { y: Bit out }\nimpl E { process drive { y = '1'; } }\n"),
        0,
        "an entity process is valid"
    );
    assert_eq!(
        check_src("module m;\nstruct S { pub x: Bit }\nimpl S { process drive {} }\n"),
        1,
        "a data type cannot own a process"
    );
    assert_eq!(
            check_src("module m;\ntrait T { fn f(self); }\nentity E {}\nimpl T for E { fn f(self) {} process drive {} }\n"),
            1,
            "a trait impl cannot add a process"
        );
}

/// The forms the newtype grammar admits. Extension is not among them —
/// `struct B(A)` has nowhere to put a body — so that is the parser's to
/// reject, not this stage's (see `syntax::parser`).
#[test]
fn struct_newtype_and_composition_are_the_two_shapes() {
    let newtype = check_src("module m;\nstruct A { x: Bit }\nstruct B(A);\n");
    assert_eq!(newtype, 0, "a newtype over another struct");
    let over_array = check_src("module m;\nstruct Word(Bit[]);\n");
    assert_eq!(over_array, 0, "a newtype over an array");
    let composed = check_src("module m;\nstruct A { x: Bit }\nstruct B { a: A, y: Bit }\n");
    assert_eq!(composed, 0, "composition builds the bigger type");
}

/// `enum B(A);` is a newtype over `A`'s variants, so `A` must be an enum.
/// The old `enum S : unsigned[2]` storage annotation is gone: an enum's
/// width is derived from its variants and discriminants.
#[test]
fn enum_newtype_base_must_be_an_enum() {
    let newtype = check_src("module m;\nenum A { X, Y }\nenum B(A);\n");
    assert_eq!(newtype, 0, "an enum base is a newtype");
    let non_enum = check_src("module m;\nenum S(unsigned[2]);\n");
    assert_eq!(non_enum, 1, "a non-enum base is not a derivation");
    let plain = check_src("module m;\nenum S { Idle = 0, Run = 1 }\n");
    assert_eq!(plain, 0, "the width comes from the variants");
}
