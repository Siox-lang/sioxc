//! Compiler-created values use registered declarations, never matching leaves.

use super::*;

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
