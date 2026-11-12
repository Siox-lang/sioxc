use super::numeric::{numeric_function, with_numeric_source, NativeNumericSource};
use super::*;

const LEFT: &str = "module providers::left;
    pub struct Measure(integer);
    pub type Alias = Measure;
    pub struct Outer(Alias);
    pub struct Rate(real);
    pub struct Count(natural);
    pub enum Level { Low = 3, High = 9 }
    pub enum DerivedLevel(Level);
    pub type LevelAlias = DerivedLevel;
    pub enum NestedLevel(LevelAlias);
    pub enum SiblingLevel(Level);
    pub struct WrappedLevel(NestedLevel);
    pub struct Stamped(Level);
    pub enum Wide { High = 18446744073709551616 }
    pub enum DerivedWide(Wide);
    pub struct Packet { pub width: integer, pub gain: real }
    pub struct Envelope(Packet);
    pub const START: Alias = Alias(7);
    pub const RATE: Rate = Rate(-0.0);
    pub const MAX: natural = 18446744073709551615;
    pub const COUNT: Count = Count(MAX);
    pub const STAMPED: Stamped = Stamped(Level::High);
    pub const DERIVED: NestedLevel = NestedLevel(Level::High);
    pub const WRAPPED_LEVEL: WrappedLevel = WrappedLevel(Level::High);
    pub const WIDE: DerivedWide = DerivedWide(Wide::High);
    pub const VALUES: Alias[6..4] = [START, Alias(11), Alias(13)];
    pub const PACKET: Envelope = Envelope(Packet { .width = 3, .gain = 2.5 });
    impl Measure {
        pub fn step(self, amount: integer) -> Self { return Self(integer(self) + amount); }
    }";

const RIGHT: &str = "module providers::right;
    pub struct Measure(integer);
    pub struct Packet { pub width: integer, pub gain: real }
    pub const PACKET: Packet = Packet { .width = 91, .gain = 9.5 };
    pub const START: Measure = Measure(91);";

const CORE: &str = "module core::primitive;
    pub enum Flag { false = 5, true = 17 }
    attr lang for Flag = \"bool\";";

fn with_sources(client: &str, mut check: impl FnMut(Source<'_, '_>, &[crate::syntax::Module])) {
    for sources in [[client, LEFT, RIGHT, CORE], [CORE, RIGHT, LEFT, client]] {
        with_numeric_source(&sources, |source, modules| check(source, modules));
    }
}

#[test]
fn declared_constant_entry_preserves_numeric_context_and_nominal_boundaries() {
    with_sources(
        "module client;
        const MAXIMUM: natural = 18446744073709551615;
        const GAIN: real = 3;
        const VALUES: real[6..4] = [1, 2, 3];
        const WRAPPED: providers::left::Count = providers::left::Count(MAXIMUM);",
        |source, modules| {
            let native = NativeNumericSource(source);
            for environment in [&source as &dyn Environment, &native] {
                let constants: Vec<_> = modules
                    .iter()
                    .filter(|module| module.path.segments[0].text == "client")
                    .flat_map(|module| &module.items)
                    .filter_map(|item| match item {
                        crate::syntax::ast::Item::Const(declaration) => Some(declaration),
                        _ => None,
                    })
                    .collect();
                assert_eq!(constants.len(), 4);
                for declaration in constants {
                    let value = evaluate_constant(declaration, environment, 0).unwrap();
                    match declaration.name.text.as_str() {
                        "MAXIMUM" => assert_eq!(value, Value::Natural(u64::MAX)),
                        "GAIN" => assert_eq!(value, Value::Real(3.0)),
                        "VALUES" => {
                            let Value::Array {
                                values,
                                left,
                                descending,
                                ..
                            } = value
                            else {
                                panic!("array")
                            };
                            assert_eq!((left, descending), (6, true));
                            assert_eq!(
                                values.as_ref(),
                                [Value::Real(1.0), Value::Real(2.0), Value::Real(3.0)]
                            );
                        }
                        "WRAPPED" => {
                            let Value::Record { fields, .. } = value else {
                                panic!("nominal value")
                            };
                            assert_eq!(fields.as_ref(), [Value::Natural(u64::MAX)]);
                        }
                        name => panic!("unexpected constant {name}"),
                    }
                }
            }
        },
    );
}

