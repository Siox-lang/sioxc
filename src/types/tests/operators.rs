//! Operators, comparisons, conditions, and literal typing.

use super::*;

#[test]
/// Numeric separators and based indices are checked at the literal's full
/// width rather than a truncated one.
fn numeric_separators_and_based_type_indices_are_checked_at_full_width() {
    let errors = check_src(
        "module m;\n\
             entity E {\n\
               a: unsigned[1_28] in,\n\
               b: Logic[0x3..0b0] in,\n\
               y: Logic out,\n\
             }\n\
             impl E { y = a[0x7f] and b[0b11]; }\n",
    );
    assert_eq!(
        errors, 0,
        "literal spelling must not turn valid widths or indices into unknown widths"
    );
}

#[test]
/// Comparing a logic value against an integer literal warns (W-P008), since
/// the comparison cannot mean what it looks like.
fn suspicious_logic_compare_warns_on_integer_literal() {
    let warns = |src: &str| diag_codes(src).iter().any(|c| c.contains("W-P008"));
    // Bit / Logic / enum vs a bare integer literal → W-P008.
    assert!(
            warns("module m;\nentity E { b: Bit in, y: Bit out, }\nimpl E { y = if b == 1 { '1' } else { '0' }; }\n"),
            "Bit == 1 should warn"
        );
    assert!(
            warns("module m;\nenum State { Idle, Run }\nentity E { y: Bit out, }\nimpl E { let s: State; y = if s == 0 { '1' } else { '0' }; }\n"),
            "enum == 0 should warn"
        );
    // Numeric vector vs integer, and Bit vs a value literal → no warning.
    assert!(
            !warns("module m;\nentity E { a: unsigned[8] in, y: Bit out, }\nimpl E { y = if a == 5 { '1' } else { '0' }; }\n"),
            "unsigned == 5 must not warn"
        );
    assert!(
            !warns("module m;\nentity E { b: Bit in, y: Bit out, }\nimpl E { y = if b == '1' { '1' } else { '0' }; }\n"),
            "Bit == '1' must not warn"
        );
}

#[test]
/// A decimal literal needs a `real` context or an explicit conversion.
fn decimal_literals_require_real_context_or_explicit_conversion() {
    let errors = check_src(
        "module m;\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let i: integer = 1.5;\n\
               let bits: unsigned[8] = 1.5;\n\
               let r: real = 1.5;\n\
               let converted: integer = integer(r);\n\
               y = '0';\n\
             }\n",
    );
    assert_eq!(
        errors, 2,
        "a decimal literal is real and narrowing remains explicit"
    );
}

#[test]
/// Comparisons and value branches require compatible operand types.
fn comparisons_and_value_branches_need_compatible_types() {
    let tb = |body: &str| format!("module m;\n#[test] entity T {{}}\nimpl T {{ {body} }}\n");
    assert_eq!(
        check_src(&tb(
            "let r: real = 1.5; let text: Char[1] = \"x\"; let bad: Bool = r == text;"
        )),
        1,
        "comparison domains"
    );
    assert_eq!(
        check_src(&tb("if if true { true } else { 1 } {}")),
        1,
        "if-expression branches"
    );
    assert_eq!(
        check_src(&tb("let bad: Bool = if true { true } else { 1 };")),
        1,
        "an enclosing assignment does not duplicate the branch error"
    );
    assert_eq!(
        check_src(&tb(
            "if match true { Bool::true => true, Bool::false => 1 } {}"
        )),
        1,
        "match-expression arms"
    );
    assert_eq!(
        check_src(
            "module m;\nenum State { Idle, Run }\n#[test] entity T {}\n\
                 impl T { let state: State = State::Idle; let bad: Bool = state < 1; }\n"
        ),
        1,
        "enum ordering still requires a matching `<=>` implementation"
    );
    assert_eq!(
            check_src(&tb(
                "let r: real = 1.5; let promoted: real = if true { 1 } else { 1.5 }; let compared: Bool = 1 < r;"
            )),
            0,
            "integer-to-real promotion stays compatible"
        );
    assert_eq!(
        check_src(&tb("let bad: integer = (if true { 1 } else { 1.5 }) + 1;")),
        1,
        "an `if` join retains real promotion when nested"
    );
    assert_eq!(
        check_src(&tb(
            "let bad: integer = (match true { Bool::true => 1, Bool::false => 1.5 }) + 1;"
        )),
        1,
        "a match join retains real promotion when nested"
    );
    assert_eq!(
        check_src(&tb(
            "let short: Char[4] = \"siox\"; let different: Bool = short != \"sioxc\";"
        )),
        0,
        "array comparison does not require assignment-compatible lengths"
    );
}

