//! `match` exhaustiveness, patterns, and unreachable arms.

use super::*;

/// Exhaustiveness was only ever computed over enum variants, so a hole in
/// a numeric match was silent. Warnings do not count as errors here, so
/// these assert on the diagnostic text.
#[test]
fn a_numeric_match_reports_the_range_it_leaves_out() {
    let cases = [
        ("0 => 5", "`1..3`"),
        ("0 | 1 => 5", "`2..3`"),
        ("0..2 => 5", "`3`"),
        ("1..3 => 5", "`0`"),
        ("0 => 5, 2..3 => 7", "`1`"),
    ];
    for (arms, expected) in cases {
        let src = format!(
            "module m;\nentity F {{ s: unsigned[2] in, z: unsigned[8] out }}\n\
                 impl F {{ z = match s {{ {arms} }}; }}\n{VEC}"
        );
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        let found = sink
            .diagnostics()
            .iter()
            .any(|d| d.message.contains("non-exhaustive") && d.message.contains(expected));
        assert!(
            found,
            "`{arms}` should report {expected}: {:?}",
            sink.diagnostics()
        );
    }
}

/// Covering the domain by alternation, by range, or value by value is as
/// exhaustive as a `_`, and must not warn.
#[test]
fn a_numeric_match_covering_its_domain_is_quiet() {
    for arms in [
        "0 | 1 => 5, 2..3 => 7",
        "0..3 => 5",
        "0 => 5, 1 => 6, 2 => 7, 3 => 8",
        "0 => 5, _ => 7",
    ] {
        let src = format!(
            "module m;\nentity F {{ s: unsigned[2] in, z: unsigned[8] out }}\n\
                 impl F {{ z = match s {{ {arms} }}; }}\n{VEC}"
        );
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        let warned = sink
            .diagnostics()
            .iter()
            .any(|d| d.message.contains("non-exhaustive"));
        assert!(
            !warned,
            "`{arms}` covers its domain: {:?}",
            sink.diagnostics()
        );
    }
}

#[test]
/// Unreachable arms warn in a match expression, as in a match statement.
fn unreachable_arms_warn_in_a_match_expression() {
    // The two match forms share `MatchArm` but not the code walking it,
    // so this was statement-only.
    let base = "module m;\nenum State { Idle, Run, Done }\nentity E { y: Bit out, }\nimpl E {\n  let s: State;\n  y = match s { ARMS };\n}\n";
    assert_eq!(
        warnings(
            &base.replace("ARMS", "State::Idle => '0', State::Idle => '1', _ => '0'"),
            codes::UNREACHABLE_MATCH_ARM
        ),
        1
    );
    assert_eq!(
        warnings(
            &base.replace("ARMS", "State::Idle => '0', _ => '1'"),
            codes::UNREACHABLE_MATCH_ARM
        ),
        0
    );
}

#[test]
/// An arm fully covered by earlier arms warns (W-P006).
fn unreachable_match_arms_warn() {
    let base = "module m;\nenum State { Idle, Run, Done }\nentity E { y: Bit out, }\nimpl E {\n  let s: State;\n  match s {\n    ARMS\n  }\n}\n";
    // An arm after `_` is unreachable.
    assert_eq!(
        warnings(
            &base.replace("ARMS", "_ => { y = '0'; } State::Idle => { y = '1'; }"),
            codes::UNREACHABLE_MATCH_ARM
        ),
        1
    );
    // A repeated variant is unreachable.
    assert_eq!(
        warnings(
            &base.replace(
                "ARMS",
                "State::Idle => { y = '0'; } State::Idle => { y = '1'; } _ => { y = '0'; }"
            ),
            codes::UNREACHABLE_MATCH_ARM
        ),
        1
    );
    // A normal, distinct set of arms is fine.
    assert_eq!(
        warnings(
            &base.replace("ARMS", "State::Idle => { y = '0'; } _ => { y = '1'; }"),
            codes::UNREACHABLE_MATCH_ARM
        ),
        0
    );
}