#[test]
fn derived_constants_preserve_nominal_identity_and_shared_storage() {
    let client = "module client; use L = providers::left; use R = providers::right;
        fn left() -> L::Alias { return L::START; }
        fn right() -> R::Measure { return R::START; }
        fn rate() -> L::Rate { return L::RATE; }
        fn count() -> L::Count { return L::COUNT; }
        fn wide() -> L::DerivedWide { return L::WIDE; }
        fn retain(value: L::Alias) -> L::Alias { let local: L::Alias = value; return local; }";
    with_sources(client, |source, modules| {
        let native = NativeNumericSource(source);
        for environment in [&source as &dyn Environment, &native] {
            let left = evaluate_function(numeric_function(modules, "left"), vec![], environment, 0)
                .unwrap();
            let right =
                evaluate_function(numeric_function(modules, "right"), vec![], environment, 0)
                    .unwrap();
            let (
                Value::Record {
                    owner: a,
                    fields: af,
                },
                Value::Record {
                    owner: b,
                    fields: bf,
                },
            ) = (&left, &right)
            else {
                panic!("newtypes must retain a nominal owner");
            };
            assert_ne!(a, b);
            assert_eq!(af.as_ref(), [Value::Integer(7)]);
            assert_eq!(bf.as_ref(), [Value::Integer(91)]);
            assert_eq!(
                source.word(left.clone()),
                None,
                "word queries must not erase newtypes"
            );
            let retained = evaluate_function(
                numeric_function(modules, "retain"),
                vec![left.clone()],
                environment,
                0,
            )
            .unwrap();
            let Value::Record { fields, .. } = retained else {
                panic!("record")
            };
            assert!(Arc::ptr_eq(af, &fields));
            let rate = evaluate_function(numeric_function(modules, "rate"), vec![], environment, 0)
                .unwrap();
            let Value::Record { fields, .. } = rate else {
                panic!("rate")
            };
            let [Value::Real(value)] = fields.as_ref() else {
                panic!("real representation")
            };
            assert_eq!(value.to_bits(), (-0.0f64).to_bits());
            let count =
                evaluate_function(numeric_function(modules, "count"), vec![], environment, 0)
                    .unwrap();
            let Value::Record { fields, .. } = count else {
                panic!("count")
            };
            assert_eq!(fields.as_ref(), [Value::Natural(u64::MAX)]);
            let wide = evaluate_function(numeric_function(modules, "wide"), vec![], environment, 0)
                .unwrap();
            assert!(matches!(wide, Value::Variant { .. }));
            assert_eq!(
                source.word(wide),
                None,
                "enum derivation must not truncate a wide analysis value"
            );
        }
    });
}

