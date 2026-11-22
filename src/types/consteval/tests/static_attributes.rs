use super::numeric::NativeNumericSource;
use super::*;

#[test]
fn static_field_attributes_keep_applied_owners_without_runtime_contents() {
    let mut sources = [
        "module left;
         use core::primitive::Flag;
         const N: integer = 99;
         pub struct Payload<T, N: integer> { pub data: T[N],
             pub backwards: integer[6..2], pub negative: integer[-2..1] }
         pub struct Derived<T, N: integer>(Payload<T, N>);
         pub type Packet = Derived<integer, 5>;
         pub struct Bus { pub payload: Packet }
         struct Local { payload: integer[3] }
         pub const bus: integer = 999;
         pub entity Device { bus: Bus in }
         impl Device {
             const length: integer = bus.payload.data'length;
             const left: integer = bus.payload.data'left;
             const right: integer = bus.payload.data'right;
             const backwards: integer = bus.payload.backwards'length;
             const low: integer = bus.payload.backwards'low;
             const high: integer = bus.payload.backwards'high;
             const ascending: Flag = bus.payload.backwards'ascending;
             const negative: integer = bus.payload.negative'left;
             const runtime: integer = bus.payload.data[0];
             const event: Flag = bus.payload.data'event;
             const computed: integer = unknown().payload.data'length;
             static fn shadow(bus: Local) -> integer { return bus.payload'length; }
             static fn local() -> integer {
                 let bus: Local = { .payload = [8, 9, 10] };
                 return bus.payload'length;
             }
             static fn global() -> integer { return bus; }
         }
         fn unknown() -> Bus { return unknown(); }",
        "module right;
         pub struct Payload<T, N: integer> { pub data: T[N] }
         pub struct Bus { pub payload: Payload<integer, 7> }
         pub entity Device { bus: Bus in }
         impl Device { const length: integer = bus.payload.data'length; }",
        "module core::primitive;
         pub enum Flag { false = 7, true = 19 } attr lang for Flag = \"bool\";",
    ];
    for _ in 0..2 {
        let mut sink = DiagnosticSink::default();
        let mut modules: Vec<_> = sources
            .iter()
            .enumerate()
            .map(|(index, source)| parse_module(FileId(index as u32), source, &mut sink))
            .collect();
        crate::syntax::imports::desugar(&mut modules, &mut sink);
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        let declarations = DeclarationIndex::collect(&resolved, &modules);
        let enums = crate::types::enum_values::EnumValues::collect(&declarations);
        let source = Source::new(&declarations).with_enums(&enums);
        let native = NativeNumericSource(source);
        for module in modules
            .iter()
            .filter(|module| module.path.segments[0].text != "core")
        {
            let left = module.path.segments[0].text == "left";
            let local = module.items.iter().find_map(|item| match item {
                Item::Struct(structure) if structure.name.text == "Local" => {
                    resolved.declared(structure.name.span)
                }
                _ => None,
            });
            for member in module
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::Impl(implementation) => Some(&implementation.items),
                    _ => None,
                })
                .flatten()
            {
                let name = match member {
                    crate::syntax::ast::ImplItem::Fn(function) => &function.name.text,
                    crate::syntax::ast::ImplItem::Const(constant) => &constant.name.text,
                    _ => continue,
                }
                .as_str();
                let expected = if !left {
                    Some(7)
                } else {
                    match name {
                        "length" | "backwards" => Some(5),
                        "left" => Some(0),
                        "right" => Some(4),
                        "low" => Some(2),
                        "high" => Some(6),
                        "negative" => Some(-2),
                        "shadow" | "local" => Some(3),
                        "global" => Some(999),
                        _ => None,
                    }
                };
                for environment in [&source as &dyn Environment, &native] {
                    let result = match member {
                        crate::syntax::ast::ImplItem::Const(constant) => {
                            evaluate_value(&constant.value, environment, None, 0)
                        }
                        crate::syntax::ast::ImplItem::Fn(function) => {
                            let arguments = if name == "shadow" {
                                vec![Value::Record {
                                    owner: local.unwrap(),
                                    fields: vec![Value::Array {
                                        element: Arc::new(Ty::Integer),
                                        values: vec![
                                            Value::Integer(1),
                                            Value::Integer(2),
                                            Value::Integer(3),
                                        ]
                                        .into(),
                                        left: 0,
                                        descending: false,
                                    }]
                                    .into(),
                                }]
                            } else {
                                vec![]
                            };
                            evaluate_function(function, arguments, environment, 0)
                        }
                        _ => unreachable!(),
                    };
                    if let Some(expected) = expected {
                        assert_eq!(result, Ok(Value::Integer(expected)), "{name}");
                    } else if name == "ascending" {
                        assert_eq!(source.condition_value(&result.unwrap()), Some(false));
                    } else {
                        let error = result.unwrap_err();
                        assert!(error.span.start < error.span.end, "{name}");
                    }
                }
            }
        }
        sources.reverse();
    }
}

