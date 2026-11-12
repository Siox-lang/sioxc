use super::*;

#[test]
fn nested_operator_defaults_keep_distinct_selected_implementations() {
    let mut sources = [
        "module client; use model::Token; use core::primitive::Flag;
         fn different() -> Flag { return Token::A != Token::B; }
         fn same() -> Flag { return Token::A != Token::A; }",
        "module model; use core::primitive::Flag; use core::ops::Equal;
         pub enum Group { First = 3, Second = 9 }
         pub enum Token { A = 9, B = 2 }
         impl Equal<Group> for Group {
             fn eq(self, rhs: Group) -> Flag { return Flag::true; }
         }
         impl Equal<Token> for Token {
             fn eq(self, rhs: Token) -> Flag {
                 if self == rhs { return Flag::true; }
                 return Group::First != Group::Second;
             }
         }",
        "module core::ops; use core::primitive::Flag;
         pub trait Equal<Rhs> {
             fn eq(self, rhs: Rhs) -> Flag;
             fn ne(self, rhs: Rhs) -> Flag {
                 let other: Rhs = rhs;
                 return if self.eq(other) { Flag::false } else { Flag::true };
             }
         }
         attr lang for Equal = \"eq\";",
        "module core::primitive; pub enum Flag { false = 5, true = 17 } attr lang for Flag = \"bool\";",
    ];
    for _ in 0..2 {
        with_numeric_source(&sources, |source, modules| {
            let native = NativeNumericSource(source);
            for environment in [&source as &dyn Environment, &native] {
                for (name, expected) in [("different", true), ("same", false)] {
                    let value =
                        evaluate_function(numeric_function(modules, name), vec![], environment, 0)
                            .unwrap();
                    assert_eq!(source.condition_value(&value), Some(expected), "{name}");
                }
            }
        });
        sources.reverse();
    }
}

#[test]
fn typed_match_results_keep_numeric_and_aggregate_contracts() {
    let mut sources = [
        "module values;
         struct Config { pub gain: real, pub count: natural }
         const CFG: Config = match 0 { 0 => { .gain = -2, .count = 18446744073709551615 }, _ => { .gain = 3, .count = 1 } };
         fn choose(value: integer) -> real { return match value { 0 => -3, _ => 2.5 }; }
         fn count() -> natural { return CFG.count; }
         fn gain() -> real { return CFG.gain; }
         fn array() -> real[4..3] { return match 0 { 0 => [1, 2], _ => [3, 4] }; }
         fn array_value() -> real { return array()[4]; }
         fn tuple() -> (real, natural) { return match 0 { 0 => (-1, 7), _ => (3, 9) }; }
         fn tuple_value() -> natural { return tuple().1; }
         fn foreign() -> real;
         fn lazy() -> real { return match 0 { 0 => 1, _ => foreign() }; }
         fn invalid() -> natural { return match 0 { 0 => -1, _ => 1 }; }
         fn invalid_field() -> Config { return match 0 { 0 => { .gain = 1, .count = 1.5 }, _ => CFG }; }
         fn invalid_array() -> real[2] { return match 0 { 0 => [1], _ => [2, 3] }; }",
        "module core::primitive; pub enum Flag { false = 5, true = 17 } attr lang for Flag = \"bool\";",
    ];
    for _ in 0..2 {
        with_numeric_source(&sources, |source, modules| {
            let native = NativeNumericSource(source);
            for environment in [&source as &dyn Environment, &native] {
                for (name, expected) in [
                    ("count", Value::Natural(u64::MAX)),
                    ("gain", Value::Real(-2.0)),
                    ("array_value", Value::Real(1.0)),
                    ("tuple_value", Value::Natural(7)),
                    ("lazy", Value::Real(1.0)),
                ] {
                    assert_eq!(
                        evaluate_function(numeric_function(modules, name), vec![], environment, 0),
                        Ok(expected),
                        "{name}"
                    );
                }
                for (argument, expected) in [(0, -3.0), (1, 2.5)] {
                    assert_eq!(
                        evaluate_function(
                            numeric_function(modules, "choose"),
                            vec![Value::Integer(argument)],
                            environment,
                            0
                        ),
                        Ok(Value::Real(expected))
                    );
                }
                for name in ["invalid", "invalid_field", "invalid_array"] {
                    assert_eq!(
                        evaluate_function(numeric_function(modules, name), vec![], environment, 0)
                            .unwrap_err()
                            .reason,
                        Reason::Unsupported,
                        "{name}"
                    );
                }
            }
        });
        sources.reverse();
    }
}

