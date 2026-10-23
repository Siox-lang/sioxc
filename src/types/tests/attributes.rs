//! Attribute targets, values, and system attributes.

use super::*;

#[test]
/// The analogue `'ddt` attribute is rejected as Phase-2 syntax (E-P010).
fn rejects_phase2_ddt() {
    let errors = check_src("module m;\nentity E { y: Bit out, }\nimpl E {\n  y = x'ddt;\n}\n");
    assert_eq!(errors, 1);
}

#[test]
/// The digital system attributes are accepted.
fn accepts_digital_sysattrs() {
    let errors = check_src(
            "module m;\nentity E { clk: Bit in, q: Bit out, }\nimpl E {\n  if clk.rising() {\n    q = clk'old;\n  }\n}\n",
        );
    assert_eq!(errors, 0);
}

#[test]
/// An attribute applied to a target its declaration does not list is
/// rejected (E-P006).
fn attribute_on_wrong_target_is_rejected() {
    // `keep` is declared for `let, port`, not `entity`.
    let errors = check_src("module m;\nentity E { y: Bit out, }\nattr keep for E = true;\n");
    assert_eq!(errors, 1);
}

#[test]
/// An attribute on a declared target is accepted.
fn attribute_on_right_target_is_fine() {
    let errors = check_src(
            "module m;\npub attr vendor_top: Bool for entity;\nentity E { y: Bit out, }\nattr vendor_top for E = true;\n",
        );
    assert_eq!(errors, 0);
}

#[test]
/// An attribute's value is checked against its declared type (E-P007).
fn attribute_value_type_is_checked() {
    // `name` expects a string; giving it an signed is an error.
    let bad = check_src("module m;\nentity E { y: Bit out, }\nattr name for E = 5;\n");
    assert_eq!(bad, 1);
    let good = check_src("module m;\nentity E { y: Bit out, }\nattr name for E = \"dut\";\n");
    assert_eq!(good, 0);
}

/// An attribute the compiler does not implement used to pass every stage
/// and lower to an `Unknown`, which surfaced only at codegen as "no engine
/// can run this design" — naming a driver index, never the attribute. It
/// is reported at the use site now.
#[test]
fn unknown_system_attribute_is_reported() {
    let attr = |a: &str| {
        check_src(&format!(
                "module m;\nentity E {{ x: unsigned[8] in, y: unsigned[8] out, }}\nimpl E {{ y = x'{a}; }}\n"
            ))
    };
    // Every implemented attribute still passes.
    for a in ["length", "high", "low", "left", "right"] {
        assert_eq!(attr(a), 0, "`'{a}` is implemented");
    }
    assert_eq!(attr("bogus"), 1, "an invented attribute is reported");
    // The edge helpers became ClockLike methods; they are not attributes.
    assert_eq!(attr("rising"), 1, "`'rising` is not an attribute");
}

/// A character pattern names a variant of a character-valued enum, so it
/// is meaningless against anything else. Expression position has always
/// rejected that (`s == '0'` on a `State` is "no numeric identity"), but
/// when char patterns landed nothing checked pattern position — and since
/// a character has no intrinsic value the arm compared two unrelated
/// discriminants and *matched*, because `State::Idle` and `'0'` are both 0.
/// A view gives each leaf of a role a direction, and writing an `in` leaf
/// is `E-P004` when written inline. Method bodies were checked with no
/// directions at all, so the same write hidden behind a method was
/// accepted *and driven* — `fn bad(self) { self.ready = '1'; }` on a
/// Source, whose `ready` is an input, defeated the whole point of the view.
/// A binding's value is checked against the declared type (E-P007), the
/// same way wherever the binding is written.
#[test]
fn a_bound_value_must_match_the_declared_type() {
    let count = |src: &str| {
        let src = format!("{src}{VEC}");
        let mut sink = DiagnosticSink::new();
        let mut module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        crate::syntax::attributes::attach(std::slice::from_mut(&mut module), &mut sink);
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
        crate::syntax::attributes::fold_reads(std::slice::from_mut(&mut module), &mut sink);
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        sink.diagnostics()
            .iter()
            .filter(|d| d.code == Some(codes::INVALID_ATTR_VALUE_TYPE))
            .count()
    };
    const DECL: &str = "module m;\n\
            pub attr speed: integer for Pll;\n\
            pub attr vendor: string for Pll;\n\
            pub attr flag: Bool for Pll;\n\
            entity Pll { clk: Bit in, locked: Bit out }\nimpl Pll { locked = clk; }\n";

    let body = |binding: &str| {
        count(&format!(
            "{DECL}entity E {{ c: Bit in, y: Bit out }}\n\
                 impl E {{ attr {binding}; let p: Pll = {{ .clk = c }}; y = p.locked; }}\n"
        ))
    };

    assert_eq!(body("speed for p = 42"), 0, "a number satisfies it");
    assert_eq!(body("vendor for p = \"acme\""), 0, "and a string");
    assert_eq!(body("flag for p = true"), 0, "and a Bool");
    assert_eq!(
        body("speed for p = \"fast\""),
        1,
        "a string where a number belongs"
    );
    assert_eq!(
        body("vendor for p = 3"),
        1,
        "a number where a string belongs"
    );
    assert_eq!(body("flag for p = 1"), 1, "a number where a Bool belongs");
    // A declaration's default is checked the same way.
    assert_eq!(
        count("module m;\npub attr speed: integer for let = \"fast\";\n"),
        1,
        "a default of the wrong type"
    );
}