#[test]
fn derived_conversions_follow_only_the_declared_chain() {
    let client = "module client; use L = providers::left; use R = providers::right;
        fn leaf() -> integer { return integer(L::Outer(17)); }
        fn ancestor() -> integer { return integer(L::Alias(L::Outer(19))); }
        fn descendant() -> integer { return integer(L::Outer(L::START)); }
        fn identity() -> integer { return integer(L::Alias(L::START)); }
        fn method() -> integer { return integer(L::START.step(2)); }
        fn array() -> integer { return integer(L::VALUES[4]); }
        fn gain() -> real { return real(L::Rate(2.5)); }
        fn natural_projection() -> natural { return natural(L::COUNT); }
        fn field() -> integer { return L::PACKET.width; }
        fn record() -> real { return L::Packet(L::PACKET).gain; }
        fn enumeration() -> integer { return integer(L::Level(L::STAMPED)); }
        fn enum_newtype() -> integer { return integer(L::NestedLevel(L::Level::High)); }
        fn enum_inherited() -> integer { return integer(L::NestedLevel::High); }
        fn enum_base() -> integer { return integer(L::Level(L::DERIVED)); }
        fn enum_mixed() -> integer { return integer(L::Level(L::WRAPPED_LEVEL)); }
        fn enum_inner() -> integer { return integer(L::NestedLevel(L::WRAPPED_LEVEL)); }
        fn enum_via_base() -> integer { return integer(L::NestedLevel(L::Level(L::STAMPED))); }
        fn enum_original() -> L::Level { return L::Level::High; }
        fn enum_identity() -> L::NestedLevel { return L::DERIVED; }
        fn enum_argument(value: L::Level) -> L::NestedLevel { return L::NestedLevel(value); }
        fn enum_sibling() -> L::DerivedLevel { return L::DerivedLevel(L::SiblingLevel::High); }
        fn mixed_sibling() -> L::DerivedLevel { return L::DerivedLevel(L::STAMPED); }
        fn mixed_sibling_reverse() -> L::Stamped { return L::Stamped(L::DERIVED); }
        fn enum_implicit() -> L::NestedLevel { return L::Level::High; }
        fn skipped_enum_projection() -> integer { return integer(L::STAMPED); }
        fn sibling() -> L::Alias { return L::Alias(R::START); }
        fn implicit() -> L::Alias { return R::START; }
        fn primitive_sibling() -> real { return real(L::START); }
        fn wrong_primitive() -> L::Alias { return L::Alias(1.5); }
        fn arity() -> L::Alias { return L::Alias(1, 2); }";
    with_sources(client, |source, modules| {
        let native = NativeNumericSource(source);
        for environment in [&source as &dyn Environment, &native] {
            for (name, expected) in [
                ("leaf", Value::Integer(17)),
                ("ancestor", Value::Integer(19)),
                ("descendant", Value::Integer(7)),
                ("identity", Value::Integer(7)),
                ("method", Value::Integer(9)),
                ("array", Value::Integer(13)),
                ("gain", Value::Real(2.5)),
                ("natural_projection", Value::Natural(u64::MAX)),
                ("field", Value::Integer(3)),
                ("record", Value::Real(2.5)),
                ("enumeration", Value::Integer(9)),
                ("enum_newtype", Value::Integer(9)),
                ("enum_inherited", Value::Integer(9)),
                ("enum_base", Value::Integer(9)),
                ("enum_mixed", Value::Integer(9)),
                ("enum_inner", Value::Integer(9)),
                ("enum_via_base", Value::Integer(9)),
            ] {
                assert_eq!(
                    evaluate_function(numeric_function(modules, name), vec![], environment, 0),
                    Ok(expected),
                    "{name}"
                );
            }
            for name in [
                "sibling",
                "implicit",
                "primitive_sibling",
                "wrong_primitive",
                "skipped_enum_projection",
                "enum_sibling",
                "mixed_sibling",
                "mixed_sibling_reverse",
                "enum_implicit",
            ] {
                assert_eq!(
                    evaluate_function(numeric_function(modules, name), vec![], environment, 0)
                        .unwrap_err()
                        .reason,
                    Reason::Unsupported,
                    "{name}"
                );
            }
            assert_eq!(
                evaluate_function(numeric_function(modules, "arity"), vec![], environment, 0)
                    .unwrap_err()
                    .reason,
                Reason::ArityMismatch
            );
            let original = evaluate_function(
                numeric_function(modules, "enum_original"),
                vec![],
                environment,
                0,
            )
            .unwrap();
            let derived = evaluate_function(
                numeric_function(modules, "enum_identity"),
                vec![],
                environment,
                0,
            )
            .unwrap();
            assert_ne!(original.enum_owner(), derived.enum_owner());
            assert_eq!(source.word(original.clone()), Some(9));
            assert_eq!(source.word(derived.clone()), Some(9));
            let stored = evaluate_function(
                numeric_function(modules, "enum_argument"),
                vec![Value::Enum {
                    owner: original.enum_owner().unwrap(),
                    discriminant: 9,
                }],
                environment,
                0,
            )
            .unwrap();
            assert_eq!(
                stored,
                Value::Enum {
                    owner: derived.enum_owner().unwrap(),
                    discriminant: 9
                }
            );
        }
    });
}

