use super::*;
use crate::diag::{DiagnosticSink, FileId};
use crate::resolve::declarations::DeclarationIndex;
use crate::syntax::ast::Expr;
use crate::types::consteval::{evaluate, Reason};

fn parse(sources: &[&str]) -> (Vec<Module>, Resolved) {
    let mut sink = DiagnosticSink::default();
    let modules: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(i, source)| crate::syntax::parse_module(FileId(i as u32), source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    (modules, resolved)
}

fn constant<'a>(module: &'a Module, name: &str) -> &'a ConstDecl {
    module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Const(declaration) if declaration.name.text == name => Some(declaration),
            _ => None,
        })
        .unwrap()
}

#[test]
fn const_context_pure_scalar_locals_and_assignments_follow_execution_order() {
    let (modules, resolved) = parse(&[
        "module model;
         type Count = integer;
         fn calculate(seed: integer) -> Count {
             seed = seed + 1;
             let result: Count = seed;
             if true { let extra: integer = 5; result = result + extra; }
             result = result * 2;
             return result;
         }
         const RESULT: integer = calculate(8);",
        "module core::primitive;
         pub enum Decision { false = 5, true = 17 }
         attr lang for Decision = \"bool\";",
    ]);
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    assert_eq!(
        evaluate(&constant(&modules[0], "RESULT").value, &declarations, 0),
        Ok(28)
    );
}

#[test]
fn scalar_calls_do_not_erase_non_scalar_local_declarations() {
    for ty in ["real", "natural", "integer[3]", "integer[1..3]"] {
        let source = format!(
            "module model;
             type Local = {ty};
             fn discard(value: integer) -> integer {{
                 let local: Local = value;
                 return 3;
             }}
             const RESULT: integer = discard(7);"
        );
        let (modules, resolved) = parse(&[&source]);
        let declarations = DeclarationIndex::collect(&resolved, &modules);
        assert_eq!(
            evaluate(&constant(&modules[0], "RESULT").value, &declarations, 0)
                .unwrap_err()
                .reason,
            Reason::Unsupported,
            "{ty}"
        );
    }
}

#[test]
fn const_context_pure_scalar_locals_keep_lexical_shadowing() {
    let (modules, resolved) = parse(&[
        "module model;
         const result: integer = 100;
         fn identity(value: integer) -> integer { return value; }
         fn calculate() -> integer {
             let result: integer = 7;
             if true { let result: integer = 30; result = result + 1; }
             return identity(result);
         }
         const RESULT: integer = calculate();",
        "module core::primitive;
         pub enum Decision { false = 5, true = 17 }
         attr lang for Decision = \"bool\";",
    ]);
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    assert_eq!(
        evaluate(&constant(&modules[0], "RESULT").value, &declarations, 0),
        Ok(7)
    );
}

#[test]
fn const_context_pure_scalar_locals_retain_enum_literal_context() {
    let (modules, resolved) = parse(&[
        "module model;
         use core::primitive::Decision;
         enum Code { 'L' = 9, 'H' }
         fn calculate() -> integer {
             let chosen: Code = 'L';
             chosen = 'H';
             let flag: Decision = false;
             if flag { return 99; }
             return integer(chosen);
         }
         const RESULT: integer = calculate();",
        "module core::primitive;
         pub enum Decision { false = 5, true = 17 }
         attr lang for Decision = \"bool\";",
    ]);
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let enums = crate::types::enum_values::EnumValues::collect(&declarations);
    let environment = crate::types::consteval::Source::new(&declarations).with_enums(&enums);
    assert_eq!(
        evaluate(&constant(&modules[0], "RESULT").value, &environment, 0),
        Ok(10)
    );
}

#[test]
fn const_context_pure_scalar_recursive_calls_do_not_capture_caller_locals() {
    let (modules, resolved) = parse(&[
        "module model;
         fn recursive(remaining: integer) -> integer {
             if remaining == 0 { return stored; }
             let stored: integer = 7;
             return recursive(remaining - 1);
         }
         const RESULT: integer = recursive(1);",
        "module core::primitive;
         pub enum Decision { false = 5, true = 17 }
         attr lang for Decision = \"bool\";",
    ]);
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    assert_eq!(
        evaluate(&constant(&modules[0], "RESULT").value, &declarations, 0)
            .unwrap_err()
            .reason,
        Reason::UnknownConstant
    );
}

