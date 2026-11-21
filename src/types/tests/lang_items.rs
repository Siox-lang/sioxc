//! Compiler-created values use registered declarations, never matching leaves.

use super::*;

#[test]
fn radix_prefix_results_follow_borrowed_registered_contracts() {
    for fixed in [false, true] {
        let target = if fixed { "Fixed" } else { "Alias" };
        let result = if fixed { "Fixed" } else { "Alias[8]" };
        let provider = format!(
            "module numbers;
             pub enum Level {{ '0', '1' }}
             pub struct Word(Level[]);
             pub type Alias = Word;
             pub type Fixed = Alias[9..2];
             impl core::digits::Decode<\"x\", Char[]> for {target} {{}}
             fn probe() {{ let value: {result} = x\"AB\"; }}"
        );
        let mut sources = [
            "module core::digits;
             pub trait Decode<Tag: Char[], Input> { static fn prefix(data: Input) -> Self {} }
             attr lang for Decode = \"prefix\";"
                .to_string(),
            provider,
            "module std::logic;
             pub enum Logic { '0', '1' }
             pub struct unsigned(Logic[]);
             pub trait Prefix<Tag: Char[], Input> {}
             impl Prefix<\"x\", Char[]> for unsigned {}"
                .to_string(),
        ];
        for _ in 0..2 {
            let mut sink = DiagnosticSink::new();
            let modules: Vec<_> = sources
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    crate::syntax::parse_module(FileId(index as u32), source, &mut sink)
                })
                .collect();
            let modules = with_core(modules, &mut sink);
            let resolved = crate::resolve::resolve(&modules, &mut sink);
            assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
            let typed = check(&modules, &resolved, &mut sink);
            assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
            let provider = modules
                .iter()
                .find(|module| module.path.segments[0].text == "numbers")
                .unwrap();
            let element = provider
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Enum(declaration) => resolved.declared(declaration.name.span),
                    _ => None,
                })
                .unwrap();
            let (target, literal) = (
                provider
                    .items
                    .iter()
                    .find_map(|item| match item {
                        Item::Impl(declaration) => Some(&declaration.target),
                        _ => None,
                    })
                    .unwrap(),
                provider
                    .items
                    .iter()
                    .find_map(|item| match item {
                        Item::Fn(function) => match function.body.as_ref()?.stmts.first()? {
                            Stmt::Let(declaration) => declaration.value.as_ref(),
                            _ => None,
                        },
                        _ => None,
                    })
                    .unwrap(),
            );
            assert_eq!(
                typed.expr_type(expr_span(literal)),
                Some(&Ty::Array {
                    elem: Box::new(Ty::Named(element)),
                    family: Some("numbers::Word".to_string()),
                    len: 8,
                    low: if fixed { 2 } else { 0 },
                })
            );
            let mut borrow_sink = DiagnosticSink::new();
            let mut checker = Checker::new(&mut borrow_sink, &resolved, &modules);
            checker.collect(&modules);
            let [original] = checker.prefix_types["x"].as_slice() else {
                panic!("one registered provider");
            };
            assert!(std::ptr::eq(*original, target));
            sources.reverse();
        }
    }
}