#[test]
fn derived_comparisons_use_the_selected_implementation_not_storage_equality() {
    let mut sources = [
        "module client; use model::{Measure, Always, Plain, Base, Child}; use core::primitive::Flag;
         fn equal() -> Flag { return Measure(7) == Measure(7); }
         fn different() -> Flag { return Measure(7) != Measure(9); }
         fn override_equal() -> Flag { return Always(7) == Always(9); }
         fn enum_base_equal() -> Flag { return Base::A == Base::B; }
         fn enum_child_distinct() -> Flag { return Child::A != Child::B; }
         fn enum_explicit_base() -> Flag { return Base(Child::A) == Base(Child::B); }
         fn no_impl() -> Flag { return Plain(7) == Plain(7); }",
        "module model; use core::primitive::Flag; use core::ops::Equal;
         pub struct Measure(integer); pub struct Always(integer); pub struct Plain(integer);
         pub enum Base { A = 3, B = 9 }
         pub enum Child(Base);
         impl Equal<Measure> for Measure {
             fn eq(self, rhs: Measure) -> Flag { return self == rhs; }
         }
         impl Equal<Always> for Always {
             fn eq(self, rhs: Always) -> Flag { return Flag::true; }
         }
         impl Equal<Base> for Base {
             fn eq(self, rhs: Base) -> Flag { return Flag::true; }
         }
         impl core::ops::Eq<Plain> for Plain {
             fn eq(self, rhs: Plain) -> Flag { return Flag::true; }
         }
         impl core::ops::Eq<Always> for Always {
             fn eq(self, rhs: Always) -> Flag { return Flag::false; }
         }",
        "module core::ops; use core::primitive::Flag;
         pub trait Equal<Rhs> {
             fn eq(self, rhs: Rhs) -> Flag;
             fn ne(self, rhs: Rhs) -> Flag {
                 return if self.eq(rhs) { Flag::false } else { Flag::true };
             }
         }
         pub trait Eq<Rhs> { fn eq(self, rhs: Rhs) -> Flag; }
         attr lang for Equal = \"eq\";",
        CORE,
    ];
    for _ in 0..2 {
        with_numeric_source(&sources, |source, modules| {
            let native = NativeNumericSource(source);
            for environment in [&source as &dyn Environment, &native] {
                for name in [
                    "equal",
                    "different",
                    "override_equal",
                    "enum_base_equal",
                    "enum_child_distinct",
                    "enum_explicit_base",
                ] {
                    let value =
                        evaluate_function(numeric_function(modules, name), vec![], environment, 0)
                            .unwrap();
                    assert_eq!(source.condition_value(&value), Some(true), "{name}");
                }
                assert!(
                    evaluate_function(numeric_function(modules, "no_impl"), vec![], environment, 0)
                        .is_err(),
                    "derivation and a conventional trait name must not invent a language equality implementation"
                );
            }
        });
        sources.reverse();
    }
}

