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