#[test]
fn source_operator_defaults_keep_selected_trait_argument_types() {
    let mut sources = [
        "module client; use model::Token; use core::primitive::Flag;
         fn different() -> Flag { return Token::A != Token::B; }
         fn same() -> Flag { return Token::A != Token::A; }",
        "module model; use core::primitive::Flag; use core::ops::Equal;
         pub enum Token { A = 9, B = 2 }
         impl Equal<Token> for Token {
             fn eq(self, rhs: Token) -> Flag { return self == rhs; }
         }",
        "module core::ops; use core::primitive::Flag;
         pub trait Equal<Rhs> {
             fn eq(self, rhs: Rhs) -> Flag;
             fn ne(self, rhs: Rhs) -> Flag {
                 let other: Rhs = rhs;
                 return if self.eq(other) { Flag::false } else { Flag::true };
             }
         }
         attr lang for Equal = \"eq\";",
        "module core::primitive; pub enum Flag { false = 5, true = 17 } attr lang for Flag = \"bool\";",
    ];
    for _ in 0..2 {
        with_numeric_source(&sources, |source, modules| {
            let native = NativeNumericSource(source);
            for environment in [&source as &dyn Environment, &native] {
                for (name, expected) in [("different", true), ("same", false)] {
                    let value =
                        evaluate_function(numeric_function(modules, name), vec![], environment, 0)
                            .unwrap();
                    assert_eq!(source.condition_value(&value), Some(expected), "{name}");
                }
            }
        });
        sources.reverse();
    }
}

