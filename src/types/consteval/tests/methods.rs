use super::numeric::{numeric_function, with_numeric_source, NativeNumericSource};
use super::*;

const CONTRACTS: &str = "module contracts;
    pub trait Echo<Input> {
        fn echo(value: Input) -> Input {
            let local: Input = value;
            return local;
        }
        fn nested(value: Input) -> Input { return self.echo(self.echo(value)); }
        fn keep() -> Self { return self; }
    }";

#[test]
fn constant_methods_retain_selected_trait_context_and_contextual_arguments() {
    let mut sources = [
        CONTRACTS,
        "module providers::left;
         use Contract = contracts::Echo;
         pub struct Token(integer);
         pub type Alias = Token;
         pub type Scalar = real;
         impl Contract<Scalar> for Alias {}",
        "module providers::right;
         use contracts::Echo;
         pub struct Token(integer);
         impl Echo<natural> for Token {}
         pub enum Code { '0' = 9, '1' = 17 }
         pub struct Palette(integer);
         impl Echo<Code> for Palette {}",
        "module client;
         use L = providers::left; use R = providers::right;
         fn real_value() -> real { return L::Alias(3).echo(7); }
         fn real_nested() -> real { return L::Alias(3).nested(-2); }
         fn natural_value() -> natural { return R::Token(5).echo(18446744073709551615); }
         fn code_value() -> R::Code { return R::Palette(0).echo('1'); }
         fn code_nested() -> R::Code { return R::Palette(0).nested('0'); }
         fn left_owner() -> integer { return integer(L::Alias(3).keep()); }
         fn right_owner() -> integer { return integer(R::Token(5).keep()); }",
    ];
    for _ in 0..2 {
        with_numeric_source(&sources, |source, modules| {
            let native = NativeNumericSource(source);
            for environment in [&source as &dyn Environment, &native] {
                for (name, expected) in [
                    ("real_value", Value::Real(7.0)),
                    ("real_nested", Value::Real(-2.0)),
                    ("natural_value", Value::Natural(u64::MAX)),
                    ("left_owner", Value::Integer(3)),
                    ("right_owner", Value::Integer(5)),
                ] {
                    assert_eq!(
                        evaluate_function(numeric_function(modules, name), vec![], environment, 0),
                        Ok(expected),
                        "{name}"
                    );
                }
                for (name, expected) in [("code_value", 17), ("code_nested", 9)] {
                    let value =
                        evaluate_function(numeric_function(modules, name), vec![], environment, 0)
                            .unwrap();
                    assert_eq!(source.word(value), Some(expected), "{name}");
                }
            }
            let default = modules
                .iter()
                .flat_map(|module| &module.items)
                .find_map(|item| match item {
                    Item::Trait(declaration) => declaration
                        .items
                        .iter()
                        .find(|method| method.name.text == "echo"),
                    _ => None,
                })
                .unwrap();
            let mut count = 0;
            for implementation in
                modules
                    .iter()
                    .flat_map(|module| &module.items)
                    .filter_map(|item| match item {
                        Item::Impl(implementation) => Some(implementation),
                        _ => None,
                    })
            {
                let owner = source.type_id(&implementation.target, None).unwrap();
                let receiver = Value::Record {
                    owner,
                    fields: vec![Value::Integer(0)].into(),
                };
                let selected = source.method_function(&default.name, &receiver).unwrap();
                assert!(std::ptr::eq(selected.function, default));
                assert!(std::ptr::eq(
                    selected.implementation.unwrap(),
                    implementation
                ));
                count += 1;
            }
            assert_eq!(count, 3, "inspect every borrowed implementation context");
        });
        sources.reverse();
    }
}

#[test]
fn constant_methods_keep_inherent_priority_ambiguity_and_signed_bindings() {
    let mut sources = [
        CONTRACTS,
        "module model;
         use contracts::Echo;
         pub struct Choice(integer);
         impl Echo<integer> for Choice {}
         impl Choice { pub fn echo(value: integer) -> integer { return value + 7; } }
         pub struct Ambiguous(integer);
         impl Echo<integer> for Ambiguous {}
         impl Echo<real> for Ambiguous {}
         pub struct Signed(integer);
         impl Echo<integer> for Signed {}",
        "module client; use model::{Choice, Ambiguous, Signed};
         fn inherent() -> integer { return Choice(0).echo(6); }
         fn ambiguous() -> Ambiguous { return Ambiguous(0).keep(); }
         fn signed_limit() -> integer { return Signed(0).echo(9223372036854775807); }
         fn invalid_integer() -> integer { return Signed(0).echo(9223372036854775808); }
         fn invalid_argument() -> integer { return Signed(0).echo(1.5); }",
    ];
    for _ in 0..2 {
        with_numeric_source(&sources, |source, modules| {
            let native = NativeNumericSource(source);
            for environment in [&source as &dyn Environment, &native] {
                for (name, value) in [("inherent", 13), ("signed_limit", i128::from(i64::MAX))] {
                    assert_eq!(
                        evaluate_function(numeric_function(modules, name), vec![], environment, 0),
                        Ok(Value::Integer(value)),
                        "{name}"
                    );
                }
                for name in ["ambiguous", "invalid_integer", "invalid_argument"] {
                    assert!(
                        evaluate_function(numeric_function(modules, name), vec![], environment, 0)
                            .is_err(),
                        "{name} cannot acquire another impl's context or an untyped argument"
                    );
                }
            }
        });
        sources.reverse();
    }
}
