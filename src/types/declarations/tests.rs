use super::*;
use crate::diag::{DiagnosticSink, FileId};
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
            index.integer_function_path(path).is_none(),
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