#[test]
fn static_port_attributes_borrow_owner_types_without_reading_signal_values() {
    let mut sources = [
        "module providers::left;
         use core::primitive::Flag;
         pub const input: integer = 999;
         fn count() -> integer { return 6; }
         pub enum Level { '0', '1' }
         pub struct Word(Level[]);
         pub type Incoming = integer[5..2];
         pub entity Device { input: Incoming in, data: integer[count()] in,
                             bits: Word[4] in, negative: integer[-2..1] in }
         impl Device {
             const length: integer = input'length;
             const left: integer = input'left;
             const right: integer = input'right;
             const low: integer = input'low;
             const high: integer = input'high;
             const ascending: Flag = input'ascending;
             const data_size: integer = data'length;
             const data_left: integer = data'left;
             const data_right: integer = data'right;
             const bits_left: integer = bits'left;
             const bits_right: integer = bits'right;
             const bits_ascending: Flag = bits'ascending;
             const negative_left: integer = negative'left;
             const negative_right: integer = negative'right;
             static fn shadow(input: integer[3]) -> integer { return input'length; }
             static fn local() -> integer { let input: integer[2] = [8, 9]; return input'length; }
             static fn global() -> integer { return input; }
             const runtime: integer = input[2];
             const event: Flag = input'event;
         }",
        "module providers::right;
         pub entity Device { input: integer[8..10] in }
         impl Device { const length: integer = input'length; }",
        "module core::primitive;
         pub enum Flag { false = 7, true = 19 } attr lang for Flag = \"bool\";",
    ];
    for _ in 0..2 {
        let mut sink = DiagnosticSink::default();
        let mut modules: Vec<_> = sources
            .iter()
            .enumerate()
            .map(|(index, source)| parse_module(FileId(index as u32), source, &mut sink))
            .collect();
        crate::syntax::imports::desugar(&mut modules, &mut sink);
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        let declarations = DeclarationIndex::collect(&resolved, &modules);
        let enums = crate::types::enum_values::EnumValues::collect(&declarations);
        let source = Source::new(&declarations).with_enums(&enums);
        {
            let native = NativeNumericSource(source);
            let mut ports = Vec::new();
            for module in modules
                .iter()
                .filter(|module| module.path.segments[0].text == "providers")
            {
                let left = module.path.segments[1].text == "left";
                let entity = module
                    .items
                    .iter()
                    .find_map(|item| match item {
                        Item::Entity(entity) => Some(entity),
                        _ => None,
                    })
                    .unwrap();
                let owner = resolved.declared(entity.name.span).unwrap();
                let port = resolved.port(owner, "input").unwrap();
                assert_eq!(resolved.def(port).unwrap().parent, Some(owner));
                assert!(std::ptr::eq(
                    declarations.value_type(port).unwrap(),
                    &entity.ports[0].ty
                ));
                ports.push(port);
                for member in module
                    .items
                    .iter()
                    .filter_map(|item| match item {
                        Item::Impl(implementation) => Some(&implementation.items),
                        _ => None,
                    })
                    .flatten()
                {
                    let name = match member {
                        crate::syntax::ast::ImplItem::Fn(function) => &function.name.text,
                        crate::syntax::ast::ImplItem::Const(constant) => &constant.name.text,
                        _ => continue,
                    }
                    .as_str();
                    let expected = if !left {
                        Some(3)
                    } else {
                        match name {
                            "length" => Some(4),
                            "left" | "high" => Some(5),
                            "right" | "low" => Some(2),
                            "data_size" => Some(6),
                            "data_left" | "bits_left" => Some(0),
                            "data_right" => Some(5),
                            "bits_right" => Some(3),
                            "negative_left" => Some(-2),
                            "negative_right" => Some(1),
                            "shadow" => Some(3),
                            "local" => Some(2),
                            "global" => Some(999),
                            _ => None,
                        }
                    };
                    for environment in [&source as &dyn Environment, &native] {
                        let arguments = if name == "shadow" {
                            vec![Value::Array {
                                element: Arc::new(Ty::Integer),
                                values: vec![
                                    Value::Integer(1),
                                    Value::Integer(2),
                                    Value::Integer(3),
                                ]
                                .into(),
                                left: 0,
                                descending: false,
                            }]
                        } else {
                            vec![]
                        };
                        let result = match member {
                            crate::syntax::ast::ImplItem::Fn(function) => {
                                evaluate_function(function, arguments, environment, 0)
                            }
                            crate::syntax::ast::ImplItem::Const(constant) => {
                                evaluate_value(&constant.value, environment, None, 0)
                            }
                            _ => unreachable!(),
                        };
                        if let Some(expected) = expected {
                            assert_eq!(result, Ok(Value::Integer(expected)), "{name}");
                        } else if matches!(name, "ascending" | "bits_ascending") {
                            assert_eq!(
                                source.condition_value(&result.unwrap()),
                                Some(name == "bits_ascending"),
                                "{name}"
                            );
                        } else {
                            let error = result.unwrap_err();
                            assert!(error.span.start < error.span.end, "{name}");
                        }
                    }
                }
            }
            assert_eq!(ports.len(), 2);
            assert_ne!(
                ports[0], ports[1],
                "equal port names have distinct owning identities"
            );
        }
        sources.reverse();
    }
}
