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
    let errors = check_src("module m;\n#[keep]\nentity E { y: Bit out, }\n");
    assert_eq!(errors, 1);
}

#[test]
/// An attribute on a declared target is accepted.
fn attribute_on_right_target_is_fine() {
    let errors = check_src(
            "module m;\npub attr vendor_top: Bool for entity;\n#[vendor_top]\nentity E { y: Bit out, }\n",
        );
    assert_eq!(errors, 0);
}

#[test]
/// An attribute's value is checked against its declared type (E-P007).
fn attribute_value_type_is_checked() {
    // `name` expects a string; giving it an signed is an error.
    let bad = check_src("module m;\n#[name = 5]\nentity E { y: Bit out, }\n");
    assert_eq!(bad, 1);
    let good = check_src("module m;\n#[name = \"dut\"]\nentity E { y: Bit out, }\n");
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
/// A declared attribute with a value type needs one. `check_attr_value`
/// returned immediately when an attribute had no value, so a bare
/// `#[speed]` on `attr speed: integer` passed unexamined and was carried
/// through elaboration into `--emit tree` as `#[speed]` — an attribute a
/// synthesis backend reads with no number in it. `Bool` is exempt: a bare
/// flag reads as `true`, as the marker attributes do.
#[test]
fn a_value_typed_attribute_needs_a_value() {
    let count = |src: &str| {
        let src = format!("{src}{VEC}");
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
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

    let body = |attr: &str| {
        count(&format!(
            "{DECL}entity E {{ c: Bit in, y: Bit out }}\n\
                 impl E {{ {attr} let p: Pll = {{ .clk = c }}; y = p.locked; }}\n"
        ))
    };

    assert_eq!(body("#[speed]"), 1, "an integer attribute needs a number");
    assert_eq!(body("#[vendor]"), 1, "a string attribute needs a string");

    // The forms that carry a value are unaffected.
    assert_eq!(body("#[speed = 42]"), 0, "a number satisfies it");
    assert_eq!(body("#[vendor = \"acme\"]"), 0, "and a string");
    assert_eq!(body("#[flag = Bool::true]"), 0, "and an explicit Bool");

    // A bare Bool flag stays legal: it reads as `true`, like `#[test]`.
    assert_eq!(body("#[flag]"), 0, "a bare Bool flag is still a flag");

    // The wrong *type* was already reported, and still is.
    assert_eq!(
        body("#[speed = \"fast\"]"),
        1,
        "a string where a number belongs"
    );
}

/// `std::attrs` declares attributes no stage reads. They resolve and apply
/// cleanly, so `#[name = "foo"]` looks like it renames the emitted entity
/// and silently does nothing.
#[test]
fn attributes_with_no_effect_are_flagged() {
    let n = |src: &str| warnings(src, codes::UNIMPLEMENTED_ATTR);
    assert_eq!(
        n("module m;\n#[name = \"x\"]\nentity E { y: unsigned[8] out, }\nimpl E { y = 1; }\n"),
        1,
        "`name` is reserved, not implemented"
    );
    assert_eq!(
            n("module m;\n#[library = \"work\"]\nentity E { y: unsigned[8] out, }\nimpl E { y = 1; }\n"),
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