#[test]
/// A match not covering every enum variant warns (W-P007).
fn non_exhaustive_enum_match_warns() {
    let base = "module m;\nenum State { Idle, Run, Done }\nentity E { y: Bit out, }\nimpl E {\n  let s: State;\n  match s {\n    ARMS\n  }\n}\n";
    // Missing `Done` and no `_` -> one warning.
    assert_eq!(
        warnings(
            &base.replace(
                "ARMS",
                "State::Idle => { y = '0'; } State::Run => { y = '1'; }"
            ),
            codes::NON_EXHAUSTIVE_MATCH
        ),
        1
    );
    // A `_` wildcard is exhaustive.
    assert_eq!(
        warnings(
            &base.replace("ARMS", "State::Idle => { y = '0'; } _ => { y = '1'; }"),
            codes::NON_EXHAUSTIVE_MATCH
        ),
        0
    );
    // All variants covered is exhaustive.
    assert_eq!(
            warnings(
                &base.replace(
                    "ARMS",
                    "State::Idle => { y = '0'; } State::Run => { y = '1'; } State::Done => { y = '0'; }"
                ),
                codes::NON_EXHAUSTIVE_MATCH
            ),
            0
        );
}

/// Exhaustiveness was only ever checked on match *statements*. In
/// expression position a missing variant means there is no value to
/// produce, yet it drew no diagnostic at all.
#[test]
fn match_expression_exhaustiveness_is_checked() {
    let src = |arms: &str| {
        format!("module m;\nenum Base {{ A, B, C }}\nentity E {{ sel: Base in, y: unsigned[8] out, }}\nimpl E {{ y = match sel {{ {arms} }}; }}\n")
    };
    assert_eq!(
        warnings(
            &src("Base::A => 1, Base::B => 2"),
            codes::NON_EXHAUSTIVE_MATCH
        ),
        1,
        "a missing variant in expression position"
    );
    assert_eq!(
        warnings(
            &src("Base::A => 1, Base::B => 2, Base::C => 3"),
            codes::NON_EXHAUSTIVE_MATCH
        ),
        0,
        "all variants named"
    );
    assert_eq!(
        warnings(&src("Base::A => 1, _ => 9"), codes::NON_EXHAUSTIVE_MATCH),
        0,
        "a wildcard covers the rest"
    );
}

#[test]
/// Every arm of a match expression must produce the same type.
fn match_expression_checks_every_arm_type() {
    let bad_assignment = check_src(
        "module m;\n\
             enum Select { Number, Other }\n\
             entity E { select: Select in, logic: Logic in, y: unsigned[8] out }\n\
             impl E {\n\
               y = match select { Select::Number => 1, Select::Other => logic };\n\
             }\n",
    );
    assert_eq!(
        bad_assignment, 1,
        "a later match arm cannot bypass assignment compatibility"
    );

    let bad_return = check_src(
        "module m;\n\
             enum Select { Number, Other }\n\
             fn choose(select: Select, logic: Logic) -> unsigned[8] {\n\
               return match select { Select::Number => 1, Select::Other => logic };\n\
             }\n",
    );
    assert_eq!(
        bad_return, 1,
        "the return context applies to every match arm"
    );

    let good = check_src(
        "module m;\n\
             enum Select { One, Two }\n\
             fn choose(select: Select) -> unsigned[8] {\n\
               return match select { Select::One => 1, Select::Two => 2 };\n\
             }\n",
    );
    assert_eq!(good, 0, "compatible match arms remain valid");
}

/// A bit-string prefix the compiler cannot evaluate is a type error, and
/// must be one in pattern position too. The pattern parser recognized
/// `"x" | "o"` where expression position accepts any letter, so the same
/// prefix produced a clean diagnostic in `y = d"42";` and a raw parse
/// error in `match s { d"42" => .. }`. `check_src` asserts the source
/// parses, which is the property that regressed.
#[test]
fn an_unevaluable_prefix_is_a_diagnostic_in_pattern_position_too() {
    let pattern = check_src(
        "module m;\nentity E { s: unsigned[8] in, y: unsigned[8] out }\n\
             impl E { match s { d\"42\" => y = 1, _ => y = 0, } }\n",
    );
    assert_eq!(pattern, 1, "reported, not a parse failure");
    let expression =
        check_src("module m;\nentity E { y: unsigned[8] out }\nimpl E { y = d\"42\"; }\n");
    assert_eq!(expression, 1, "and expression position is unchanged");

    // The prefixes the table does list still work in both positions.
    let ok = check_src(
        "module m;\nentity E { s: unsigned[8] in, y: unsigned[8] out }\n\
             impl E { match s { x\"A?\" => y = 1, o\"7?\" => y = 2, _ => y = x\"0F\", } }\n",
    );
    assert_eq!(ok, 0, "`x` and `o` are in RADIX_PREFIXES");
}