#[test]
fn const_context_pure_scalar_receiver_locals_retain_self_type() {
    let (modules, resolved) = parse(&["module model;
         enum Code { 'L' = 9, 'H' }
         impl Code {
             pub fn code(self) -> integer {
                 let selected: Self = 'L';
                 selected = 'H';
                 return integer(selected);
             }
         }
         fn calculate(value: Code) -> integer { return value.code(); }
         const RESULT: integer = calculate('L');"]);
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let enums = crate::types::enum_values::EnumValues::collect(&declarations);
    let environment = crate::types::consteval::Source::new(&declarations).with_enums(&enums);
    assert_eq!(
        evaluate(&constant(&modules[0], "RESULT").value, &environment, 0),
        Ok(10)
    );
}

#[test]
fn const_context_pure_scalar_locals_reject_nonlocal_and_delayed_writes() {
    for body in [
        "TARGET = 2; return TARGET;",
        "let value: integer = 1; value = 2 after 1; return value;",
        "let value: integer; return value;",
    ] {
        let source = format!(
            "module model; const TARGET: integer = 7;
            fn calculate() -> integer {{ {body} }}
            const RESULT: integer = calculate();"
        );
        let (modules, resolved) = parse(&[&source]);
        let declarations = DeclarationIndex::collect(&resolved, &modules);
        assert_eq!(
            evaluate(&constant(&modules[0], "RESULT").value, &declarations, 0)
                .unwrap_err()
                .reason,
            Reason::Unsupported,
            "{body}"
        );
    }
}

#[test]
fn const_context_registered_boolean_conditions_are_available_to_integer_queries() {
    let (modules, resolved) = parse(&[
        "module model;
        fn choose() -> integer {
            if 2 > 4 { return 99; }
            if 2 < 4 { return 3; }
            return 1;
        }
        const RESULT: integer = choose();",
        "module core::primitive;
        pub enum Decision { false = 5, true = 17 }
        attr lang for Decision = \"bool\";",
    ]);
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    assert_eq!(
        evaluate(&constant(&modules[0], "RESULT").value, &declarations, 0),
        Ok(3)
    );
}

#[test]
fn const_context_checker_queries_use_shared_enum_encodings() {
    let (modules, resolved) = parse(&["module model;
        enum Code { First = 9, Next }
        fn width() -> integer { return integer(Code::Next); }
        const WIDTH: integer = width();"]);
    let mut sink = DiagnosticSink::default();
    let mut checker = Checker::new(&mut sink, &resolved, &modules);
    checker.collect(&modules);
    assert_eq!(
        checker.const_int(&constant(&modules[0], "WIDTH").value),
        Some(10)
    );
}

#[test]
fn const_context_checker_queries_share_scalar_receiver_methods() {
    let (modules, resolved) = parse(&["module model;
        enum Code { First = 9, Next }
        trait Other { fn code(self) -> integer; }
        impl Other for Code { fn code(self) -> integer { return 99; } }
        impl Code { pub fn code(self) -> integer { return integer(self); } }
        const RESULT: integer = Code::Next.code();"]);
    let mut sink = DiagnosticSink::default();
    let mut checker = Checker::new(&mut sink, &resolved, &modules);
    checker.collect(&modules);
    assert_eq!(
        checker.const_int(&constant(&modules[0], "RESULT").value),
        Some(10)
    );
}

#[test]
fn const_context_scalar_methods_keep_owner_ids_and_borrow_trait_defaults() {
    let sources = [
        "module left; enum Code { Low = 9, High }
         impl Code { pub fn code(self) -> integer { return integer(self); } }
         const RESULT: integer = Code::High.code();",
        "module right; enum Code { Low = 3, High }
         trait Inspect { fn code(self) -> integer { return 17; } }
         impl Inspect for Code {}
         const RESULT: integer = Code::High.code();",
    ];
    for sources in [sources, [sources[1], sources[0]]] {
        let (modules, resolved) = parse(&sources);
        let mut sink = DiagnosticSink::default();
        let mut checker = Checker::new(&mut sink, &resolved, &modules);
        checker.collect(&modules);
        for module in &modules {
            let expected = if module.path.segments[0].text == "left" {
                10
            } else {
                17
            };
            assert_eq!(
                checker.const_int(&constant(module, "RESULT").value),
                Some(expected)
            );
        }
    }
}

#[test]
fn enum_contracts_borrow_inherited_variants_by_resolved_owner() {
    let sources = [
        "module left; pub enum Base { Low = 9, High } pub enum Mode(Base);",
        "module right; pub enum Base { Low = 3, High } pub enum Mode(Base);",
    ];
    for sources in [sources, [sources[1], sources[0]]] {
        let (modules, resolved) = parse(&sources);
        let index = DeclarationIndex::collect(&resolved, &modules);
        assert_eq!(index.enumerations().count(), 4);
        let mut owners = Vec::new();
        for module in &modules {
            let Item::Enum(base) = &module.items[0] else {
                panic!("base")
            };
            let Item::Enum(derived) = &module.items[1] else {
                panic!("derived")
            };
            let base_id = resolved.declared(base.name.span).unwrap();
            let owner = resolved.declared(derived.name.span).unwrap();
            owners.push(owner);
            assert_ne!(owner, base_id);
            assert!(std::ptr::eq(index.enumeration(owner).unwrap(), derived));
            assert_eq!(index.enum_base(owner), Some(base_id));
            let variants = index.enum_variants(owner).unwrap();
            assert_eq!(variants.len(), 2);
            for ((id, actual), expected) in variants.iter().zip(&base.variants) {
                assert_eq!(Some(*id), resolved.declared(expected.name.span));
                assert!(std::ptr::eq(*actual, expected));
            }
        }
        assert_ne!(owners[0], owners[1]);
    }
}

#[test]
fn checker_enum_domains_share_inherited_members_even_when_values_are_unknown() {
    let sources = [
        "module left;
         extern \"C\" { fn runtime() -> integer; }
         pub enum Base { Low = runtime(), High }
         pub type Alias = Base;
         pub enum Mode(Alias);
         pub enum Top(Mode);",
        "module right;
         pub enum Base { Low = 3, High }
         pub enum Mode(Base);
         pub enum Top(Mode);",
    ];
    for sources in [sources, [sources[1], sources[0]]] {
        let (modules, resolved) = parse(&sources);
        let mut sink = DiagnosticSink::default();
        let mut checker = Checker::new(&mut sink, &resolved, &modules);
        checker.collect(&modules);
        let mut owners = HashMap::new();
        for (owner, _) in checker.declarations.enumerations() {
            let name = resolved.qualified_name_str(owner).unwrap();
            let members = checker.enum_values.members(owner).unwrap();
            let names = members
                .iter()
                .map(|(member, _)| resolved.def(*member).unwrap().name.clone())
                .collect::<Vec<_>>();
            assert_eq!(
                checker
                    .enum_member_names(owner)
                    .unwrap()
                    .collect::<Vec<_>>(),
                names
            );
            assert_eq!(names, ["Low", "High"]);
            if name.starts_with("left::") {
                assert!(members.iter().all(|(_, value)| value.is_none()));
            }
            owners.insert(name.to_owned(), owner);
        }
        for module in ["left", "right"] {
            let base = Ty::Named(owners[&format!("{module}::Base")]);
            let top = Ty::Named(owners[&format!("{module}::Top")]);
            assert!(checker.conversion_exists(&base, &top));
            assert!(checker.conversion_exists(&top, &base));
        }
        assert!(!checker.conversion_exists(
            &Ty::Named(owners["left::Top"]),
            &Ty::Named(owners["right::Top"]),
        ));
        assert!(!checker.conversion_exists(
            &Ty::Named(owners["right::Base"]),
            &Ty::Named(owners["left::Top"]),
        ));
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    }
}

#[test]
fn conversion_targets_keep_resolved_owners_through_renamed_aliases() {
    let sources = [
        "module left;
         pub enum Mode { Low = 9, High = 17 }
         pub type Alias = Mode;",
        "module right;
         pub entity Mode {}
         pub type Alias = Mode;",
        "module caller;
         use Local = left::Alias;
         use Remote = right::Alias;
         type RemoteGroup = Remote[2];
         type Numbers = integer[2];
         type Modes = Local[2];
         const ENUM: Local = Local();
         const ENTITY: Remote = Remote();
         const GROUP: RemoteGroup = RemoteGroup();
         const ARRAY: Numbers = Numbers();
         const VARIANT: Local = Local::High;
         const NOT_VARIANT: Local = Modes::High;
         const DEFAULT: Local = Local::new();
         const NOT_DEFAULT: Remote = Remote::new();
         const TEXT: string = string();
         const NUMBER: integer = integer();",
    ];
    for sources in [sources, [sources[2], sources[1], sources[0]]] {
        let (modules, resolved) = parse(&sources);
        let mut sink = DiagnosticSink::default();
        let mut checker = Checker::new(&mut sink, &resolved, &modules);
        checker.collect(&modules);
        let caller = modules
            .iter()
            .find(|module| module.path.segments[0].text == "caller")
            .unwrap();
        let callee = |name| {
            let Expr::Call { callee, .. } = &constant(caller, name).value else {
                panic!("{name} must be a call")
            };
            let Expr::Path(path) = callee.as_ref() else {
                panic!("{name} must name a path")
            };
            path
        };
        let enumeration = checker.ast_ty(&constant(caller, "ENUM").ty);
        assert!(checker.is_enum_ty(&enumeration));
        let variant_value = &constant(caller, "VARIANT").value;
        let Expr::Path(variant_path) = variant_value else {
            panic!("variant path")
        };
        let (owner, variant) = checker.declarations.enum_variant(variant_path).unwrap();
        let Expr::Path(array_variant) = &constant(caller, "NOT_VARIANT").value else {
            panic!("path")
        };
        assert!(checker.declarations.enum_variant(array_variant).is_none());
        assert_eq!(enumeration, Ty::Named(owner));
        assert_eq!(checker.type_of(variant_value, &HashMap::new()), enumeration);
        assert_eq!(
            consteval::evaluate_value(
                variant_value,
                &consteval::Source::new(&checker.declarations).with_enums(&checker.enum_values),
                None,
                0
            ),
            Ok(consteval::Value::Variant { owner, variant })
        );
        assert_eq!(
            checker.conversion_ty(callee("ENUM")),
            Some(enumeration.clone())
        );
        assert_eq!(
            checker.associated_conversion_ty(callee("DEFAULT")),
            Some(enumeration)
        );
        assert!(checker.callee_is_declared(callee("ENUM")));
        assert!(checker.conversion_ty(callee("ENTITY")).is_none());
        assert!(checker
            .associated_conversion_ty(callee("NOT_DEFAULT"))
            .is_none());
        assert!(!checker.callee_is_declared(callee("ENTITY")));
        assert!(checker.conversion_ty(callee("GROUP")).is_none());
        assert!(!checker.callee_is_declared(callee("GROUP")));
        assert!(matches!(
            checker.conversion_ty(callee("ARRAY")),
            Some(Ty::Array { len: 2, .. })
        ));
        assert!(checker.conversion_ty(callee("TEXT")).is_some());
        assert_eq!(checker.conversion_ty(callee("NUMBER")), Some(Ty::Integer));
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    }
}

#[test]
fn enum_contract_queries_do_not_invent_members_of_a_derivation_cycle() {
    let mut sink = DiagnosticSink::default();
    let module =
        crate::syntax::parse_module(FileId(0), "module model; enum A(B); enum B(A);", &mut sink);
    let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
    assert!(sink.has_errors(), "the source cycle remains diagnosed");
    let index = DeclarationIndex::collect(&resolved, std::slice::from_ref(&module));
    for item in &module.items {
        let Item::Enum(declaration) = item else {
            continue;
        };
        let id = resolved.declared(declaration.name.span).unwrap();
        assert!(index.enum_variants(id).is_none());
    }
}

#[test]
fn generated_enum_members_keep_their_resolved_declaration_owner() {
    let mut sink = DiagnosticSink::default();
    let mut modules = vec![crate::syntax::parse_module(
        FileId(0),
        "module model;
        macro modes($Name: ident, $start: expr) { enum $Name { Low = $start, High } }
        modes!(Left, 9);
        modes!(Right, 3);
        enum Derived(Left);",
        &mut sink,
    )];
    crate::syntax::macros::expand(&mut modules, &HashMap::new(), &mut sink);
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let enums: Vec<_> = modules[0]
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Enum(enumeration) => Some(enumeration),
            _ => None,
        })
        .collect();
    assert_eq!(enums.len(), 3);
    assert_eq!(
        enums[0].variants[0].name.span,
        enums[1].variants[0].name.span
    );
    for enumeration in &enums[..2] {
        let owner = resolved.declared(enumeration.name.span).unwrap();
        for ((id, source), expected) in declarations
            .enum_variants(owner)
            .unwrap()
            .iter()
            .zip(&enumeration.variants)
        {
            assert_eq!(resolved.def(*id).unwrap().parent, Some(owner));
            assert!(std::ptr::eq(*source, expected));
        }
    }
    let left = resolved.declared(enums[0].name.span).unwrap();
    let derived = resolved.declared(enums[2].name.span).unwrap();
    for (id, _) in declarations.enum_variants(derived).unwrap() {
        assert_eq!(resolved.def(id).unwrap().parent, Some(left));
    }
    let right = resolved.declared(enums[1].name.span).unwrap();
    let checker = Checker::new(&mut sink, &resolved, &modules);
    for owner in [left, right, derived] {
        let names = checker
            .enum_member_names(owner)
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(names, ["Low", "High"]);
        for (name, (member, _)) in names
            .iter()
            .zip(checker.enum_values.members(owner).unwrap())
        {
            assert!(std::ptr::eq(
                *name,
                resolved.def(*member).unwrap().name.as_str()
            ));
        }
    }
    assert!(checker.conversion_exists(&Ty::Named(derived), &Ty::Named(left)));
    assert!(checker.conversion_exists(&Ty::Named(left), &Ty::Named(derived)));
    assert!(!checker.conversion_exists(&Ty::Named(derived), &Ty::Named(right)));
}

#[test]
fn generic_aliases_substitute_resolved_type_and_value_parameters() {
    let (modules, resolved) = parse(&[
        "module provider; const BASE: integer = 6; const N: integer = 40;
         pub entity Device<T, N: integer> {}
         pub type Pipe<T, N: integer> = Device<T, (N + BASE)>;
         pub type Route<T, Count: integer> = Pipe<T, Count>;
         pub type Group<T, Count: integer> = Route<T, Count>[2];
         pub type Select<T> = T;",
        "module caller; use provider::{Route, Group, Select, Pipe}; const BASE: integer = 99;
         const a: Route<real, 3> = 0; const b: Route<Count = 4, T = integer> = 0;
         const group: Group<integer, 4> = 0; const forwarded: Select<Pipe<integer, 5>> = 0;",
    ]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    for (name, expected) in [("a", 9), ("b", 10), ("forwarded", 11)] {
        let ty = &constant(&modules[1], name).ty;
        assert!(index.entity_type_id(ty).is_some(), "{name}");
        let normalized = index.normalize_type(ty).unwrap();
        let Type::Generic { args, .. } = normalized.as_ref() else {
            panic!("{name}: {normalized:?}")
        };
        let GenericArg::Positional(value) = &args[1] else {
            panic!("{args:?}")
        };
        assert_eq!(evaluate(value, &index, 0), Ok(expected));
        assert!(matches!(args[0], GenericArg::PositionalType(_)));
    }
    let group = index
        .normalize_type(&constant(&modules[1], "group").ty)
        .unwrap();
    assert!(
        matches!(group.as_ref(), Type::Indexed { base, .. } if matches!(base.as_ref(), Type::Generic { .. }))
    );
}

#[test]
fn nested_applications_are_not_alias_definition_cycles() {
    let (modules, resolved) = parse(&["module m; type Select<T> = T;
        const x: Select<Select<integer>> = 0;"]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    let normalized = index
        .normalize_type(&constant(&modules[0], "x").ty)
        .unwrap();
    assert!(matches!(normalized.as_ref(), Type::Path(path) if path.segments[0].text == "integer"));
}

#[test]
fn prepared_trait_arguments_borrow_values_and_reuse_resolved_bindings() {
    let (modules, resolved) = parse(&["module model;
        trait Contract<T, N: integer> { fn inspect(self, value: T[N]) -> T[N]; }
        struct Value {} impl Contract<N = 3, T = real> for Value {
            fn inspect(self, value: real[3]) -> real[3] { return value; }
        }"]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    let Item::Trait(contract) = &modules[0].items[0] else {
        panic!("trait")
    };
    let Item::Impl(implementation) = &modules[0].items[2] else {
        panic!("impl")
    };
    let arguments = index
        .arguments(&contract.params, &implementation.trait_args)
        .unwrap();
    let parameter = resolved
        .declared(contract.params.params[1].name.span)
        .unwrap();
    let GenericArg::Named { value, .. } = &implementation.trait_args[0] else {
        panic!("value")
    };
    assert!(std::ptr::eq(arguments.value(parameter).unwrap(), value));
    assert!(arguments
        .value(
            resolved
                .declared(contract.params.params[0].name.span)
                .unwrap()
        )
        .is_none());
    let method = &contract.items[0];
    for ty in [
        method.params[1].ty.as_ref().unwrap(),
        method.ret.as_ref().unwrap(),
    ] {
        let instantiated = index.instantiate_type(&arguments, ty).unwrap();
        let Type::Indexed {
            base,
            index: Some(width),
            ..
        } = instantiated.as_ref()
        else {
            panic!("array")
        };
        assert!(matches!(base.as_ref(), Type::Path(path) if path.segments[0].text == "real"));
        assert_eq!(evaluate(width, &index, 0), Ok(3));
    }
}

#[test]
fn layout_contracts_borrow_struct_bases_fields_ports_and_alias_targets() {
    let (modules, resolved) = parse(&["module model;
        enum Wire { Low, High }
        struct Packet<N: integer> { data: integer[N] }
        struct Derived<N: integer>(Wire[N]);
        entity Bus<N: integer> { data: integer[N] in }
        type Alias<N: integer> = Packet<N>;"]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    for item in &modules[0].items {
        let (name, parameters, expected): (_, _, Vec<&Type>) = match item {
            Item::Struct(declaration) => (
                &declaration.name,
                &declaration.params,
                declaration
                    .base
                    .iter()
                    .chain(declaration.fields.iter().map(|field| &field.ty))
                    .collect(),
            ),
            Item::Entity(declaration) => (
                &declaration.name,
                &declaration.params,
                declaration.ports.iter().map(|port| &port.ty).collect(),
            ),
            Item::Using(Using {
                kind: UsingKind::Alias { name, params, ty },
                ..
            }) => (name, params, vec![ty]),
            _ => continue,
        };
        let id = resolved.declared(name.span).unwrap();
        assert!(std::ptr::eq(index.layout_params(id).unwrap(), parameters));
        let actual: Vec<_> = index.layout_types(id).collect();
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(std::ptr::eq(actual, expected));
        }
    }
}

#[test]
fn generic_aliases_accept_impl_self_but_not_value_locals() {
    let (modules, resolved) = parse(&["module model;
        type Identity<T> = T; struct Value {}
        impl Value {
            fn inspect(self, value: integer) -> Value {
                let owner: Identity<Self> = self;
                let invalid: Identity<value> = self;
                return self;
            }
        }"]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    let Item::Impl(implementation) = &modules[0].items[2] else {
        panic!("impl")
    };
    let ImplItem::Fn(function) = &implementation.items[0] else {
        panic!("function")
    };
    for (position, statement) in function.body.as_ref().unwrap().stmts[..2]
        .iter()
        .enumerate()
    {
        let Stmt::Let(declaration) = statement else {
            panic!("let")
        };
        assert_eq!(
            index
                .normalize_type(declaration.ty.as_ref().unwrap())
                .is_some(),
            position == 0
        );
    }
}

#[test]
fn generic_aliases_share_integer_contract_classification() {
    let (modules, resolved) = parse(&["module m; type Select<T> = T;
        fn plus(n: Select<integer>) -> Select<integer> { return n + 1; }
        const RESULT: Select<integer> = plus(6);
        const AGAIN: integer = RESULT + 1;"]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    assert_eq!(
        evaluate(&constant(&modules[0], "RESULT").value, &index, 0),
        Ok(7)
    );
    assert_eq!(
        evaluate(&constant(&modules[0], "AGAIN").value, &index, 0),
        Ok(8)
    );
}

#[test]
fn aliases_inside_tuples_and_view_targets_normalize_recursively() {
    let (modules, resolved) = parse(&["module m;
        struct Box<T> { pub value: T }
        struct Bus<T> { pub data: T }
        view Source<T> for Bus<T> { data out }
        type Holder<T> = Box<T>; type Transport<T> = Bus<T>;
        type Pair<T> = (Holder<T>, Transport<T> Source);
        const C: Pair<integer> = 0;"]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    let normalized = index
        .normalize_type(&constant(&modules[0], "C").ty)
        .unwrap();
    let Type::Tuple { elems, .. } = normalized.as_ref() else {
        panic!("{normalized:?}")
    };
    for (ty, expected) in [(&elems[0], "Box"), (&elems[1], "Bus")] {
        let target = match ty {
            Type::View { target, .. } => target.as_ref(),
            other => other,
        };
        let Type::Generic { base, args, .. } = target else {
            panic!("{target:?}")
        };
        let Type::Path(path) = base.as_ref() else {
            panic!("{base:?}")
        };
        assert_eq!(
            resolved
                .def(resolved.resolved(path.span).unwrap())
                .unwrap()
                .name,
            expected
        );
        assert!(
            matches!(&args[0], GenericArg::PositionalType(Type::Path(p)) if p.segments[0].text == "integer")
        );
    }
}

#[test]
fn entity_alias_normalization_retains_shape_and_borrows_unchanged_types() {
    let (modules, resolved) = parse(&[
        "module provider; const BASE: integer = 6;
         pub entity Cell<W: integer> {}
         pub type Fixed = Cell<(BASE + 1)>; pub type Link = Fixed; pub type Group = Link[2];",
        "module caller; use provider::{Cell, Link, Group}; const BASE: integer = 99;
         const direct: Cell<7> = 0; const linked: Link = 0;
         const grouped: Group = 0; const wrapped: Link[3] = 0;",
    ]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    let direct = &constant(&modules[1], "direct").ty;
    let normalized = index.normalize_type(direct).unwrap();
    assert!(matches!(normalized, std::borrow::Cow::Borrowed(_)));
    assert!(std::ptr::eq(normalized.as_ref(), direct));
    let target = modules[0]
        .items
        .iter()
        .find_map(|item| match item {
            Item::Using(Using {
                kind: UsingKind::Alias { name, ty, .. },
                ..
            }) if name.text == "Fixed" => Some(ty),
            _ => None,
        })
        .unwrap();
    let linked = index
        .normalize_type(&constant(&modules[1], "linked").ty)
        .unwrap();
    assert!(matches!(linked, std::borrow::Cow::Borrowed(_)));
    assert!(std::ptr::eq(linked.as_ref(), target));
    let Type::Generic { args, .. } = linked.as_ref() else {
        unreachable!()
    };
    let GenericArg::Positional(value) = &args[0] else {
        unreachable!()
    };
    assert_eq!(evaluate(value, &index, 0), Ok(7));
    for name in ["linked", "grouped", "wrapped"] {
        assert_eq!(
            index.entity_type_id(&constant(&modules[1], name).ty),
            index.entity_type_id(direct)
        );
    }
    let wrapped = index
        .normalize_type(&constant(&modules[1], "wrapped").ty)
        .unwrap();
    assert!(matches!(wrapped, std::borrow::Cow::Owned(_)));
    let Type::Indexed {
        base,
        index: Some(size),
        ..
    } = wrapped.as_ref()
    else {
        unreachable!()
    };
    assert!(matches!(base.as_ref(), Type::Generic { .. }));
    assert_eq!(evaluate(size, &index, 0), Ok(3));
}

#[test]
fn same_leaf_entity_aliases_select_their_own_definitions_and_values() {
    let (modules, resolved) = parse(&[
        "module left; pub entity Cell<W: integer> {} pub type Link = Cell<7>;",
        "module right; pub entity Cell<W: integer> {} pub type Link = Cell<11>;",
        "module caller; use L = left::Link; use R = right::Link;
         const l: L = 0; const r: R = 0;",
    ]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    let l = &constant(&modules[2], "l").ty;
    let r = &constant(&modules[2], "r").ty;
    assert_ne!(index.entity_type_id(l), index.entity_type_id(r));
    for (ty, expected) in [(l, 7), (r, 11)] {
        let normalized = index.normalize_type(ty).unwrap();
        let Type::Generic { args, .. } = normalized.as_ref() else {
            unreachable!()
        };
        let GenericArg::Positional(value) = &args[0] else {
            unreachable!()
        };
        assert_eq!(evaluate(value, &index, 0), Ok(expected));
        let Type::Path(path) = ty else { unreachable!() };
        assert_eq!(
            resolved.entity_id(resolved.resolved(path.span).unwrap()),
            index.entity_type_id(ty)
        );
    }
}

#[test]
fn cyclic_or_unresolved_aliases_do_not_invent_an_entity() {
    for source in [
        "module m; type A = B; type B = A; const c: A = 0;",
        "module m; type A = Missing; const c: A = 0;",
    ] {
        let mut sink = DiagnosticSink::default();
        let module = crate::syntax::parse_module(FileId(0), source, &mut sink);
        let modules = [module];
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        assert!(sink.has_errors());
        let index = DeclarationIndex::collect(&resolved, &modules);
        let ty = &constant(&modules[0], "c").ty;
        assert!(index.normalize_type(ty).is_none());
        assert!(index.entity_type_id(ty).is_none());
    }
}

#[test]
fn entity_declarations_are_borrowed_and_import_renames_keep_identity() {
    let left = "module left; pub entity Cell { value: integer out }";
    let right = "module right; pub entity Cell { value: real out }";
    let caller = "module caller; use L = left::Cell; use R = right::Cell;
        entity Top {} impl Top { let l: L; let r: R; }";
    for sources in [[left, right, caller], [caller, right, left]] {
        let (modules, resolved) = parse(&sources);
        let index = DeclarationIndex::collect(&resolved, &modules);
        let declaration = |module_name: &str| {
            modules
                .iter()
                .find(|module| module.path.segments[0].text == module_name)
                .and_then(|module| {
                    module.items.iter().find_map(|item| match item {
                        Item::Entity(entity) => Some(entity),
                        _ => None,
                    })
                })
                .unwrap()
        };
        let left = declaration("left");
        let right = declaration("right");
        let left_id = resolved.declared(left.name.span).unwrap();
        let right_id = resolved.declared(right.name.span).unwrap();
        assert_ne!(left_id, right_id);
        assert!(std::ptr::eq(index.entity(left_id).unwrap(), left));
        assert!(std::ptr::eq(index.entity(right_id).unwrap(), right));
        assert!(index.function(left_id).is_none());
        assert!(index.constant(right_id).is_none());
        let module = modules
            .iter()
            .find(|m| m.path.segments[0].text == "caller")
            .unwrap();
        let implementation = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Impl(implementation) => Some(implementation),
                _ => None,
            })
            .unwrap();
        for (item, expected) in implementation.items.iter().zip([left_id, right_id]) {
            let crate::syntax::ast::ImplItem::Let(declaration) = item else {
                unreachable!()
            };
            assert_eq!(
                index.entity_type_id(declaration.ty.as_ref().unwrap()),
                Some(expected)
            );
        }
    }
}

#[test]
fn entity_classification_uses_resolved_wrappers_not_shadowed_or_missing_names() {
    let (modules, resolved) = parse(&["module m; entity Cell {}
        entity Buffer<T> {} impl<T> Buffer<T> { let data: T; }
        entity Top {} impl Top { let cells: Cell[2]; let buffer: Buffer<integer>; }"]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    for item in &modules[0].items {
        let Item::Impl(implementation) = item else {
            continue;
        };
        for item in &implementation.items {
            let crate::syntax::ast::ImplItem::Let(declaration) = item else {
                continue;
            };
            assert_eq!(
                index
                    .entity_type_id(declaration.ty.as_ref().unwrap())
                    .is_some(),
                declaration.name.text != "data"
            );
        }
    }
    let mut sink = DiagnosticSink::default();
    let missing =
        crate::syntax::parse_module(FileId(99), "module missing; const c: Cell = 1;", &mut sink);
    assert!(index.entity_type_id(&constant(&missing, "c").ty).is_none());
}

#[test]
fn trait_contracts_borrow_source_identity_and_rebuild_after_rewrites() {
    let left = "module left; pub trait Contract {
        fn required(self) -> integer;
        fn fallback(self) -> integer { return 3; }
    }";
    let right = "module right; pub trait Contract {
        fn other(self) -> real;
        fn fallback(self) -> real { return 2.5; }
    }";
    let caller = "module caller; use L = left::Contract; use R = right::Contract;
        struct Value(integer);
        impl L for Value { fn required(self) -> integer { return 1; } }
        impl R for Value { fn other(self) -> real { return 2.5; } }";
    for sources in [[left, right, caller], [caller, right, left]] {
        let (mut modules, resolved) = parse(&sources);
        {
            let index = DeclarationIndex::collect(&resolved, &modules);
            let mut ids = Vec::new();
            for module in &modules {
                for item in &module.items {
                    match item {
                        Item::Trait(contract) => {
                            let id = resolved.declared(contract.name.span).unwrap();
                            let borrowed = index.trait_decl(id).unwrap();
                            assert!(std::ptr::eq(borrowed, contract));
                            assert!(std::ptr::eq(&borrowed.items, &contract.items));
                            assert!(index.function(id).is_none());
                            ids.push(id);
                        }
                        Item::Impl(implementation) => {
                            let path = implementation.trait_.as_ref().unwrap();
                            let id = resolved.resolved(path.span).unwrap();
                            let contract = index.trait_decl(id).unwrap();
                            assert_eq!(
                                contract.items[0].name.text,
                                if path.segments[0].text == "L" {
                                    "required"
                                } else {
                                    "other"
                                }
                            );
                        }
                        Item::Struct(declaration) => assert!(index
                            .trait_decl(resolved.declared(declaration.name.span).unwrap())
                            .is_none()),
                        _ => {}
                    }
                }
            }
            assert_eq!(ids.len(), 2);
            assert_ne!(ids[0], ids[1]);
        }
        let mut changed = Vec::new();
        for module in &mut modules {
            for item in &mut module.items {
                if let Item::Trait(contract) = item {
                    changed.push(resolved.declared(contract.name.span).unwrap());
                    contract.items[0].name.text = "rewritten".into();
                }
            }
        }
        let index = DeclarationIndex::collect(&resolved, &modules);
        for id in changed {
            assert_eq!(
                index.trait_decl(id).unwrap().items[0].name.text,
                "rewritten"
            );
        }
    }
}

#[test]
fn declarations_and_contracts_borrow_the_original_ast() {
    let (modules, resolved) = parse(&["module a;
        const VALUE: integer = 40 + 2;
        fn function(n: integer) -> integer { return n + VALUE; }"]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    let value = constant(&modules[0], "VALUE");
    let id = resolved.declared(value.name.span).unwrap();
    assert!(std::ptr::eq(index.constant(id).unwrap(), value));
    assert!(index.function(id).is_none());
    let Item::Fn(function) = &modules[0].items[1] else {
        unreachable!()
    };
    let id = resolved.declared(function.name.span).unwrap();
    let borrowed = index.function(id).unwrap();
    assert!(std::ptr::eq(borrowed, function));
    assert!(std::ptr::eq(&borrowed.params, &function.params));
    assert!(std::ptr::eq(&borrowed.ret, &function.ret));
    assert!(index.constant(id).is_none());
}

#[test]
fn equal_names_imports_and_constant_aliases_keep_resolved_identity() {
    let (modules, resolved) = parse(&[
        "module left; pub const VALUE: integer = 4;
         pub fn calculate(n: integer) -> integer { return n + VALUE; }",
        "module right; pub const VALUE: integer = 12;
         pub fn calculate(n: integer) -> integer { return n + VALUE; }",
        "module caller; use left_call = left::calculate; use right_call = right::calculate;
         use L = left::VALUE; use R = right::VALUE;
         const LEFT: integer = L; const RIGHT: integer = R;
         const SUM: integer = left_call(LEFT) + right_call(RIGHT);
         const QUALIFIED: integer = left::calculate(1) + right::calculate(2);",
        "module imported; use left::calculate;
         const RESULT: integer = calculate(3);",
    ]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    assert_eq!(
        evaluate(&constant(&modules[2], "SUM").value, &index, 0),
        Ok(32)
    );
    assert_eq!(
        evaluate(&constant(&modules[2], "QUALIFIED").value, &index, 0),
        Ok(19)
    );
    assert_eq!(
        evaluate(&constant(&modules[3], "RESULT").value, &index, 0),
        Ok(7)
    );
}

#[test]
fn module_constants_are_not_captured_by_caller_parameters() {
    let (modules, resolved) = parse(&["module a;
        const VALUE: integer = 7;
        fn global_value() -> integer { return VALUE; }
        fn caller(VALUE: integer) -> integer { return global_value() + VALUE; }
        const RESULT: integer = caller(90);"]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    assert_eq!(
        evaluate(&constant(&modules[0], "RESULT").value, &index, 0),
        Ok(97)
    );
}

#[test]
fn an_unresolved_spelling_never_falls_back_to_a_known_leaf() {
    let (modules, resolved) = parse(&["module a; const VALUE: integer = 7;
        fn calculate(n: integer) -> integer { return n; }"]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    let mut sink = DiagnosticSink::default();
    let other = crate::syntax::parse_module(
        FileId(99),
        "module other; const MISSING: integer = VALUE; const CALL: integer = calculate(1);",
        &mut sink,
    );
    let Expr::Path(path) = &constant(&other, "MISSING").value else {
        unreachable!()
    };
    assert!(index.constant_path(path).is_none());
    let Expr::Call { callee, .. } = &constant(&other, "CALL").value else {
        unreachable!()
    };
    let Expr::Path(path) = callee.as_ref() else {
        unreachable!()
    };
    assert!(index.function_path(path).is_none());
}

#[test]
fn foreign_prototypes_are_indexed_but_never_executed_as_constants() {
    let (modules, resolved) = parse(&["module a;
        extern \"C\" { fn host(n: integer) -> integer; }
        const CALL: integer = host(1);"]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    let Expr::Call { callee, .. } = &constant(&modules[0], "CALL").value else {
        unreachable!()
    };
    let Expr::Path(path) = callee.as_ref() else {
        unreachable!()
    };
    assert!(index.function_path(path).unwrap().body.is_none());
    assert_eq!(
        evaluate(&constant(&modules[0], "CALL").value, &index, 0)
            .unwrap_err()
            .reason,
        Reason::Unsupported
    );
}

#[test]
fn integer_analysis_follows_aliases_but_does_not_erase_other_value_types() {
    let (modules, resolved) = parse(&["module a;
        type Count = integer; type Alias = Count;
        fn calculate(n: Alias) -> Count { return n + 1; }
        fn array() -> integer[2] { return [1, 2]; }
        fn real_argument(n: real) -> integer { return 1; }
        const RESULT: integer = calculate(6);
        const ARRAY: integer[2] = array();
        const REAL: integer = real_argument(1);"]);
    let index = DeclarationIndex::collect(&resolved, &modules);
    assert_eq!(
        evaluate(&constant(&modules[0], "RESULT").value, &index, 0),
        Ok(7)
    );
    for name in ["ARRAY", "REAL"] {
        let Expr::Call { callee, .. } = &constant(&modules[0], name).value else {
            unreachable!()
        };
        let Expr::Path(path) = callee.as_ref() else {
            unreachable!()
        };
        assert!(
            index.function_path(path).is_some(),
            "the declaration still exists"
        );
        assert!(
            crate::types::consteval::Environment::function(&index, callee).is_none(),
            "the value needs a typed interpreter"
        );
        assert_eq!(
            evaluate(&constant(&modules[0], name).value, &index, 0)
                .unwrap_err()
                .reason,
            Reason::Unsupported
        );
    }
}

#[test]
fn indexes_rebuilt_after_rewrites_see_the_current_body() {
    let (mut modules, resolved) = parse(&["module a;
        fn value() -> integer { return 1; }
        const RESULT: integer = value();"]);
    {
        let index = DeclarationIndex::collect(&resolved, &modules);
        assert_eq!(
            evaluate(&constant(&modules[0], "RESULT").value, &index, 0),
            Ok(1)
        );
    }
    let Item::Fn(function) = &mut modules[0].items[0] else {
        unreachable!()
    };
    let crate::syntax::ast::Stmt::Return {
        value: Some(Expr::Int { text, .. }),
        ..
    } = &mut function.body.as_mut().unwrap().stmts[0]
    else {
        unreachable!()
    };
    *text = "9".into();
    let index = DeclarationIndex::collect(&resolved, &modules);
    assert_eq!(
        evaluate(&constant(&modules[0], "RESULT").value, &index, 0),
        Ok(9)
    );
}

#[test]
fn recursive_aliases_inside_tuples_and_transformed_arguments_are_diagnosed() {
    for source in [
        "module m; type Tree<T> = (T, Tree<(T, T)>);",
        "module m; type A<T> = (T, B<T>); type B<U> = (U, A<U>);",
    ] {
        let mut sink = DiagnosticSink::default();
        let module = crate::syntax::parse_module(FileId(0), source, &mut sink);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        let modules = [module];
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        assert!(
            sink.diagnostics()
                .iter()
                .any(|d| d.code == Some(crate::diag::codes::DUPLICATE_ITEM)
                    && d.message.contains("is defined in terms of itself")),
            "{:?}",
            sink.diagnostics()
        );
        let index = DeclarationIndex::collect(&resolved, &modules);
        assert!(modules[0].items.iter().any(|item| matches!(item, Item::Using(Using { kind: UsingKind::Alias { ty, .. }, .. }) if index.alias_cycle(ty).is_some())));
    }
}

#[test]
fn resolved_alias_materialization_visits_trait_contract_arguments() {
    let (mut modules, resolved) = parse(&["module m;
        trait Apply<Input, Output> { fn apply(self, value: Input) -> Output; }
        struct Counter(integer);
        type Scalar<T: Apply<integer, integer>> = T;
        impl Apply<Scalar<integer>, Scalar<integer>> for Counter {
            fn apply(self, value: Scalar<integer>) -> Scalar<integer> { return value; }
        }"]);
    let mut sink = DiagnosticSink::default();
    crate::resolve::normalize_aliases(&mut modules, &resolved, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let implementation = modules[0]
        .items
        .iter()
        .find_map(|item| match item {
            Item::Impl(implementation) => Some(implementation),
            _ => None,
        })
        .unwrap();
    assert_eq!(implementation.trait_args.len(), 2);
    for argument in &implementation.trait_args {
        let ty = argument.as_type().unwrap();
        assert!(
            matches!(ty, Type::Path(path) if path.segments[0].text == "integer"),
            "{argument:?}"
        );
    }
}
