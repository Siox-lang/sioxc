use super::numeric::NativeNumericSource;
use super::*;

const CORE: &str = "module core::factories;
    pub trait Factory { static fn default() -> Self; }
    attr lang for Factory = \"default\";";

#[test]
fn default_constructor_applications_keep_arguments_and_reject_runtime_places() {
    let text = "module applications;
        type Samples<N: integer> = integer[N];
        const GENERIC: Samples<3> = Samples<3>();
        const INDEXED: integer[4..2] = integer[4..2]();
        const BOUNDED: integer = integer<1..8>();
        fn foreign() -> integer;
        const DYNAMIC: integer[2] = integer[foreign()]();
        const VALUE: integer = 1;
        const VALUECALL: integer = VALUE();
        struct Record<T> { pub item: T }
        const NOMINAL: Record<integer> = Record<integer>();";
    let mut sink = DiagnosticSink::default();
    let module = parse_module(FileId(0), text, &mut sink);
    let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let declarations = DeclarationIndex::collect(&resolved, std::slice::from_ref(&module));
    let source = Source::new(&declarations);
    let native = NativeNumericSource(source);
    for item in &module.items {
        let Item::Const(constant) = item else {
            continue;
        };
        for environment in [&source as &dyn Environment, &native] {
            let value = evaluate_value(&constant.value, environment, None, 0);
            match constant.name.text.as_str() {
                "GENERIC" | "INDEXED" => {
                    let Value::Array {
                        values,
                        left,
                        descending,
                        ..
                    } = value.unwrap()
                    else {
                        panic!("applied array")
                    };
                    assert_eq!(values.len(), 3);
                    assert!(values.iter().all(|value| value == &Value::Integer(0)));
                    assert_eq!(
                        (left, descending),
                        if constant.name.text == "GENERIC" {
                            (0, false)
                        } else {
                            (4, true)
                        }
                    );
                }
                "VALUE" => assert_eq!(value, Ok(Value::Integer(1))),
                _ => {
                    assert!(
                        value.is_err(),
                        "{} must not become a bare default: {value:?}",
                        constant.name.text
                    );
                }
            }
        }
    }
}