#[test]
fn radix_prefix_failures_do_not_acquire_a_named_library_fallback() {
    for (declarations, digits, message) in [
        ("", "AB", "no `impl Prefix` declares it"),
        (
            "struct Left(Cell[]); struct Right(Cell[]);
          impl Prefix<\"x\", Char[]> for Left {}
          impl Prefix<\"x\", Char[]> for Right {}",
            "AB",
            "ambiguous",
        ),
        (
            "struct Record { pub data: integer }
          impl Prefix<\"x\", Char[]> for Record {}",
            "AB",
            "cannot represent",
        ),
        (
            "struct Record { pub data: integer }
             impl Prefix<\"x\", Char[]> for Record[] {}",
            "AB",
            "cannot represent",
        ),
        (
            "struct Narrow(Cell[]);
          impl Prefix<\"x\", Char[]> for Narrow[4] {}",
            "AB",
            "cannot represent",
        ),
        (
            "struct Packed(Cell[]);
          impl Prefix<\"x\", Char[]> for Packed {}",
            "GG",
            "invalid hex",
        ),
    ] {
        let source = format!(
            "module invalid;
             enum Cell {{ '0', '1' }}
             enum Logic {{ '0', '1' }} struct unsigned(Logic[]);
             {declarations}
             fn probe() {{ let value: integer = x\"{digits}\"; }}"
        );
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &source, &mut sink);
        let modules = with_core(vec![module], &mut sink);
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        let typed = check(&modules, &resolved, &mut sink);
        let errors: Vec<_> = sink
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
            .collect();
        let unsupported_array_impl = declarations.contains("for Record[]");
        assert_eq!(
            errors.len(),
            if unsupported_array_impl { 2 } else { 1 },
            "{errors:?}"
        );
        assert!(errors
            .iter()
            .all(|error| error.code == Some(codes::TYPE_MISMATCH)));
        let prefix = errors
            .iter()
            .find(|error| error.message.contains(message))
            .unwrap();
        assert!(prefix.message.contains("bit-string"), "{errors:?}");
        assert_eq!(
            prefix.labels.len(),
            usize::from(message == "cannot represent")
        );
        if unsupported_array_impl {
            assert!(errors
                .iter()
                .any(|error| error.message == "a trait cannot be implemented for `Record[]`"));
        }
        let literal = modules[0]
            .items
            .iter()
            .find_map(|item| match item {
                Item::Fn(function) => match function.body.as_ref()?.stmts.first()? {
                    Stmt::Let(declaration) => declaration.value.as_ref(),
                    _ => None,
                },
                _ => None,
            })
            .unwrap();
        assert_eq!(typed.expr_type(expr_span(literal)), Some(&Ty::Error));
    }
}

#[test]
fn unregistered_library_namespace_traits_do_not_grant_kernel_capabilities() {
    let mut sources = [
        ("module core::contracts; pub trait Capability {}", FileId(0)),
        ("module std::contracts; pub trait Capability {}", FileId(1)),
        (
            "module caller;
          fn core_bound<T: core::contracts::Capability>(value: T) -> T { return value; }
          fn std_bound<T: std::contracts::Capability>(value: T) -> T { return value; }
          fn check(i: integer, r: real, values: integer[2]) {
              core_bound(i);
              std_bound(r);
              core_bound(values);
          }",
            FileId(2),
        ),
    ];
    for _ in 0..2 {
        let sink = check_modules(&sources);
        let errors: Vec<_> = sink
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
            .collect();
        assert_eq!(errors.len(), 3, "{:?}", sink.diagnostics());
        assert!(errors
            .iter()
            .all(|diagnostic| diagnostic.code == Some(codes::TYPE_MISMATCH)
                && diagnostic.message.contains("Capability")));
        sources.reverse();
    }
}

#[test]
fn generic_bound_capabilities_follow_registered_identity_and_explicit_impls() {
    let mut sources = [
        (
            "module core::registered;
          pub trait Capability<Rhs, Out> { fn add(rhs: Rhs) -> Out; }
          attr lang for Capability = \"add\";",
            FileId(0),
        ),
        (
            "module std::contracts;
          pub trait Capability {}
          impl Capability for integer {}",
            FileId(1),
        ),
        (
            "module caller;
          use Hook = core::registered::Capability;
          use Explicit = std::contracts::Capability;
          fn registered<T: Hook<T, T>>(value: T) -> T { return value; }
          fn seeded<T: Add<T, T>>(value: T) -> T { return value; }
          fn explicit<T: Explicit>(value: T) -> T { return value; }
          fn check(i: integer, r: real) {
              let a: integer = registered(i);
              let b: real = registered(r);
              let c: integer = seeded(i);
              let d: integer = explicit(i);
              let missing: real = explicit(r);
          }",
            FileId(2),
        ),
    ];
    for _ in 0..2 {
        let sink = check_modules(&sources);
        let errors: Vec<_> = sink
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "{:?}", sink.diagnostics());
        assert_eq!(errors[0].code, Some(codes::TYPE_MISMATCH));
        assert!(
            errors[0].message.contains("real") && errors[0].message.contains("Capability"),
            "{:?}",
            errors[0]
        );
        sources.reverse();
    }
}

