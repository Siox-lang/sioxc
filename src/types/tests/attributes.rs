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