#[test]
fn derived_boundaries_reject_malformed_or_mismatched_representations() {
    let client = "module client; use L = providers::left; use R = providers::right;
        fn initial() -> L::Alias { return L::START; }
        fn envelope() -> L::Envelope { return L::PACKET; }
        fn packet() -> R::Packet { return R::PACKET; }
        fn retain_packet(value: L::Packet) -> L::Packet { return value; }
        fn retain_envelope(value: L::Envelope) -> L::Envelope { return value; }
        fn retain(value: L::Alias) -> L::Alias { return value; }";
    with_sources(client, |source, modules| {
        let native = NativeNumericSource(source);
        for environment in [&source as &dyn Environment, &native] {
            let initial =
                evaluate_function(numeric_function(modules, "initial"), vec![], environment, 0)
                    .unwrap();
            let Value::Record { owner, .. } = initial else {
                panic!("newtype")
            };
            for fields in [
                vec![],
                vec![Value::Integer(7), Value::Integer(9)],
                vec![Value::Real(7.0)],
                vec![Value::Integer(i128::from(i64::MAX) + 1)],
            ] {
                assert_eq!(
                    evaluate_function(
                        numeric_function(modules, "retain"),
                        vec![Value::Record {
                            owner,
                            fields: fields.into()
                        }],
                        environment,
                        0,
                    )
                    .unwrap_err()
                    .reason,
                    Reason::Unsupported
                );
            }
            let envelope = evaluate_function(
                numeric_function(modules, "envelope"),
                vec![],
                environment,
                0,
            )
            .unwrap();
            let packet =
                evaluate_function(numeric_function(modules, "packet"), vec![], environment, 0)
                    .unwrap();
            let Value::Record { owner, .. } = &envelope else {
                panic!("wrapper")
            };
            let Value::Record { fields: base, .. } = &envelope else {
                unreachable!()
            };
            let Value::Record {
                owner: packet_owner,
                ..
            } = &base[0]
            else {
                panic!("declared base record")
            };
            for fields in [
                vec![],
                vec![Value::Integer(3)],
                vec![Value::Integer(3), Value::Real(2.5), Value::Integer(7)],
                vec![Value::Real(3.0), Value::Real(2.5)],
                vec![Value::Integer(3), Value::Integer(2)],
            ] {
                let malformed = Value::Record {
                    owner: *packet_owner,
                    fields: fields.into(),
                };
                for (name, argument) in [
                    ("retain_packet", malformed.clone()),
                    (
                        "retain_envelope",
                        Value::Record {
                            owner: *owner,
                            fields: Arc::from([malformed]),
                        },
                    ),
                ] {
                    assert_eq!(
                        evaluate_function(
                            numeric_function(modules, name),
                            vec![argument],
                            environment,
                            0
                        )
                        .unwrap_err()
                        .reason,
                        Reason::Unsupported,
                        "{name}: validate the base record's fields, not only its owner"
                    );
                }
            }
            for gain in [-0.0f64, f64::NAN] {
                let fields: Arc<[Value]> = Arc::from([Value::Integer(3), Value::Real(gain)]);
                let valid = Value::Record {
                    owner: *packet_owner,
                    fields: Arc::clone(&fields),
                };
                let retained = evaluate_function(
                    numeric_function(modules, "retain_packet"),
                    vec![valid],
                    environment,
                    0,
                )
                .unwrap();
                let Value::Record { fields: actual, .. } = retained else {
                    panic!("record")
                };
                assert!(
                    Arc::ptr_eq(&fields, &actual),
                    "unchanged valid fields keep their storage"
                );
            }
            let wrong = Value::Record {
                owner: *owner,
                fields: Arc::from([packet]),
            };
            let interpreter = Interpreter {
                environment,
                locals: Vec::new(),
                operators: Vec::new(),
            };
            let field = Ident {
                text: "width".into(),
                span: Span::new(FileId(0), 0..1),
            };
            assert_eq!(
                interpreter.project_field(envelope, &field),
                Some(Value::Integer(3))
            );
            assert_eq!(
                interpreter.project_field(wrong, &field),
                None,
                "a foreign Packet cannot impersonate a wrapper's declared base"
            );
        }
    });
}