fn checked_metadata(sources: &[&str]) -> (Vec<Module>, DiagnosticSink) {
    let mut sink = DiagnosticSink::new();
    let mut modules: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(i, source)| crate::syntax::parse_module(FileId(i as u32), source, &mut sink))
        .collect();
    crate::syntax::attributes::attach(&mut modules, &mut sink);
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    crate::syntax::attributes::fold_reads(&mut modules, &mut sink);
    let typed = check(&modules, &resolved, &mut sink);
    fold_metadata_constants(&mut modules, &typed);
    (modules, sink)
}

#[test]
fn integer_metadata_materializes_defaults_bindings_and_reads() {
    let (modules, sink) = checked_metadata(&["module m;
        attr score: integer for entity, let, port = (1 + 2) * 3;
        const BASE: integer = 6;
        fn amount(n: integer) -> integer { return n * BASE; }
        entity E { p: integer out }
        attr score for E = amount(7);
        impl E {
            attr score for p = -(3 + 4);
            attr score for v = 5 * 2;
            let v: integer = 0;
            p = E'score + v'score;
        }"]);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let printed = crate::syntax::pretty::print_module(&modules[0]);
    for expected in [
        "attr score: integer for entity, let, port = 9;",
        "attr score for E = 42;",
        "attr score for v = 10;",
        "p = 42 + 10;",
    ] {
        assert!(printed.contains(expected), "missing {expected}: {printed}");
    }
    let entity = modules[0]
        .items
        .iter()
        .find_map(|item| match item {
            Item::Entity(entity) => Some(entity),
            _ => None,
        })
        .unwrap();
    assert!(matches!(&entity.attrs[0].value, Some(Expr::Int { text, .. }) if text == "42"));
    assert!(matches!(&entity.ports[0].attrs[0].value,
        Some(Expr::Unary { op: UnOp::Neg, rhs, .. })
        if matches!(rhs.as_ref(), Expr::Int { text, .. } if text == "7")));
}

#[test]
fn integer_metadata_rejects_runtime_and_invalid_constant_values() {
    for value in [
        "\"fast\"",
        "1.5",
        "missing",
        "cycle",
        "1 / 0",
        "1 << 64",
        "0x7fff_ffff_ffff_ffff + 1",
        "packed()",
        "real_value()",
    ] {
        let source = format!(
            "module m;
            attr score: integer for entity;
            const cycle: integer = cycle;
            fn packed() -> integer[8] {{ return 0; }}
            fn real_value() -> real {{ return 1.5; }}
            entity E {{}}
            attr score for E = {value};"
        );
        let (_, sink) = checked_metadata(&[&source]);
        assert_eq!(
            sink.diagnostics()
                .iter()
                .filter(|d| d.code == Some(codes::INVALID_ATTR_VALUE_TYPE))
                .count(),
            1,
            "{value}: {:?}",
            sink.diagnostics()
        );
    }
}

#[test]
fn copied_metadata_preserves_default_and_binding_site_resolution() {
    let provider = "module provider;
        const BASE: integer = 6;
        fn amount(n: integer) -> integer { return n * BASE; }
        pub attr score: integer for entity, let = BASE + 1;
        pub entity Cell { q: integer out }
        impl Cell { attr score = amount(7); q = Cell'score; }
        pub entity Plain { q: integer out }
        impl Plain { q = Plain'score; }";
    let client = "module client;
        use provider::{Cell, Plain, score};
        const BASE: integer = 90;
        fn amount(n: integer) -> integer { return n * BASE; }
        entity Top { inherited: integer out, defaulted: integer out }
        impl Top {
            let cell: Cell = {};
            let plain: Plain = {};
            inherited = cell'score;
            defaulted = plain'score;
        }";
    for sources in [[client, provider], [provider, client]] {
        let (modules, sink) = checked_metadata(&sources);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        let module = modules
            .iter()
            .find(|m| m.path.segments[0].text == "client")
            .unwrap();
        let printed = crate::syntax::pretty::print_module(module);
        assert!(printed.contains("inherited = 42;"), "{printed}");
        assert!(printed.contains("defaulted = 7;"), "{printed}");
    }
}

#[test]
fn precedence_stays_literal_only_until_bootstrap_can_evaluate_it() {
    let (_, sink) = checked_metadata(&["module m;
        attr precedence: integer for impl;
        struct S {}
        impl S { attr precedence = 20 + 20; }"]);
    assert_eq!(
        sink.diagnostics()
            .iter()
            .filter(|d| d.code == Some(codes::INVALID_ATTR_VALUE_TYPE))
            .count(),
        1
    );
}

#[test]
fn integer_metadata_keeps_kernel_boundaries_and_radix_values() {
    for (value, expected) in [
        ("0x2a", 42i64),
        ("0b101010", 42),
        ("-(5 + 3)", -8),
        ("-0x8000_0000_0000_0000", i64::MIN),
        ("0x7fff_ffff_ffff_ffff", i64::MAX),
    ] {
        let source = format!(
            "module m; attr score: integer for entity;
            entity E {{}} attr score for E = {value};"
        );
        let (modules, sink) = checked_metadata(&[&source]);
        assert!(!sink.has_errors(), "{value}: {:?}", sink.diagnostics());
        let Item::Entity(entity) = &modules[0].items[1] else {
            unreachable!()
        };
        assert_eq!(
            Checker::const_literal(entity.attrs[0].value.as_ref().unwrap()),
            Some(expected)
        );
    }
}

/// `std::attrs` declares attributes no stage reads. They resolve and apply
/// cleanly, so `#[name = "foo"]` looks like it renames the emitted entity
/// and silently does nothing.
#[test]
fn attributes_with_no_effect_are_flagged() {
    let n = |src: &str| warnings(src, codes::UNIMPLEMENTED_ATTR);
    assert_eq!(
        n("module m;\nentity E { y: unsigned[8] out, }\nimpl E { attr name = \"x\"; y = 1; }\n"),
        1,
        "`name` is reserved, not implemented"
    );
    assert_eq!(
            n("module m;\nentity E { y: unsigned[8] out, }\nimpl E { attr library = \"work\"; y = 1; }\n"),
            1,
            "so is `library`"
        );
    // The implemented ones stay quiet.
    assert_eq!(
        n("module m;\n#[test]\nentity E { y: unsigned[8] out, }\nimpl E { y = 1; }\n"),
        0,
        "`test` is acted on"
    );
}

#[test]
/// A type attribute answers for every value of its type and for `Self`; it
/// must be a shape fact, may not take a system or metadata attribute's
/// name, and a private one is read only inside its type's implementation.
fn type_attributes_are_shape_facts() {
    let base = "module m;\nstruct Q(Logic[7..0]);\n\
                impl Q {\n  pub attr ints: integer = self'high + 1;\n  ATTR\n  \
                pub fn get(v: Q) -> integer { return Self'ints + v'ints; }\n}\n\
                entity E { q: Q in, y: integer out, }\nimpl E {\n  y = READ;\n}\n";
    let codes =
        |attr: &str, read: &str| diag_codes(&base.replace("ATTR", attr).replace("READ", read));
    let fine = codes(
        "pub attr twice: integer = self'ints * 2;",
        "q'ints + q'twice",
    );
    assert!(fine.iter().all(|c| c == "None"), "{fine:?}");
    // The value, not its shape.
    let value = codes("attr bad: integer = integer(self) + 1;", "q'ints");
    assert!(
        value.contains(&format!("{:?}", Some(codes::INVALID_TYPE_ATTR))),
        "{value:?}"
    );
    // A system attribute's name, and a metadata attribute's.
    let system = codes("attr high: integer = 1;", "q'ints");
    assert!(
        system.contains(&format!("{:?}", Some(codes::INVALID_TYPE_ATTR))),
        "{system:?}"
    );
    // Private outside the type's implementation.
    let private = codes("attr hidden: integer = self'low;", "q'hidden");
    assert!(
        private.contains(&format!("{:?}", Some(codes::PRIVATE_MEMBER))),
        "{private:?}"
    );
    // An unknown one is still unknown.
    let unknown = codes("", "q'nothing");
    assert!(
        unknown.contains(&format!("{:?}", Some(codes::UNKNOWN_NAME))),
        "{unknown:?}"
    );
}

#[test]
/// `extern "intrinsic"` is `core::intrinsics`' alone (E-P036).
fn intrinsic_abi_is_reserved_for_core() {
    let codes =
        diag_codes("module m;\nextern \"intrinsic\" {\n    pub fn sqrt(x: real) -> real;\n}\n");
    assert!(
        codes.contains(&format!("{:?}", Some(codes::LANG_ITEM))),
        "{codes:?}"
    );
}