#[test]
/// Built-in arithmetic requires numeric operands.
fn intrinsic_arithmetic_requires_numeric_operands() {
    let errors = check_src(
        "module m;\n\
             #[test] entity T {}\n\
             impl T {\n\
               let r: real = 1.5;\n\
               let text: Char[1] = \"x\";\n\
               let bad_add: real = r + text;\n\
               let bad_sub: Char = 'a' - 'b';\n\
               let bad_shift: integer = 1 << r;\n\
               let promoted: real = 1 + r;\n\
               let bits: unsigned[8] = 3;\n\
               let incremented: unsigned[8] = bits + 1;\n\
             }\n",
    );
    assert_eq!(
        errors, 3,
        "intrinsic arithmetic has numeric operand domains"
    );
}

#[test]
/// A string literal in a non-string position gets a targeted hint rather
/// than a bare mismatch.
fn string_literal_gets_a_targeted_hint() {
    let sp = crate::diag::Span::new(FileId(0), 0..1);
    let s = |t: &str| Expr::StrLit {
        text: t.to_string(),
        span: sp,
    };
    // A named scalar points at the character literal.
    let h = strlit_help(&Ty::Named(crate::resolve::DefId(0)), &s("0")).unwrap();
    assert!(h.contains("'0'"), "{h}");
    // A bit vector points at the bit-string literal.
    let h = strlit_help(
        &Ty::Array {
            elem: Box::new(Ty::Named(crate::resolve::DefId(0))),
            family: Some("unsigned".to_string()),
            len: 4,
        },
        &s("0101"),
    )
    .unwrap();
    assert!(h.contains("b\"0101\""), "{h}");
    // Assigning a string to a Char array is correct — no hint.
    let str_ty = Ty::Array {
        elem: Box::new(Ty::Char),
        len: 2,
        family: None,
    };
    assert!(strlit_help(&str_ty, &s("hi")).is_none());
}

#[test]
/// A bare logic value is not a condition: `Logic` opts out of `Condition`.
fn bare_logic_condition_is_rejected() {
    let errors = check_src(
            "module m;\nentity E { rst: Logic in, y: Bit out, }\nimpl E {\n  if rst {\n    y = '0';\n  }\n}\n",
        );
    assert_eq!(errors, 1);
}

#[test]
/// A compared logic value is a condition, since the comparison yields a
/// boolean.
fn compared_logic_and_bit_conditions_are_fine() {
    // `rst == '1'` is a comparison (-> Bool); `en` is a Bit. Both valid.
    let errors = check_src(
            "module m;\nentity E { rst: Logic in, en: Bit in, y: Bit out, }\nimpl E {\n  if rst == '1' {\n    y = '0';\n  }\n  if en {\n    y = '1';\n  }\n}\n",
        );
    assert_eq!(errors, 0);
}

#[test]
/// Integer and logic literals adopt the type of their context.
fn integer_and_logic_literals_are_polymorphic() {
    // signed literal -> any unsigned; '1' -> Bit or Logic. No conversions needed.
    let errors = check_src(
            "module m;\nentity E { count: unsigned[8] out, q: Bit out, clk: Bit out, }\nimpl E {\n  let value: unsigned[8] = 0;\n  count = value;\n  q = '1';\n  clk = '0';\n}\n",
        );
    assert_eq!(errors, 0);
}

#[test]
/// A nominal array newtype forwards a blanket array operator whose bound it
/// satisfies.
fn nominal_array_newtype_forwards_matching_blanket_array_operator() {
    let errors = check_src(
        "module m;\n\
             impl<T: Operator<\"and\", T, T>> Operator<\"and\", T, T> for T[] {\n\
               fn apply(self, rhs: T[]) -> T[] { return self and rhs; }\n\
             }\n\
             struct Flags(Bit[]);\n\
             entity E { a: Flags[4] in, b: Flags[4] in, y: Flags[4] out }\n\
             impl E { y = a and b; }\n",
    );
    assert_eq!(errors, 0);
}