#[test]
fn explicit_from_bodies_take_precedence_over_automatic_derivation() {
    let mut sources = [
        "module client; use model::{Base, Changed, Amount, Opaque, Default, DefaultEnum, Missing, Ambiguous, Generic, Fallback, FallbackDefault, Unregistered};
         fn mapped() -> integer { return integer(Changed(Base::A)); }
         fn record() -> integer { return Amount(2).value; }
         fn default_struct() -> integer { return integer(Default(2)); }
         fn default_enum() -> integer { return integer(DefaultEnum(Base::A)); }
         fn fallback() -> integer { return Fallback(2).value; }
         fn fallback_default() -> integer { return integer(FallbackDefault(2)); }
         fn unregistered() -> Unregistered { return Unregistered(2); }
         fn missing() -> Missing { return Missing(Base::A); }
         fn ambiguous() -> Ambiguous { return Ambiguous(Base::A); }
         fn generic() -> integer { return integer(Generic(Base::A)); }
         fn implicit() -> Changed { return Base::A; }
         fn impure() -> Opaque { return Opaque(Base::A); }",
        "module model; use core::convert::Make;
         pub enum Base { A = 3, B = 9 }
         pub enum Changed(Base);
         pub enum Opaque(Base);
         pub enum Missing(Base);
         pub enum Ambiguous(Base);
         pub enum Generic(Base);
         pub enum DefaultEnum(Base);
         pub struct Default(integer);
         pub struct Amount { pub value: integer }
         pub struct Fallback { pub value: integer }
         pub struct FallbackDefault(integer);
         pub struct Unregistered { pub value: integer }
         fn foreign() -> integer;
         impl Make<integer> for Default {}
         impl From<integer> for Fallback {
             static fn from(value: integer) -> Self { return Self { .value = value + 20 }; }
         }
         impl From<integer> for FallbackDefault {}
         impl core::convert::From<integer> for Unregistered {
             static fn from(value: integer) -> Self { return Self { .value = value + 99 }; }
         }
         impl Make<Base> for DefaultEnum {}
         impl Make<Base> for Missing { static fn from(value: Base) -> Self; }
         impl Make<Base> for Ambiguous {
             static fn from(value: Base) -> Self { return Ambiguous::A; }
         }
         impl Make<Base> for Ambiguous {
             static fn from(value: Base) -> Self { return Ambiguous::B; }
         }
         impl<S> Make<S> for Generic {
             static fn from(value: S) -> Self { return Generic::B; }
         }
         impl Make<Base> for Changed {
             static fn from(value: Base) -> Self { return Changed::B; }
         }
         impl Make<integer> for Amount {
             static fn from(value: integer) -> Self { return Self { .value = value + 10 }; }
         }
         impl Make<Base> for Opaque {
             static fn from(value: Base) -> Self {
                 let unused: integer = foreign();
                 return Opaque::A;
             }
         }",
        "module core::convert;
         pub trait Make<Source> {
             static fn from(value: Source) -> Self { return Self(value); }
         }
         pub trait From<Source> { static fn from(value: Source) -> Self; }
         attr lang for Make = \"from\";",
        CORE,
    ];
    for _ in 0..2 {
        with_numeric_source(&sources, |source, modules| {
            let native = NativeNumericSource(source);
            for environment in [&source as &dyn Environment, &native] {
                for (name, expected) in [
                    ("mapped", 9),
                    ("record", 12),
                    ("default_struct", 2),
                    ("default_enum", 3),
                    ("fallback", 22),
                    ("fallback_default", 2),
                ] {
                    assert_eq!(
                        evaluate_function(numeric_function(modules, name), vec![], environment, 0),
                        Ok(Value::Integer(expected)),
                        "{name}"
                    );
                }
                assert!(
                    evaluate_function(
                        numeric_function(modules, "unregistered"),
                        vec![],
                        environment,
                        0
                    )
                    .is_err(),
                    "an unregistered same-named trait cannot provide a From conversion"
                );
                assert!(
                    evaluate_function(numeric_function(modules, "missing"), vec![], environment, 0)
                        .is_err(),
                    "a selected declaration without a body must not use automatic derivation"
                );
                assert!(
                    evaluate_function(
                        numeric_function(modules, "ambiguous"),
                        vec![],
                        environment,
                        0
                    )
                    .is_err(),
                    "ambiguous From bodies must not use automatic derivation"
                );
                match evaluate_function(
                    numeric_function(modules, "generic"),
                    vec![],
                    environment,
                    0,
                ) {
                    Ok(value) => assert_eq!(
                        value,
                        Value::Integer(9),
                        "select the generic body, never automatic Base::A"
                    ),
                    Err(error) => assert_eq!(error.reason, Reason::Unsupported),
                }
                assert!(
                    evaluate_function(numeric_function(modules, "impure"), vec![], environment, 0)
                        .is_err(),
                    "automatic derivation must not bypass an applicable effectful From body"
                );
                assert!(
                    evaluate_function(
                        numeric_function(modules, "implicit"),
                        vec![],
                        environment,
                        0
                    )
                    .is_err(),
                    "From supplies explicit constructors, not implicit Into conversions"
                );
            }
        });
        sources.reverse();
    }
}

#[test]
fn recursive_derived_contracts_do_not_recurse_without_a_bound() {
    let mut sink = DiagnosticSink::default();
    let modules = [parse_module(
        FileId(0),
        "module cycle;
        struct A(B); struct B(A);
        fn construct() -> A { return A(1); }",
        &mut sink,
    )];
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let errors = sink
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 2, "{:?}", sink.diagnostics());
    assert!(
        errors.iter().all(|diagnostic| {
            diagnostic.code == Some(crate::diag::codes::DUPLICATE_ITEM)
                && diagnostic.message.contains("defined in terms of itself")
        }),
        "{errors:?}"
    );
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let source = Source::new(&declarations);
    assert!(!source.supports_function(numeric_function(&modules, "construct")));
    assert!(
        evaluate_function(numeric_function(&modules, "construct"), vec![], &source, 0).is_err()
    );
}