#[test]
/// A character pattern requires a character-valued enum scrutinee.
fn a_character_pattern_needs_a_character_valued_enum() {
    let count = |src: &str| {
        let src = format!("{src}{VEC}");
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        sink.diagnostics()
            .iter()
            .filter(|d| {
                d.code == Some(codes::INVALID_PATTERN) || d.code == Some(codes::TYPE_MISMATCH)
            })
            .count()
    };
    const STATE: &str = "module m;\nenum State { Idle, Run }\n";

    assert_eq!(
        count(&format!(
            "{STATE}entity E {{ s: State in, a: unsigned[8] out }}\n\
                 impl E {{ match s {{ '0' => a = 1, _ => a = 2, }} }}\n"
        )),
        1,
        "a character against a name-valued enum"
    );
    assert_eq!(
        count(
            "module m;\nentity E { n: unsigned[4] in, a: unsigned[8] out }\n\
                 impl E { match n { '0' => a = 1, _ => a = 2, } }\n"
        ),
        1,
        "a character against a numeric scrutinee"
    );
    assert_eq!(
        count(
            "module m;\nentity E { l: Logic in, a: unsigned[8] out }\n\
                 impl E { match l { 'Q' => a = 1, _ => a = 2, } }\n"
        ),
        1,
        "a character that is not one of the enum's variants"
    );
    // Inside an alternation, where the pattern walk has to recurse.
    assert_eq!(
        count(&format!(
            "{STATE}entity E {{ s: State in, a: unsigned[8] out }}\n\
                 impl E {{ match s {{ State::Idle | '0' => a = 1, _ => a = 2, }} }}\n"
        )),
        1,
        "and one hidden in an or-pattern"
    );

    // The spelling that is meant to work.
    assert_eq!(
        count(
            "module m;\nentity E { l: Logic in, a: unsigned[8] out }\n\
                 impl E { match l { '0' | '1' => a = 1, 'Z' => a = 2, _ => a = 3, } }\n"
        ),
        0,
        "characters against `Logic`, alternation included"
    );
}

#[test]
/// Match patterns must lie inside the scrutinee's domain.
fn match_patterns_must_belong_to_the_scrutinee_domain() {
    let enums = "module m;\nenum Left { Zero, One }\nenum Right { Zero, One }\n";
    assert_eq!(
        check_src(&format!(
            "{enums}#[test] entity T {{}}\nimpl T {{\n\
                 let value: Left = Left::Zero;\n\
                 match value {{ Right::Zero => {{}}, _ => {{}}, }}\n\
                 }}\n"
        )),
        1,
        "equal discriminants from different enums are not interchangeable"
    );
    assert_eq!(
        check_src(&format!(
            "{enums}#[test] entity T {{}}\nimpl T {{\n\
                 let value: unsigned[2] = 0;\n\
                 match value {{ Left::Zero => {{}}, _ => {{}}, }}\n\
                 }}\n"
        )),
        1,
        "an enum variant cannot pattern-match a numeric value"
    );
    assert_eq!(
        check_src(&format!(
            "{enums}#[test] entity T {{}}\nimpl T {{\n\
                 let value: Left = Left::Zero;\n\
                 match value {{ 0 => {{}}, _ => {{}}, }}\n\
                 }}\n"
        )),
        1,
        "an integer pattern cannot inspect an enum discriminant"
    );
    assert_eq!(
        check_src(&format!(
            "{enums}#[test] entity T {{}}\nimpl T {{\n\
                 let value: Left = Left::Zero;\n\
                 match value {{ \"--\" => {{}}, _ => {{}}, }}\n\
                 }}\n"
        )),
        1,
        "a bit mask cannot inspect an enum discriminant"
    );
    assert_eq!(
        check_src(&format!(
            "{enums}#[test] entity T {{}}\nimpl T {{\n\
                 let value: Left = Left::Zero;\n\
                 match value {{ Left::Zero | Left::One => {{}}, }}\n\
                 }}\n"
        )),
        0,
        "matching alternatives from the scrutinee enum remains valid"
    );
}