#[test]
/// It does not forward one whose bound it fails.
fn nominal_array_newtype_does_not_forward_unsatisfied_array_operator() {
    let errors = check_src(
        "module m;\n\
             enum Cell { Off, On }\n\
             impl<T: Operator<\"and\", T, T>> Operator<\"and\", T, T> for T[] {\n\
               fn apply(self, rhs: T[]) -> T[] { return self and rhs; }\n\
             }\n\
             struct Cells(Cell[]);\n\
             entity E { a: Cells[4] in, b: Cells[4] in, y: Cells[4] out }\n\
             impl E { y = a and b; }\n",
    );
    assert_eq!(errors, 1);
}

#[test]
/// A blanket array operator that cannot lower is rejected rather than
/// accepted and dropped.
fn unsupported_blanket_array_operator_is_rejected_until_it_can_lower() {
    let blanket = |op: &str| {
        format!(
            "module m;\n\
                 #[precedence = 35]\n\
                 impl<T: Operator<\"{op}\", T, T>> Operator<\"{op}\", T, T> for T[] {{\n\
                   fn apply(self, rhs: T[]) -> T[] {{ return self; }}\n\
                 }}\n"
        )
    };
    // The whole logic family lowers element-wise now, so `xor` is
    // accepted alongside `and`/`or` rather than held back.
    assert_eq!(check_src(&blanket("xor")), 0);
    assert_eq!(check_src(&blanket("nand")), 0);
    // Arithmetic has no element-wise lowering, and saying so beats
    // accepting an impl nothing would call.
    assert_eq!(check_src(&blanket("+")), 1);
}

#[test]
/// Operators on user types require a matching impl.
fn operators_on_user_types_need_an_impl() {
    let base = "module m;\nstruct V { a: Bit }\nOPIMPL\nentity E { p: V in, q: V in, y: Bit out, }\nimpl E {\n  let r: V = p + q;\n  y = '0';\n}\n";
    // Without an impl, `+` on a struct is rejected.
    assert_eq!(check_src(&base.replace("OPIMPL\n", "")), 1);
    // With `impl Operator<"+", V, V> for V`, it is accepted.
    assert_eq!(
            check_src(&base.replace(
                "OPIMPL",
                "impl Operator<\"+\", V, V> for V {\n  fn apply(self, rhs: V) -> V {\n    return self;\n  }\n}"
            )),
            0
        );
}

#[test]
/// An operator overload must match its declared input type.
fn operator_overloads_match_the_declared_input_type() {
    let header = "module m;\nstruct Left { a: Bit }\nstruct Right { b: Bit }\n";
    let explicit = "impl Operator<\"+\", Right, Left> for Left {\n\
                          fn apply(self, rhs: Right) -> Left { return self; }\n\
                        }\n";
    assert_eq!(
        check_src(&format!(
            "{header}{explicit}entity E {{ a: Left in, b: Right in, }}\n\
                 impl E {{ let good: Left = a + b; }}\n"
        )),
        0,
        "the declared right-hand type selects the overload"
    );
    assert_eq!(
        check_src(&format!(
            "{header}{explicit}entity E {{ a: Left in, }}\n\
                 impl E {{ let bad: Left = a + a; }}\n"
        )),
        1,
        "an impl for another input type is not a wildcard"
    );

    let self_typed = "impl Operator<\"+\", Self, Self> for Left {\n\
                            fn apply(self, rhs: Self) -> Self { return self; }\n\
                          }\n";
    assert_eq!(
        check_src(&format!(
            "{header}{self_typed}entity E {{ a: Left in, b: Right in, }}\n\
                 impl E {{ let bad: Left = a + b; }}\n"
        )),
        1,
        "`Self` means the impl owner rather than any input type"
    );
}

#[test]
/// The six comparisons derive from the three-way `<=>`, so struct equality
/// follows from one impl.
fn struct_equality_is_derived_from_three_way_comparison() {
    let base = |operator: &str| {
        format!(
            "module m;\nstruct V {{ a: Bit }}\n{operator}\n\
                 entity E {{ p: V in, q: V in, }}\n\
                 impl E {{ let equal: Bool = p == q; }}\n"
        )
    };
    assert_eq!(
        check_src(&base("")),
        1,
        "struct equality needs an operator contract"
    );
    assert_eq!(
        check_src(&base(
            "impl Operator<\"<=>\", V, Ordering> for V {\n\
                   fn apply(self, rhs: V) -> Ordering { return Ordering::Equal; }\n\
                 }"
        )),
        0,
        "one `<=>` implementation derives equality"
    );
}