#[test]
fn contextual_numeric_literals_precede_native_word_projection() {
    struct Words<'i, 'a>(Source<'i, 'a>);
    impl Environment for Words<'_, '_> {
        fn source(&self) -> Option<Source<'_, '_>> {
            Some(self.0)
        }
        fn domain(&self) -> IntegerDomain {
            IntegerDomain::NativeWord
        }
        fn literal(&self, text: &str) -> Option<i128> {
            unsigned_lit_text(text).map(|word| i128::from(word as i64))
        }
    }
    let sources = ["module values;
        struct Config { pub count: natural, pub gain: real }
        const CFG: Config = Config { .count = 0xffffffffffffffff, .gain = -2 };
        fn count() -> natural { return CFG.count; }
        fn gain() -> real { return CFG.gain; }
        fn negative() -> real { return -3; }
        fn add(value: real) -> real { return -2 + value + -1; }
        fn wide_real() -> real { return 0xffffffffffffffff; }
        fn bad() -> natural { return -1; }"];
    with_numeric_source(&sources, |source, modules| {
        let words = Words(source);
        for environment in [&source as &dyn Environment, &words] {
            for (name, expected) in [
                ("count", Value::Natural(u64::MAX)),
                ("gain", Value::Real(-2.0)),
                ("negative", Value::Real(-3.0)),
                ("wide_real", Value::Real(u64::MAX as f64)),
            ] {
                assert_eq!(
                    evaluate_function(numeric_function(modules, name), vec![], environment, 0),
                    Ok(expected),
                    "{name}"
                );
            }
            assert_eq!(
                evaluate_function(
                    numeric_function(modules, "add"),
                    vec![Value::Real(8.0)],
                    environment,
                    0
                ),
                Ok(Value::Real(5.0))
            );
            assert!(
                evaluate_function(numeric_function(modules, "bad"), vec![], environment, 0)
                    .is_err()
            );
        }
    });
}

#[test]
fn numeric_aggregates_keep_domains_labels_and_unchanged_storage() {
    let mut sources = [
        "module values;
         struct Config { pub gain: real, pub count: natural, pub data: real[-2..-1], pub counts: natural[4..5] }
         const CFG: Config = Config { .gain = 1.5, .count = 18446744073709551615,
             .data = [1, 2.5], .counts = [18446744073709551615, 1] };
         const KERNEL: integer = 1;
         fn update() -> real {
             let value: Config = Config { ..CFG, .gain = CFG.gain + 1 };
             return value.gain + value.data[-2];
         }
         fn counts() -> natural { return CFG.counts[4] - CFG.counts[5]; }
         fn tuple(value: (real[11..10], natural)) -> real { return value.0[11] + real(value.1); }
         fn tuple_argument() -> real { return tuple((CFG.data, 2)); }
         fn data() -> real[11..10] { return CFG.data; }
         fn returned_data() -> real { return data()[11]; }
         fn identity(value: (real, real)) -> (real, real) { return value; }
         fn overflow() -> integer { let value: Config = Config { ..CFG, .count = natural(0) - natural(1) }; return 0; }
         fn wrong_domain() -> integer { let value: Config = Config { ..CFG, .count = 1.5 }; return 0; }
         fn typed_integer() -> integer { let value: Config = Config { ..CFG, .count = KERNEL }; return 0; }",
        "module core::primitive; pub enum Flag { false = 5, true = 17 } attr lang for Flag = \"bool\";",
    ];
    for _ in 0..2 {
        with_numeric_source(&sources, |source, modules| {
            let native = NativeNumericSource(source);
            for environment in [&source as &dyn Environment, &native] {
                for (name, expected) in [
                    ("update", Value::Real(3.5)),
                    ("counts", Value::Natural(u64::MAX - 1)),
                    ("tuple_argument", Value::Real(3.0)),
                    ("returned_data", Value::Real(1.0)),
                ] {
                    assert_eq!(
                        evaluate_function(numeric_function(modules, name), vec![], environment, 0),
                        Ok(expected),
                        "{name}"
                    );
                }
                for (name, reason) in [
                    ("overflow", Reason::Overflow),
                    ("wrong_domain", Reason::Unsupported),
                    ("typed_integer", Reason::Unsupported),
                ] {
                    assert_eq!(
                        evaluate_function(numeric_function(modules, name), vec![], environment, 0)
                            .unwrap_err()
                            .reason,
                        reason,
                        "{name}"
                    );
                }
                let values: Arc<[Value]> = vec![Value::Real(f64::NAN), Value::Real(-0.0)].into();
                let result = evaluate_function(
                    numeric_function(modules, "identity"),
                    vec![Value::Tuple(values.clone())],
                    environment,
                    0,
                )
                .unwrap();
                let Value::Tuple(result) = result else {
                    panic!("tuple result");
                };
                assert!(
                    Arc::ptr_eq(&values, &result),
                    "unchanged NaN/signed-zero tuples must not be copied"
                );
                assert!(matches!(result[0], Value::Real(value) if value.is_nan()));
                assert!(
                    matches!(result[1], Value::Real(value) if value.to_bits() == (-0.0f64).to_bits())
                );
            }
        });
        sources.reverse();
    }
}

pub(super) fn with_numeric_source(
    sources: &[&str],
    check: impl FnOnce(Source<'_, '_>, &[crate::syntax::Module]),
) {
    let mut sink = DiagnosticSink::default();
    let mut modules: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(i, source)| parse_module(FileId(i as u32), source, &mut sink))
        .collect();
    crate::syntax::imports::desugar(&mut modules, &mut sink);
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let enums = crate::types::enum_values::EnumValues::collect(&declarations);
    check(Source::new(&declarations).with_enums(&enums), &modules);
}

pub(super) fn numeric_function<'a>(modules: &'a [crate::syntax::Module], name: &str) -> &'a FnDecl {
    modules
        .iter()
        .flat_map(|module| &module.items)
        .find_map(|item| match item {
            Item::Fn(function) if function.name.text == name => Some(function),
            _ => None,
        })
        .unwrap()
}

pub(super) struct NativeNumericSource<'i, 'a>(pub(super) Source<'i, 'a>);