/// A bare name in pattern position lowered to a wildcard, because
/// `arm_match_cond` treats any pattern it cannot lower as "matches
/// anything". So `Idle => ...` (instead of `State::Idle`) silently
/// swallowed the entire match, `_` arms included, with no diagnostic.
#[test]
fn bare_name_is_not_a_pattern() {
    let m = |arms: &str| {
        format!("module m;\nenum State {{ Idle, Run }}\nentity E {{ v: unsigned[8] in, r: unsigned[8] out, }}\nimpl E {{ match v {{ {arms} }} }}\n")
    };
    assert_eq!(
        warnings(
            &m("Idle => { r = 1; } _ => { r = 9; }"),
            codes::INVALID_PATTERN
        ),
        1,
        "a bare name is rejected, not treated as a catch-all"
    );
    // The forms the lowering does honour stay clean.
    assert_eq!(
        warnings(
            &m("State::Idle => { r = 1; } _ => { r = 9; }"),
            codes::INVALID_PATTERN
        ),
        0,
        "a qualified variant is valid"
    );
    assert_eq!(
        warnings(
            &m("0..9 => { r = 1; } _ => { r = 9; }"),
            codes::INVALID_PATTERN
        ),
        0,
        "ranges are valid"
    );
    // `|` alternatives are checked through, not just the outer pattern.
    assert_eq!(
        warnings(
            &m("State::Idle | Run => { r = 1; } _ => { r = 9; }"),
            codes::INVALID_PATTERN
        ),
        1,
        "a bad alternative inside `|` is caught"
    );
}

/// A bit pattern whose text is not a well-formed mask was invisible: IR
/// lowering turned it into a wildcard (swallowing the arm and every arm
/// after it) while the runner never matched it — silently wrong, and
/// differently wrong per engine.
#[test]
fn malformed_bit_pattern_is_rejected() {
    let m = |arms: &str| {
        format!("module m;\nentity E {{ v: unsigned[8] in, r: unsigned[8] out, }}\nimpl E {{ match v {{ {arms} _ => {{ r = 9; }} }} }}\n")
    };
    for bad in ["\"2\"", "x\"G\"", "o\"8\""] {
        assert_eq!(
            warnings(
                &m(&format!("{bad} => {{ r = 1; }}")),
                codes::INVALID_PATTERN
            ),
            1,
            "{bad} should be rejected"
        );
    }
    for good in ["\"01--\"", "x\"A?\"", "o\"7?\"", "\"0000_11--\""] {
        assert_eq!(
            warnings(
                &m(&format!("{good} => {{ r = 1; }}")),
                codes::INVALID_PATTERN
            ),
            0,
            "{good} is a valid pattern"
        );
    }
}

/// A range arm wholly inside an earlier one can never match (first match
/// wins) — the enum and wildcard cases were caught, ranges were not.
#[test]
fn unreachable_range_arm_warns_only_when_fully_covered() {
    let m = |arms: &str| {
        format!("module m;\nentity E {{ v: unsigned[8] in, r: unsigned[8] out, }}\nimpl E {{ match v {{ {arms} _ => {{ r = 9; }} }} }}\n")
    };
    assert_eq!(
        warnings(
            &m("0..9 => { r = 1; } 2..5 => { r = 2; }"),
            codes::UNREACHABLE_MATCH_ARM
        ),
        1,
        "fully covered"
    );
    assert_eq!(
        warnings(
            &m("0..9 => { r = 1; } 5 => { r = 2; }"),
            codes::UNREACHABLE_MATCH_ARM
        ),
        1,
        "a literal inside an earlier range"
    );
    assert_eq!(
        warnings(
            &m("0..9 => { r = 1; } 5..15 => { r = 2; }"),
            codes::UNREACHABLE_MATCH_ARM
        ),
        0,
        "a partial overlap is still reachable"
    );
    assert_eq!(
        warnings(
            &m("0..9 => { r = 1; } 10..20 => { r = 2; }"),
            codes::UNREACHABLE_MATCH_ARM
        ),
        0,
        "disjoint ranges"
    );
}