#[test]
/// Suffix traits define the literal forms and disambiguate between them.
fn suffix_traits_define_and_disambiguate_literals() {
    let time = "struct Time { fs: unsigned[48] }\nimpl Suffix<\"s\", integer> for Time {}\n";
    // A Suffix impl's symbol defines the literal's type: Time = 5s passes.
    assert_eq!(
            check_src(&format!(
                "module m;\n{time}entity E {{ y: Bit out, }}\nimpl E {{\n  let t: Time = 5s;\n  y = '0';\n}}\n"
            )),
            0
        );
    // Two types defining the same suffix is an ambiguity error (the
    // cascading init mismatch is separate).
    let score = "struct Score { p: unsigned[8] }\nimpl Suffix<\"s\", integer> for Score {}\n";
    let src = format!(
            "module m;\n{time}{score}entity E {{ y: Bit out, }}\nimpl E {{\n  let t: Time = 5s;\n  y = '0';\n}}\n"
        );
    assert_eq!(warnings(&src, codes::UNKNOWN_NAME), 1);
}

#[test]
/// Suffix and radix bit-string literals are type-checked.
fn suffix_and_bitstring_literals_are_checked() {
    // Known unit suffixes and valid bit-strings pass.
    assert_eq!(
            check_src(
                "module m;\nentity E { y: unsigned[8] out, }\nimpl E {\n  let t: integer = 10ns;\n  let f: integer = 100MHz;\n  y = x\"AB\";\n}\n"
            ),
            0
        );
    // An unknown suffix is an error.
    assert_eq!(
            check_src("module m;\nentity E { y: Bit out, }\nimpl E {\n  let c: integer = 5i;\n  y = '0';\n}\n"),
            1
        );
    // Bad digits for the base are an error (`G` is not a hex digit).
    assert_eq!(
        check_src("module m;\nentity E { y: unsigned[8] out, }\nimpl E {\n  y = x\"1G\";\n}\n"),
        1
    );
    // An unknown prefix (no `impl Prefix` declares `q`) is an error.
    assert_eq!(
        check_src("module m;\nentity E { y: unsigned[8] out, }\nimpl E {\n  y = q\"1010\";\n}\n"),
        1
    );
}

#[test]
/// A user type opts into conditions by implementing `Boolean`.
fn user_type_opts_into_condition_via_boolean() {
    // Without an `impl Boolean for State`, `if state` is rejected.
    let without = check_src(
            "module m;\nenum State { Idle, Run }\nentity E { y: Bit out, }\nimpl E {\n  let state: State;\n  if state {\n    y = '1';\n  }\n}\n",
        );
    assert_eq!(without, 1);

    // With it, the enum becomes usable as a condition.
    let with = check_src(
            "module m;\nenum State { Idle, Run }\nimpl Boolean for State {\n  fn as_bool(self) -> Bool {\n    match self {\n      State::Idle => return false,\n      _ => return true,\n    }\n  }\n}\nentity E { y: Bit out, }\nimpl E {\n  let state: State;\n  if state {\n    y = '1';\n  }\n}\n",
        );
    assert_eq!(with, 0);
}

#[test]
/// A character literal defaults to `Char` but adopts an annotated enum type.
fn char_literal_defaults_to_char_but_takes_annotated_type() {
    // Bare: '0' is a Char.  Annotated / if-expr context: it takes the
    // target type (Bit/Logic), including through an if-expression.
    assert_eq!(
        check_src("module m;\nentity E { y: Bit out, }\nimpl E { y = '0'; }\n"),
        0,
        "'0' assigns to a Bit output"
    );
    assert_eq!(
        check_src("module m;\nentity E { y: Logic out, }\nimpl E { y = '1'; }\n"),
        0,
        "'1' assigns to a Logic output"
    );
    assert_eq!(
            check_src("module m;\nentity E { c: Bit in, y: Bit out, }\nimpl E { y = if c { '1' } else { '0' }; }\n"),
            0,
            "char literals in if-expr branches read through the Bit target"
        );
}