impl Environment for NativeNumericSource<'_, '_> {
    fn source(&self) -> Option<Source<'_, '_>> {
        Some(self.0)
    }
    fn domain(&self) -> IntegerDomain {
        IntegerDomain::NativeWord
    }
}

#[test]
fn typed_numeric_values_keep_declared_domains_through_calls_and_assignments() {
    let mut sources = [
        "module client; use providers::numbers::{provider = self};
         fn update(value: provider::Scalar) -> provider::Scalar {
             let local: provider::Scalar = 2;
             local = value + local;
             value = local / 2;
             return value;
         }
         fn next(value: provider::Count) -> provider::Count { value = value + 1; return value; }
         fn identity(value: provider::Count) -> provider::Count { return value; }
         fn literal_count() -> provider::Count { return identity(18446744073709551615); }
         fn named_real() -> provider::Scalar { return provider::HALF + 1; }
         fn named_natural() -> provider::Count { return provider::TOTAL - 5; }
         fn projection() -> integer { return integer(provider::TOTAL / 3); }
         fn alias_constructor() -> real { return provider::Scalar(provider::TOTAL); }",
        "module providers::numbers;
         pub type Scalar = real;
         pub type Count = natural;
         pub const HALF: Scalar = 1.5;
         pub const TOTAL: Count = 18446744073709551615;",
    ];
    for _ in 0..2 {
        with_numeric_source(&sources, |source, modules| {
            let native = NativeNumericSource(source);
            for environment in [&source as &dyn Environment, &native] {
                assert_eq!(
                    evaluate_function(
                        numeric_function(modules, "update"),
                        vec![Value::Real(5.0)],
                        environment,
                        0
                    ),
                    Ok(Value::Real(3.5))
                );
                assert_eq!(
                    evaluate_function(
                        numeric_function(modules, "next"),
                        vec![Value::Natural(u64::MAX - 1)],
                        environment,
                        0
                    ),
                    Ok(Value::Natural(u64::MAX))
                );
                for (name, expected) in [
                    ("literal_count", Value::Natural(u64::MAX)),
                    ("named_real", Value::Real(2.5)),
                    ("named_natural", Value::Natural(u64::MAX - 5)),
                    ("projection", Value::Integer(i128::from(u64::MAX / 3))),
                    ("alias_constructor", Value::Real(u64::MAX as f64)),
                ] {
                    assert_eq!(
                        evaluate_function(numeric_function(modules, name), vec![], environment, 0),
                        Ok(expected.clone()),
                        "{name}"
                    );
                    assert_eq!(
                        expected.integer(),
                        if name == "projection" {
                            Some(i128::from(u64::MAX / 3))
                        } else {
                            None
                        }
                    );
                }
                assert_eq!(source.word(Value::Natural(3)), None);
                assert_eq!(source.word(Value::Real(3.0)), None);
                assert!(
                    evaluate_function(
                        numeric_function(modules, "next"),
                        vec![Value::Integer(3)],
                        environment,
                        0
                    )
                    .is_err(),
                    "typed integer arguments cannot become natural implicitly"
                );
            }
        });
        sources.reverse();
    }
}

#[test]
fn numeric_conditions_use_registered_boolean_values_and_ieee_real_comparisons() {
    let sources = [
        "module values; use core::primitive::Flag;
         const MAXIMUM: natural = 18446744073709551615;
         fn choose(value: real) -> real { if value > 1.0 { return value + 0.5; } else { return 0; } }
         fn negative_zero() -> Flag { return -0.0 == 0.0; }
         fn unordered() -> Flag { return (0.0 / 0.0) != (0.0 / 0.0); }
         fn natural_order() -> Flag { return MAXIMUM > natural(1); }",
        "module core::primitive; pub enum Flag { false = 5, true = 17 } attr lang for Flag = \"bool\";",
    ];
    with_numeric_source(&sources, |source, modules| {
        let native = NativeNumericSource(source);
        for environment in [&source as &dyn Environment, &native] {
            for (input, expected) in [(2.0, 2.5), (0.5, 0.0), (f64::NAN, 0.0)] {
                assert_eq!(
                    evaluate_function(
                        numeric_function(modules, "choose"),
                        vec![Value::Real(input)],
                        environment,
                        0
                    ),
                    Ok(Value::Real(expected))
                );
            }
            for name in ["negative_zero", "unordered", "natural_order"] {
                let value =
                    evaluate_function(numeric_function(modules, name), vec![], environment, 0)
                        .unwrap();
                assert_eq!(source.condition_value(&value), Some(true), "{name}");
            }
        }
    });
}