#[test]
fn ambiguous_suffix_does_not_acquire_the_intrinsic_fallback_type() {
    let source = "module suffixes;
        struct Left {} struct Right {}
        impl Suffix<\"ns\", integer> for Left {}
        impl Suffix<\"ns\", integer> for Right {}
        fn value() -> integer { return 7ns; }";
    let mut sink = DiagnosticSink::default();
    let module = crate::syntax::parse_module(FileId(0), source, &mut sink);
    let modules = with_core(vec![module], &mut sink);
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let typed = check(&modules, &resolved, &mut sink);
    assert_eq!(sink.error_count(), 1, "{:?}", sink.diagnostics());
    let error = sink
        .diagnostics()
        .iter()
        .find(|d| d.severity == crate::diag::Severity::Error)
        .unwrap();
    assert_eq!(error.code, Some(codes::UNKNOWN_NAME));
    assert!(error.message.contains("suffixes::Left::ns"));
    assert!(error.message.contains("suffixes::Right::ns"));
    let expression = modules[0]
        .items
        .iter()
        .find_map(|item| match item {
            Item::Fn(function) => match &function.body.as_ref()?.stmts[0] {
                Stmt::Return { value, .. } => value.as_ref(),
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    assert_eq!(typed.expr_type(expr_span(expression)), Some(&Ty::Error));
}

#[test]
fn suffix_contracts_follow_aliases_and_module_order() {
    let consumer = "module consumer;
        fn from_left() -> left::Measure { return 7leftunit; }
        fn from_right() -> right::Measure { return 9rightunit; }
        fn from_array() -> left::Word { return 3words; }";
    let left = "module left;
        impl Suffix<\"leftunit\", integer> for Alias {}
        impl Suffix<\"words\", integer> for Packed {}
        pub struct Measure {}
        pub type Alias = Measure;
        pub enum Cell { Low, High }
        pub struct Word(Cell[]);
        pub type Packed = Word;";
    let right = "module right;
        pub struct Measure {}
        impl Suffix<\"rightunit\", integer> for Measure {}";
    for reversed in [false, true] {
        for wrong_owner in [false, true] {
            let source = if wrong_owner {
                consumer.replace(
                    "from_left() -> left::Measure",
                    "from_left() -> right::Measure",
                )
            } else {
                consumer.to_string()
            };
            let mut sink = DiagnosticSink::default();
            let mut modules = with_core(
                vec![
                    crate::syntax::parse_module(FileId(0), &source, &mut sink),
                    crate::syntax::parse_module(FileId(1), left, &mut sink),
                    crate::syntax::parse_module(FileId(2), right, &mut sink),
                ],
                &mut sink,
            );
            if reversed {
                modules.reverse();
            }
            let resolved = crate::resolve::resolve(&modules, &mut sink);
            assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
            let typed = check(&modules, &resolved, &mut sink);
            assert_eq!(
                sink.error_count(),
                usize::from(wrong_owner),
                "reversed={reversed} wrong_owner={wrong_owner}: {:?}",
                sink.diagnostics()
            );
            let owner = |module: &str, name: &str| {
                let declaration = modules
                    .iter()
                    .find(|source| {
                        source.path.segments.len() == 1 && source.path.segments[0].text == module
                    })
                    .unwrap()
                    .items
                    .iter()
                    .find_map(|item| match item {
                        Item::Struct(declaration) if declaration.name.text == name => {
                            Some(declaration.name.span)
                        }
                        Item::Enum(declaration) if declaration.name.text == name => {
                            Some(declaration.name.span)
                        }
                        _ => None,
                    })
                    .unwrap();
                resolved.declared(declaration).unwrap()
            };
            let expected = [
                ("from_left", Ty::Named(owner("left", "Measure"))),
                ("from_right", Ty::Named(owner("right", "Measure"))),
                (
                    "from_array",
                    Ty::Array {
                        elem: Box::new(Ty::Named(owner("left", "Cell"))),
                        family: Some("left::Word".to_string()),
                        len: 0,
                        low: 0,
                    },
                ),
            ];
            for (name, expected) in expected {
                let expression = modules
                    .iter()
                    .flat_map(|module| &module.items)
                    .find_map(|item| match item {
                        Item::Fn(function) if function.name.text == name => {
                            match &function.body.as_ref()?.stmts[0] {
                                Stmt::Return { value, .. } => value.as_ref(),
                                _ => None,
                            }
                        }
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(typed.expr_type(expr_span(expression)), Some(&expected));
            }
        }
    }
}

#[test]
fn resize_preserves_its_operands_element_identity() {
    let source = "module consumer;
        enum Logic { OtherLow, OtherHigh }
        fn resize_left(value: left::Word[8]) -> left::Word[3] { return resize(value, 3); }
        fn resize_right(value: right::Word[8]) -> right::Word[3] { return resize(value, 3); }
        fn resize_plain(value: left::Cell[8]) -> left::Cell[3] { return resize(value, 3); }";
    let left = "module left; pub enum Cell { Low, High } pub struct Word(Cell[]);";
    let right = "module right; pub enum Cell { Low, High } pub struct Word(Cell[]);";
    for reversed in [false, true] {
        let mut sink = DiagnosticSink::default();
        let mut modules = with_core(
            vec![
                crate::syntax::parse_module(FileId(0), source, &mut sink),
                crate::syntax::parse_module(FileId(1), left, &mut sink),
                crate::syntax::parse_module(FileId(2), right, &mut sink),
            ],
            &mut sink,
        );
        if reversed {
            modules.reverse();
        }
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = check(&modules, &resolved, &mut sink);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        let mut checked = 0;
        for function in modules
            .iter()
            .flat_map(|module| &module.items)
            .filter_map(|item| {
                if let Item::Fn(function) = item {
                    Some(function)
                } else {
                    None
                }
            })
        {
            let Stmt::Return {
                value: Some(call), ..
            } = &function.body.as_ref().unwrap().stmts[0]
            else {
                panic!("return")
            };
            let Expr::Call { args, .. } = call else {
                panic!("resize call")
            };
            let Some(Ty::Array { elem, family, .. }) = typed.expr_type(expr_span(&args[0])) else {
                panic!("typed array input")
            };
            assert_eq!(
                typed.expr_type(expr_span(call)),
                Some(&Ty::Array {
                    elem: elem.clone(),
                    family: family.clone(),
                    len: 3,
                    low: 0,
                }),
                "{} reversed={reversed}",
                function.name.text
            );
            checked += 1;
        }
        assert_eq!(checked, 3);
    }
}

const RESULTS: &str = "module user;
    enum Bool { Off, On }
    fn comparison() -> Bool { return 1 < 2; }
    fn event(value: integer) -> Bool { return value'event; }
    fn ascending(value: integer[2]) -> Bool { return value'ascending; }
    fn present() -> Bool { return exists(\"missing\"); }";

#[test]
fn boolean_results_follow_a_renamed_lang_item_in_either_module_order() {
    let source = RESULTS.replace("-> Bool", "-> core::primitive::Decision");
    let core = "module core::primitive;
        pub enum Decision { Off, On }
        attr lang for Decision = \"bool\";";
    for core_first in [false, true] {
        let mut sink = DiagnosticSink::default();
        let mut modules = with_core(
            vec![
                crate::syntax::parse_module(FileId(0), &source, &mut sink),
                crate::syntax::parse_module(FileId(1), core, &mut sink),
            ],
            &mut sink,
        );
        assert_eq!(modules.len(), 2, "explicit core must not be replaced");
        if core_first {
            modules.reverse();
        }
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let expected = Ty::Named(resolved.lang("bool").expect("registered Decision"));
        let typed = check(&modules, &resolved, &mut sink);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        assert_results(&modules, &typed, &expected);
    }
}

#[test]
fn a_user_bool_cannot_supply_an_unregistered_result_role() {
    let mut sink = DiagnosticSink::default();
    let modules = [crate::syntax::parse_module(FileId(0), RESULTS, &mut sink)];
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    assert!(resolved.lang("bool").is_none());
    let typed = check(&modules, &resolved, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    assert_results(&modules, &typed, &Ty::Error);
}

#[test]
fn boolean_literals_and_patterns_follow_the_registered_owner() {
    let source = "module user;
        enum Bool { false, true }
        fn yes() -> core::primitive::Decision { return true; }
        fn no() -> core::primitive::Decision { return false; }
        fn explicit() -> Bool { return Bool::true; }
        fn select(value: core::primitive::Decision) -> integer {
            return match value { true => 11, false => 22 };
        }";
    for declaration in [
        "pub enum Decision { true, false }",
        "pub enum Base { true, false } pub enum Decision(Base);",
    ] {
        for core_first in [false, true] {
            let mut sink = DiagnosticSink::default();
            let core = format!(
                "module core::primitive; {declaration}
                 attr lang for Decision = \"bool\";"
            );
            let mut modules = vec![
                crate::syntax::parse_module(FileId(0), source, &mut sink),
                crate::syntax::parse_module(FileId(1), &core, &mut sink),
            ];
            if core_first {
                modules.reverse();
            }
            let resolved = crate::resolve::resolve(&modules, &mut sink);
            let typed = check(&modules, &resolved, &mut sink);
            assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
            let owner = resolved.lang("bool").unwrap();
            let user = modules
                .iter()
                .find(|module| module.span.file == FileId(0))
                .unwrap();
            for item in &user.items {
                let Item::Fn(function) = item else { continue };
                let Stmt::Return {
                    value: Some(Expr::Path(path)),
                    ..
                } = &function.body.as_ref().unwrap().stmts[0]
                else {
                    continue;
                };
                let selected = resolved.enum_owner(path).unwrap();
                if function.name.text == "explicit" {
                    assert_ne!(selected, owner);
                    assert!(!path.is_boolean_literal());
                } else {
                    assert_eq!(selected, owner);
                    assert!(path.is_boolean_literal());
                }
                assert_eq!(typed.expr_type(path.span), Some(&Ty::Named(selected)));
            }
        }
    }
}

#[test]
fn boolean_literal_marker_survives_macros_without_capturing_explicit_paths() {
    let mut sink = DiagnosticSink::default();
    let source = "module user;
        enum Bool { false, true }
        macro literal() { true }
        macro explicit() { Bool::true }
        fn yes() -> core::primitive::Bool { return literal!(); }
        fn user_value() -> Bool { return explicit!(); }";
    let module = crate::syntax::parse_module(FileId(0), source, &mut sink);
    let mut modules = with_core(vec![module], &mut sink);
    crate::syntax::macros::expand(&mut modules, &HashMap::new(), &mut sink);
    // Equal spans are also possible in generated ASTs; they never identify literals.
    for item in &mut modules[0].items {
        let Item::Fn(function) = item else { continue };
        let Stmt::Return {
            value: Some(Expr::Path(path)),
            ..
        } = &mut function.body.as_mut().unwrap().stmts[0]
        else {
            panic!("expanded enum path")
        };
        for segment in &mut path.segments {
            segment.span = path.span;
        }
    }
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = check(&modules, &resolved, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let owner = resolved.lang("bool").unwrap();
    for item in &modules[0].items {
        let Item::Fn(function) = item else { continue };
        let Stmt::Return {
            value: Some(Expr::Path(path)),
            ..
        } = &function.body.as_ref().unwrap().stmts[0]
        else {
            panic!("expanded enum path")
        };
        let selected = resolved.enum_owner(path).unwrap();
        assert_eq!(selected == owner, function.name.text == "yes");
        assert_eq!(typed.expr_type(path.span), Some(&Ty::Named(selected)));
    }
}

#[test]
fn explicit_derived_enum_paths_keep_owner_with_coincident_generated_spans() {
    for coincident in [false, true] {
        let mut sink = DiagnosticSink::default();
        let source = "module user;
            enum Base { A, B }
            enum Derived(Base);
            macro derived() { Derived::A }
            fn ordinary() -> Derived { return Derived::A; }
            fn generated() -> Derived { return derived!(); }";
        let module = crate::syntax::parse_module(FileId(0), source, &mut sink);
        let mut modules = with_core(vec![module], &mut sink);
        crate::syntax::macros::expand(&mut modules, &HashMap::new(), &mut sink);
        if coincident {
            for item in &mut modules[0].items {
                let Item::Fn(function) = item else { continue };
                if function.name.text != "generated" {
                    continue;
                }
                let Stmt::Return {
                    value: Some(Expr::Path(path)),
                    ..
                } = &mut function.body.as_mut().unwrap().stmts[0]
                else {
                    panic!("expanded enum path")
                };
                for segment in &mut path.segments {
                    segment.span = path.span;
                }
            }
        }
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let Item::Enum(derived) = &modules[0].items[1] else {
            panic!("derived enum")
        };
        let expected = resolved.declared(derived.name.span).unwrap();
        for item in &modules[0].items {
            let Item::Fn(function) = item else { continue };
            let Stmt::Return {
                value: Some(Expr::Path(path)),
                ..
            } = &function.body.as_ref().unwrap().stmts[0]
            else {
                panic!("enum value")
            };
            assert_eq!(
                resolved.enum_owner(path),
                Some(expected),
                "{} with coincident={coincident}",
                function.name.text
            );
        }
        let typed = check(&modules, &resolved, &mut sink);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        for item in &modules[0].items {
            let Item::Fn(function) = item else { continue };
            let Stmt::Return {
                value: Some(value), ..
            } = &function.body.as_ref().unwrap().stmts[0]
            else {
                panic!("enum value")
            };
            assert_eq!(
                typed.expr_type(expr_span(value)),
                Some(&Ty::Named(expected))
            );
        }
    }
}

#[test]
fn inherited_enum_owners_survive_imports_module_order_and_generated_spans() {
    for coincident in [false, true] {
        for reverse in [false, true] {
            for wrong_owner in [false, true] {
                let mut sink = DiagnosticSink::default();
                let negative = if wrong_owner {
                    "fn wrong() -> Left { return Right::A; }
                     fn wrong_pattern(value: Left) -> integer {
                         return match value { Right::A => 3, _ => 0 };
                     }"
                } else {
                    ""
                };
                let source = format!(
                    "module user;
                     use Left = a::state::State;
                     use Right = b::state::State;
                     macro left_value() {{ Left::A }}
                     macro right_value() {{ Right::A }}
                     fn left() -> Left {{ return left_value!(); }}
                     fn right() -> Right {{ return right_value!(); }}
                     fn select(value: Left) -> integer {{
                         return match value {{ Left::A => 3, Left::B => 7 }};
                     }} {negative}"
                );
                let sources = [
                    source.as_str(),
                    "module a::state; pub enum Base { A = 3, B = 7 }
                     pub enum State(Base);",
                    "module b::state; pub enum Base { A = 1, B = 9 }
                     pub enum State(Base);",
                ];
                let modules = sources
                    .iter()
                    .enumerate()
                    .map(|(file, source)| {
                        crate::syntax::parse_module(FileId(file as u32), source, &mut sink)
                    })
                    .collect();
                let mut modules = with_core(modules, &mut sink);
                crate::syntax::macros::expand(&mut modules, &HashMap::new(), &mut sink);
                let user = modules
                    .iter_mut()
                    .find(|m| m.span.file == FileId(0))
                    .unwrap();
                if coincident {
                    for item in &mut user.items {
                        let Item::Fn(function) = item else { continue };
                        let Stmt::Return {
                            value: Some(value), ..
                        } = &mut function.body.as_mut().unwrap().stmts[0]
                        else {
                            panic!("return")
                        };
                        match value {
                            Expr::Path(path) => {
                                for segment in &mut path.segments {
                                    segment.span = path.span;
                                }
                            }
                            Expr::Match { arms, .. } => {
                                for arm in arms {
                                    if let Pattern::Path(path) = &mut arm.pattern {
                                        for segment in &mut path.segments {
                                            segment.span = path.span;
                                        }
                                    }
                                }
                            }
                            _ => panic!("variant or match"),
                        }
                    }
                }
                if reverse {
                    modules.reverse();
                }
                let resolved = crate::resolve::resolve(&modules, &mut sink);
                assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
                let typed = check(&modules, &resolved, &mut sink);
                let errors: Vec<_> = sink
                    .diagnostics()
                    .iter()
                    .filter(|d| d.severity == crate::diag::Severity::Error)
                    .collect();
                assert_eq!(
                    errors.len(),
                    if wrong_owner { 2 } else { 0 },
                    "coincident={coincident}, reverse={reverse}: {errors:?}"
                );
                assert!(errors.iter().all(|d| d.code == Some(codes::TYPE_MISMATCH)));
                let user = modules.iter().find(|m| m.span.file == FileId(0)).unwrap();
                let mut owners = Vec::new();
                for item in &user.items {
                    let Item::Fn(function) = item else { continue };
                    let expected = match function.name.text.as_str() {
                        "left" => "a::state::State",
                        "right" => "b::state::State",
                        _ => continue,
                    };
                    let Stmt::Return {
                        value: Some(Expr::Path(path)),
                        ..
                    } = &function.body.as_ref().unwrap().stmts[0]
                    else {
                        panic!("variant")
                    };
                    let owner = resolved.enum_owner(path).unwrap();
                    assert_eq!(resolved.qualified_name(owner).as_deref(), Some(expected));
                    assert_eq!(typed.expr_type(path.span), Some(&Ty::Named(owner)));
                    let variant = resolved.def(resolved.resolved(path.span).unwrap()).unwrap();
                    assert_ne!(variant.parent, Some(owner));
                    owners.push(owner);
                }
                assert_eq!(owners.len(), 2);
                assert_ne!(owners[0], owners[1]);
            }
        }
    }
}

#[test]
fn derived_enum_patterns_keep_owner_when_generated_spans_coincide() {
    let mut sink = DiagnosticSink::default();
    let source = "module user;
        enum Base { A, B }
        enum Derived(Base);
        fn choose(value: Derived) -> integer {
            return match value { Derived::A => 11, Derived::B => 22 };
        }";
    let module = crate::syntax::parse_module(FileId(0), source, &mut sink);
    let mut modules = with_core(vec![module], &mut sink);
    let Item::Fn(function) = &mut modules[0].items[2] else {
        panic!("function")
    };
    let Stmt::Return {
        value: Some(Expr::Match { arms, .. }),
        ..
    } = &mut function.body.as_mut().unwrap().stmts[0]
    else {
        panic!("match")
    };
    for arm in arms {
        let Pattern::Path(path) = &mut arm.pattern else {
            panic!("variant")
        };
        for segment in &mut path.segments {
            segment.span = path.span;
        }
    }
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    check(&modules, &resolved, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
}

#[test]
fn boolean_literals_do_not_fall_back_to_user_types_or_missing_variants() {
    for core in [
        "module core::primitive;",
        "module core::primitive; pub struct Decision { }
            attr lang for Decision = \"bool\";",
        "module core::primitive; pub enum Decision { Off, On }
            attr lang for Decision = \"bool\";",
    ] {
        let mut sink = DiagnosticSink::default();
        let source = "module user; enum Bool { false, true }
            fn literal() -> Bool { return true; }";
        let modules = vec![
            crate::syntax::parse_module(FileId(0), source, &mut sink),
            crate::syntax::parse_module(FileId(1), core, &mut sink),
        ];
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = check(&modules, &resolved, &mut sink);
        let errors: Vec<_> = sink
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].code, Some(codes::LANG_ITEM));
        let Item::Fn(function) = &modules[0].items[1] else {
            panic!("function")
        };
        let Stmt::Return {
            value: Some(Expr::Path(path)),
            ..
        } = &function.body.as_ref().unwrap().stmts[0]
        else {
            panic!("literal")
        };
        assert!(resolved.resolved(path.span).is_none());
        assert_eq!(typed.expr_type(path.span), Some(&Ty::Error));
    }
}

#[test]
fn boolean_literal_patterns_reject_same_leaf_user_enums() {
    let mut sink = DiagnosticSink::default();
    let module = crate::syntax::parse_module(
        FileId(0),
        "module user; enum Bool { false, true }
         fn wrong(value: Bool) -> integer {
             return match value { true => 1, false => 0 };
         }",
        &mut sink,
    );
    let modules = with_core(vec![module], &mut sink);
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    check(&modules, &resolved, &mut sink);
    let errors: Vec<_> = sink
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
        .collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    for diagnostic in errors {
        assert_eq!(diagnostic.code, Some(codes::TYPE_MISMATCH));
        assert!(diagnostic.message.contains("core::primitive::Bool"));
        assert!(!diagnostic.message.contains("(bool)"));
    }
}

#[test]
fn boolean_metadata_values_follow_the_registered_owner() {
    for (value, expected_errors) in [
        ("true", 0),
        ("core::primitive::Decision::false", 0),
        ("Bool::true", 1),
    ] {
        let mut sink = DiagnosticSink::default();
        let source = format!(
            "module user;
            enum Bool {{ true, false }}
            attr flag: core::primitive::Decision for entity;
            entity E {{}}
            attr flag for E = {value};"
        );
        let core = "module core::primitive; pub enum Decision { true, false }
            attr lang for Decision = \"bool\";";
        let mut modules = vec![
            crate::syntax::parse_module(FileId(0), &source, &mut sink),
            crate::syntax::parse_module(FileId(1), core, &mut sink),
        ];
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        crate::resolve::attributes::attach(&mut modules, &resolved, &mut sink);
        check(&modules, &resolved, &mut sink);
        assert_eq!(
            sink.error_count(),
            expected_errors,
            "{value}: {:?}",
            sink.diagnostics()
        );
        assert_eq!(
            sink.diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.code == Some(codes::INVALID_ATTR_VALUE_TYPE))
                .count(),
            expected_errors
        );
    }
}

#[test]
fn range_values_and_index_contracts_follow_a_renamed_role_in_either_order() {
    let source = "module user;
        struct Range { pub left: integer, pub right: integer }
        struct Buffer { pub value: integer }
        impl Index<core::ops::Bounds, integer> for Buffer {
            fn index(self, r: core::ops::Bounds) -> integer { return self.value + r.left; }
        }
        impl Index<Range, real> for Buffer {
            fn index(self, r: Range) -> real { return real(r.right); }
        }
        impl IndexAssign<core::ops::Bounds, integer> for Buffer {
            fn index_assign(self, r: core::ops::Bounds, value: integer) {
                self.value = value + r.left;
            }
        }
        impl IndexAssign<Range, real> for Buffer {
            fn index_assign(self, r: Range, value: real) { self.value = integer(value) + r.left; }
        }
        fn make() -> core::ops::Bounds { return 1..2; }
        fn read(buffer: Buffer) -> integer { return buffer[1..2]; }
        fn read_user(buffer: Buffer, r: Range) -> real { return buffer[r]; }
        entity E {}
        impl E { let buffer: Buffer; process { buffer[1..2] = 4; } }";
    for core_first in [false, true] {
        let mut sink = DiagnosticSink::default();
        let core = "module core::ops;
            pub struct Bounds { pub left: integer, pub right: integer }
            attr lang for Bounds = \"range\";";
        let mut modules = with_core(
            vec![
                crate::syntax::parse_module(FileId(0), source, &mut sink),
                crate::syntax::parse_module(FileId(1), core, &mut sink),
            ],
            &mut sink,
        );
        if core_first {
            modules.swap(0, 1);
        }
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = check(&modules, &resolved, &mut sink);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        let owner = resolved.lang("range").unwrap();
        let user = modules
            .iter()
            .find(|module| module.span.file == FileId(0))
            .unwrap();
        for item in &user.items {
            let Item::Fn(function) = item else { continue };
            let Stmt::Return {
                value: Some(value), ..
            } = &function.body.as_ref().unwrap().stmts[0]
            else {
                panic!("return fixture")
            };
            let expected = match function.name.text.as_str() {
                "make" => Ty::Named(owner),
                "read" => Ty::Integer,
                "read_user" => Ty::Real,
                other => panic!("unexpected function {other}"),
            };
            assert_eq!(typed.expr_type(expr_span(value)), Some(&expected));
        }
    }
}

#[test]
fn range_writes_do_not_select_an_equal_named_user_contract() {
    let source = "module user;
        struct Range { pub left: integer, pub right: integer }
        struct Buffer { pub value: integer }
        impl IndexAssign<core::ops::Bounds, integer> for Buffer {
            fn index_assign(self, r: core::ops::Bounds, value: integer) { self.value = value; }
        }
        impl IndexAssign<Range, real> for Buffer {
            fn index_assign(self, r: Range, value: real) { self.value = integer(value); }
        }
        entity E {}
        impl E { let buffer: Buffer; process { buffer[1..2] = 4.0; } }";
    let mut sink = DiagnosticSink::default();
    let core = "module core::ops;
        pub struct Bounds { pub left: integer, pub right: integer }
        attr lang for Bounds = \"range\";";
    let modules = with_core(
        vec![
            crate::syntax::parse_module(FileId(0), source, &mut sink),
            crate::syntax::parse_module(FileId(1), core, &mut sink),
        ],
        &mut sink,
    );
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    check(&modules, &resolved, &mut sink);
    assert_eq!(sink.error_count(), 1, "{:?}", sink.diagnostics());
    assert!(sink
        .diagnostics()
        .iter()
        .any(|diagnostic| diagnostic.code == Some(codes::TYPE_MISMATCH)
            && diagnostic.message.contains("IndexAssign")));
}

#[test]
fn a_user_range_cannot_supply_an_unregistered_range_role() {
    let mut sink = DiagnosticSink::default();
    let source = "module user; struct Range { pub left: integer, pub right: integer }
        fn make() -> Range { return 1..2; }";
    let module = crate::syntax::parse_module(FileId(0), source, &mut sink);
    let modules = with_core(vec![module], &mut sink);
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    assert!(resolved.lang("range").is_none());
    let typed = check(&modules, &resolved, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let Item::Fn(function) = &modules[0].items[1] else {
        panic!("function")
    };
    let Stmt::Return {
        value: Some(value), ..
    } = &function.body.as_ref().unwrap().stmts[0]
    else {
        panic!("range")
    };
    assert_eq!(typed.expr_type(expr_span(value)), Some(&Ty::Error));
}

fn assert_results(modules: &[Module], typed: &Typed, expected: &Ty) {
    let mut checked = 0;
    for item in modules.iter().flat_map(|module| &module.items) {
        let Item::Fn(function) = item else { continue };
        let Stmt::Return {
            value: Some(value), ..
        } = &function.body.as_ref().unwrap().stmts[0]
        else {
            panic!("return fixture")
        };
        assert_eq!(
            typed.expr_type(expr_span(value)),
            Some(expected),
            "{}",
            function.name.text
        );
        checked += 1;
    }
    assert_eq!(checked, 4);
}