#[test]
/// Literals default to their core types when no context overrides them.
fn literals_default_to_their_core_types() {
    let ty = |src: &str| {
        let mut sink = DiagnosticSink::new();
        let m = crate::syntax::parse_module(FileId(0), src, &mut sink);
        let r = crate::resolve::resolve(std::slice::from_ref(&m), &mut sink);
        let c = Checker::new(&mut sink, &r, std::slice::from_ref(&m));
        c.type_of(&value_expr(&m), &HashMap::new())
    };
    // helper: the value in `impl E { y = <value>; }`
    /// The value expression of the first item, for tests that inspect one
    /// literal's inferred type.
    fn value_expr(m: &crate::syntax::Module) -> Expr {
        for item in &m.items {
            if let Item::Impl(im) = item {
                for it in &im.items {
                    if let ImplItem::Stmt(Stmt::Assign { value, .. }) = it {
                        return value.clone();
                    }
                }
            }
        }
        panic!("no assignment");
    }
    assert!(matches!(ty("module m;\nimpl E { y = 42; }\n"), Ty::Integer));
    assert!(matches!(ty("module m;\nimpl E { y = 3.14; }\n"), Ty::Real));
    assert!(matches!(ty("module m;\nimpl E { y = '0'; }\n"), Ty::Char));
    assert!(matches!(
        ty("module m;\nimpl E { y = \"abc\"; }\n"),
        Ty::Array { .. }
    ));
    // `true`/`false` desugar to `Bool::true`/`Bool::false`, so std's `Bool`
    // The enum must be in scope for them to resolve as a named type.
    assert!(matches!(
        ty("module m;\nenum Bool { false, true }\nimpl E { y = true; }\n"),
        Ty::Named(_)
    ));
}

#[test]
/// Boolean operators reject types that are not bit-shaped.
fn boolean_ops_reject_non_bit_types() {
    // `and`/`or`/`not` are boolean-per-bit: bit-derived / Boolean only.
    assert_eq!(
            check_src("module m;\nentity E { a: real in, b: real in, y: real out, }\nimpl E { y = a and b; }\n"),
            1,
            "`and` on real is rejected"
        );
    assert_eq!(
            check_src("module m;\nentity E { a: unsigned[8] in, b: unsigned[8] in, y: unsigned[8] out, }\nimpl E { y = a and b; }\n"),
            0,
            "`and` on a bit array is fine (per-bit, returns the array)"
        );
    // integer is a number, not bits — no boolean operators on it.
    assert_eq!(
            check_src("module m;\nentity E { a: integer in, b: integer in, y: integer out, }\nimpl E { y = a and b; }\n"),
            1,
            "`and` on integer variables is rejected"
        );
    // ...but a literal mask coerces to the bit operand's width.
    assert_eq!(
            check_src("module m;\nentity E { a: unsigned[8] in, y: unsigned[8] out, }\nimpl E { y = a and 15; }\n"),
            0,
            "`b and 15` (literal mask) is fine"
        );
    // comparison results are Bool, so boolean ops chain them.
    assert_eq!(
            check_src("module m;\nentity E { a: unsigned[8] in, b: unsigned[8] in, y: Bool out, }\nimpl E { y = (a > b) and (a != b); }\n"),
            0,
            "boolean ops on comparison results are fine"
        );
}

#[test]
/// A logical operator's template controls the result type.
fn logical_operator_template_controls_output_type() {
    let src = "module m;\n\
            enum Left { L }\n\
            enum Right { R }\n\
            enum Result { Yes }\n\
            impl Operator<\"and\", Right, Result> for Left {\n\
              fn apply(self, rhs: Right) -> Result { return Result::Yes; }\n\
            }\n\
            entity E { a: Left in, b: Right in, y: Result out }\n\
            impl E { y = a and b; }\n";
    assert_eq!(
        check_src(src),
        0,
        "the Output parameter types the expression"
    );
}

#[test]
/// A custom operator selects both its input and output templates.
fn custom_operator_selects_input_and_output_templates() {
    let ok = "module m;\n\
            attr precedence: integer for impl;\n\
            enum Left { L } enum Right { R } enum Result { Yes }\n\
            #[precedence = 45]\n\
            impl Operator<\"merge\", Right, Result> for Left {\n\
              fn apply(self, rhs: Right) -> Result { return Result::Yes; }\n\
            }\n\
            entity E { a: Left in, b: Right in, y: Result out }\n\
            impl E { y = a merge b; }\n";
    assert_eq!(check_src(ok), 0);

    let bad = ok.replace("b: Right in", "b: Left in");
    assert_eq!(
        check_src(&bad),
        1,
        "the Input template participates in overload selection"
    );
}