#[test]
fn typed_numeric_failures_do_not_wrap_clamp_or_erase_annotations() {
    let sources = ["module values;
         fn add(value: natural) -> natural { return value + 1; }
         fn subtract(value: natural) -> natural { return value - 1; }
         fn multiply(value: natural) -> natural { return value * value; }
         fn divide(value: natural) -> natural { return value / 0; }
         fn invalid_shift(value: natural) -> natural { return value << 64; }
         fn real_to_count(value: real) -> natural { return natural(value); }
         fn real_to_integer(value: real) -> integer { return integer(value); }
         fn count_to_integer(value: natural) -> integer { return integer(value); }
         fn invalid_return() -> natural { return 1.5; }
         fn invalid_local() -> real { let value: natural = 1.5; return 2.0; }
         fn invalid_assignment() -> real { let value: natural = 1; value = 1.5; return 2.0; }
         fn invalid_argument() -> natural { return add(1.5); }
         fn invalid_integer_to_count() -> natural { return natural(18446744073709551615); }
         fn invalid_integer_to_real() -> real { return real(18446744073709551615); }
         fn integer_identity(value: integer) -> integer { return value; }
         fn invalid_integer_argument() -> integer { return integer_identity(18446744073709551615); }
         fn invalid_integer_return() -> integer { return 18446744073709551615; }"];
    with_numeric_source(&sources, |source, modules| {
        for (name, argument, reason) in [
            ("add", Value::Natural(u64::MAX), Reason::Overflow),
            ("subtract", Value::Natural(0), Reason::Overflow),
            ("multiply", Value::Natural(u64::MAX), Reason::Overflow),
            ("divide", Value::Natural(1), Reason::DivisionByZero),
            ("invalid_shift", Value::Natural(1), Reason::InvalidShift),
            ("real_to_count", Value::Real(-0.5), Reason::Unsupported),
            (
                "real_to_count",
                Value::Real(u64::MAX as f64),
                Reason::Unsupported,
            ),
            (
                "real_to_count",
                Value::Real(f64::INFINITY),
                Reason::Unsupported,
            ),
            (
                "real_to_integer",
                Value::Real(f64::NAN),
                Reason::Unsupported,
            ),
            (
                "real_to_integer",
                Value::Real(-(i64::MIN as f64)),
                Reason::Unsupported,
            ),
            (
                "count_to_integer",
                Value::Natural(u64::MAX),
                Reason::Unsupported,
            ),
        ] {
            let error = evaluate_function(
                numeric_function(modules, name),
                vec![argument.clone()],
                &source,
                0,
            )
            .unwrap_err();
            assert_eq!(error.reason, reason, "{name}: {argument:?}");
            assert!(error.span.start < error.span.end);
        }
        for name in [
            "invalid_return",
            "invalid_local",
            "invalid_assignment",
            "invalid_argument",
        ] {
            assert_eq!(
                evaluate_function(numeric_function(modules, name), vec![], &source, 0)
                    .unwrap_err()
                    .reason,
                Reason::Unsupported,
                "{name}"
            );
        }
        let native = NativeNumericSource(source);
        for environment in [&source as &dyn Environment, &native] {
            for name in [
                "invalid_integer_to_count",
                "invalid_integer_to_real",
                "invalid_integer_argument",
                "invalid_integer_return",
            ] {
                assert!(
                    evaluate_function(numeric_function(modules, name), vec![], environment, 0)
                        .is_err(),
                    "{name}: an integer operand must fit its signed ABI before conversion"
                );
            }
        }
    });
}