#[test]
fn typed_defaults_preserve_source_owners_overrides_and_aggregate_labels() {
    let mut sources = [
        CORE,
        "module left;
         use Make = core::factories::Factory;
         pub enum Phase { First = 7, Last = 19 }
         pub enum Selected { First = 23, Last = 41 }
         impl Make for Selected { static fn default() -> Self { return Selected::Last; } }
         pub struct Count(integer);
         impl Make for Count { static fn default() -> Self { return Count(13); } }
         pub struct Packet { pub number: integer, pub phase: Phase, pub selected: Selected,
             pub count: Count, pub samples: integer[-2..1], pub pair: (natural, real) }
         pub type Alias = Packet;
         pub struct Wrapped(Alias);
         pub type Rows = integer[6..4];
         pub type Pair = (natural, real);
         pub const PACKET: Alias = Alias();
         pub const PARTIAL: Packet = Packet { .number = 9 };
         pub const EXPLICIT: Selected = Selected::default();
         pub const ARRAY: Rows = Rows();
         pub const TUPLE: Pair = Pair();
         pub const INTEGER: integer = integer();
         pub const NATURAL: natural = natural();
         pub const REAL: real = real();
         pub const CHARACTER: Char = Char();
         pub const WRAPPED: Wrapped = Wrapped();
         fn default() -> integer { return 45; }
         pub const FUNCTION: integer = default();
         pub fn width() -> integer { return PACKET.samples'length + integer(PACKET.count); }",
        "module right;
         pub trait Default { static fn default() -> Self; }
         pub struct Packet { pub number: integer }
         impl Default for Packet { static fn default() -> Self { return Packet { .number = 77 }; } }
         pub const PACKET: Packet = Packet();
         pub enum Phase { First = 31, Last = 53 }
         pub const PHASE: Phase = Phase();",
    ];
    for _ in 0..2 {
        let mut sink = DiagnosticSink::default();
        let mut modules: Vec<_> = sources
            .iter()
            .enumerate()
            .map(|(index, text)| parse_module(FileId(index as u32), text, &mut sink))
            .collect();
        crate::syntax::imports::desugar(&mut modules, &mut sink);
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        let declarations = DeclarationIndex::collect(&resolved, &modules);
        let enums = crate::types::enum_values::EnumValues::collect(&declarations);
        let source = Source::new(&declarations).with_enums(&enums);
        let native = NativeNumericSource(source);
        for module in &modules {
            let left = module.path.segments[0].text == "left";
            for item in &module.items {
                let Item::Const(constant) = item else {
                    continue;
                };
                for environment in [&source as &dyn Environment, &native] {
                    let value = evaluate_value(&constant.value, environment, None, 0)
                        .unwrap_or_else(|error| {
                            panic!(
                                "{}::{}: {error:?}",
                                module.path.segments[0].text, constant.name.text
                            )
                        });
                    match constant.name.text.as_str() {
                        "PACKET" | "PARTIAL" => {
                            let Value::Record { owner, fields } = value else {
                                panic!("record")
                            };
                            let structure = declarations.structure(owner).unwrap();
                            assert_eq!(
                                resolved.def(owner).unwrap().module.as_deref(),
                                Some(if left { "left" } else { "right" })
                            );
                            assert_eq!(fields.len(), structure.fields.len());
                            assert_eq!(
                                fields[0],
                                Value::Integer(if constant.name.text == "PARTIAL" {
                                    9
                                } else {
                                    0
                                })
                            );
                            if left {
                                assert_eq!(source.word(fields[1].clone()), Some(7));
                                assert_eq!(source.word(fields[2].clone()), Some(41));
                                let Value::Record { fields: count, .. } = &fields[3] else {
                                    panic!("newtype")
                                };
                                assert_eq!(count.as_ref(), &[Value::Integer(13)]);
                                let Value::Array {
                                    values,
                                    left,
                                    descending,
                                    ..
                                } = &fields[4]
                                else {
                                    panic!("array")
                                };
                                assert_eq!(values.len(), 4);
                                assert!(values.iter().all(|value| value == &Value::Integer(0)));
                                assert_eq!((*left, *descending), (-2, false));
                                assert_eq!(
                                    fields[5],
                                    Value::Tuple(vec![Value::Natural(0), Value::Real(0.0)].into())
                                );
                            }
                        }
                        "EXPLICIT" => assert_eq!(source.word(value), Some(41)),
                        "PHASE" => assert_eq!(source.word(value), Some(31)),
                        "ARRAY" => {
                            let Value::Array {
                                values,
                                left,
                                descending,
                                ..
                            } = value
                            else {
                                panic!("array")
                            };
                            assert_eq!(values.len(), 3);
                            assert!(values.iter().all(|value| value == &Value::Integer(0)));
                            assert_eq!((left, descending), (6, true));
                        }
                        "TUPLE" => assert_eq!(
                            value,
                            Value::Tuple(vec![Value::Natural(0), Value::Real(0.0)].into())
                        ),
                        "INTEGER" => assert_eq!(value, Value::Integer(0)),
                        "NATURAL" => assert_eq!(value, Value::Natural(0)),
                        "REAL" => assert_eq!(value, Value::Real(0.0)),
                        "CHARACTER" => assert_eq!(value, Value::Character('\0')),
                        "WRAPPED" => {
                            let Value::Record { fields, .. } = value else {
                                panic!("wrapper")
                            };
                            assert_eq!(fields.len(), 1);
                            let Value::Record { fields, .. } = &fields[0] else {
                                panic!("wrapped record")
                            };
                            assert_eq!(fields.len(), 6);
                            assert_eq!(source.word(fields[1].clone()), Some(7));
                        }
                        "FUNCTION" => assert_eq!(value, Value::Integer(45)),
                        _ => panic!("uncovered default fixture"),
                    }
                }
            }
        }
        let width = modules
            .iter()
            .flat_map(|module| &module.items)
            .find_map(|item| match item {
                Item::Fn(function) if function.name.text == "width" => Some(function),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            evaluate_function(width, vec![], &source, 0),
            Ok(Value::Integer(17))
        );
        assert_eq!(
            evaluate_function(width, vec![], &native, 0),
            Ok(Value::Integer(17))
        );
        sources.reverse();
    }
}

#[test]
fn unsupported_default_overrides_never_fall_back_to_structural_zero() {
    let text = "module failures;
        use Make = core::factories::Factory;
        struct Missing(integer);
        impl Make for Missing { static fn default() -> Self; }
        struct Effect(integer);
        fn foreign() -> integer;
        impl Make for Effect { static fn default() -> Self { return Effect(foreign()); } }
        struct Recursive(integer);
        impl Make for Recursive { static fn default() -> Self { return Self(); } }
        struct Ambiguous(integer);
        impl Make for Ambiguous { static fn default() -> Self { return Ambiguous(4); } }
        impl Make for Ambiguous { static fn default() -> Self { return Ambiguous(8); } }
        struct Generic<T> { pub value: T }
        const MISSING: Missing = Missing();
        const EFFECT: Effect = Effect();
        const RECURSIVE: Recursive = Recursive();
        const AMBIGUOUS: Ambiguous = Ambiguous();
        type Applied = Generic<integer>;
        const GENERIC: Applied = Applied();
        type Empty = Effect[0];
        const EMPTY: Empty = Empty();";
    let mut sink = DiagnosticSink::default();
    let mut modules = vec![
        parse_module(FileId(0), CORE, &mut sink),
        parse_module(FileId(1), text, &mut sink),
    ];
    crate::syntax::imports::desugar(&mut modules, &mut sink);
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let source = Source::new(&declarations);
    let native = NativeNumericSource(source);
    for item in &modules[1].items {
        let Item::Const(constant) = item else {
            continue;
        };
        for environment in [&source as &dyn Environment, &native] {
            let value = evaluate_value(&constant.value, environment, None, 0);
            if constant.name.text == "EMPTY" {
                let Value::Array { values, .. } = value.unwrap() else {
                    panic!("empty array")
                };
                assert!(
                    values.is_empty(),
                    "no element default body is called for an empty array"
                );
            } else {
                let error = value.unwrap_err();
                assert!(error.span.start < error.span.end);
            }
        }
    }
}