/// A literal that cannot fit the operand width made the comparison mask it
/// (600 -> 88 on a `unsigned[8]`), so the guard compared the wrong value and
/// silently passed. The wrapped-expression case must still be allowed.
#[test]
fn out_of_range_comparison_literal_is_rejected() {
    let ent = "module m;\nentity E { q: unsigned[8] in, y: unsigned[8] out, }\nimpl E { y = ";
    assert_eq!(
        check_src(&format!("{ent}if q == 600 {{ 1 }} else {{ 0 }}; }}\n")),
        1,
        "600 cannot be a unsigned[8]"
    );
    assert_eq!(
        check_src(&format!("{ent}if q == 0 - 3 {{ 1 }} else {{ 0 }}; }}\n")),
        0,
        "a wrapped constant is a real 8-bit pattern (253)"
    );
    assert_eq!(
        check_src(&format!("{ent}if q == 255 {{ 1 }} else {{ 0 }}; }}\n")),
        0,
        "the top of the range still fits"
    );
    // The literal may sit on either side.
    assert_eq!(
        check_src(&format!("{ent}if 600 == q {{ 1 }} else {{ 0 }}; }}\n")),
        1,
        "flagged from the left too"
    );
}

/// Fit checking is advisory for expressions that exceed the narrow
/// evaluator. Host overflow must not abort semantic analysis; later
/// arbitrary-width lowering retains the expression.
#[test]
fn overflowing_conversion_constant_does_not_panic() {
    let src = "module m;\n\
            entity E { y: unsigned[64] out }\n\
            impl E { y = unsigned[64](9223372036854775807 + 1); }\n";
    let _ = check_src(src);
}

/// Hardware has no divide-by-zero trap, so a constant zero divisor just
/// yielded 0 with no complaint.
#[test]
fn constant_zero_divisor_is_rejected() {
    let src =
        "module m;\nentity E { a: unsigned[8] in, y: unsigned[8] out, }\nimpl E { y = a / 0; }\n";
    assert_eq!(check_src(src), 1);
    let ok = "module m;\nentity E { a: unsigned[8] in, b: unsigned[8] in, y: unsigned[8] out, }\nimpl E { y = a / b; }\n";
    assert_eq!(check_src(ok), 0, "a runtime divisor is fine");
}

#[test]
/// Operators the grammar reserves cannot be overloaded.
fn reserved_operators_cannot_be_overloaded() {
    let header = "module m;\nattr precedence: integer for impl;\n\
            enum A { A0 }\n";
    // Grammar symbols (assignment, path, ranges) and the derived
    // comparisons cannot be claimed by an operator impl.
    for sym in ["=", "::", ".", "..", "<", "=="] {
        let src = format!(
                "{header}#[precedence = 5] impl Operator<\"{sym}\", A, A> for A {{ fn apply(self, rhs: A) -> A {{ return self; }} }}\n"
            );
        assert!(
            check_src(&src) >= 1,
            "reserved operator `{sym}` should error"
        );
    }
    // A genuine custom punctuation operator is accepted.
    let ok = format!(
            "{header}#[precedence = 5] impl Operator<\"^^\", A, A> for A {{ fn apply(self, rhs: A) -> A {{ return self; }} }}\n"
        );
    assert_eq!(check_src(&ok), 0);
}

#[test]
/// A custom operator must declare a precedence, and declare it consistently
/// across impls.
fn custom_operator_precedence_is_required_and_consistent() {
    let header = "module m;\nattr precedence: integer for impl;\n\
            enum A { A0 } enum B { B0 }\n";
    let missing = format!(
            "{header}impl Operator<\"join\", A, A> for A {{ fn apply(self, rhs: A) -> A {{ return self; }} }}\n"
        );
    assert_eq!(check_src(&missing), 1);

    let conflict = format!(
            "{header}\
             #[precedence = 40] impl Operator<\"join\", A, A> for A {{ fn apply(self, rhs: A) -> A {{ return self; }} }}\n\
             #[precedence = 30] impl Operator<\"join\", B, B> for B {{ fn apply(self, rhs: B) -> B {{ return self; }} }}\n"
        );
    assert_eq!(check_src(&conflict), 1);
}
