use super::*;
use crate::diag::{DiagnosticSink, FileId};
use crate::syntax::{ast::Item, parse_module};

fn expression(text: &str) -> Expr {
    let source = format!("module constants; const VALUE: integer = {text};");
    let mut sink = DiagnosticSink::default();
    let module = parse_module(FileId(0), &source, &mut sink);
    assert!(!sink.has_errors(), "{text}: {:?}", sink.diagnostics());
    let Item::Const(value) = module.items.into_iter().next().unwrap() else {
        panic!("expected a constant");
    };
    value.value
}

fn literal(text: &str) -> Result<i128, NotConstant> {
    integer(&expression(text), |_| None)
}

#[test]
fn arithmetic_keeps_the_analysis_domain() {
    for (text, expected) in [
        ("0xF_f + 0b1_0", 257),
        ("-(2 + 3) * 4", -20),
        ("17 / 3 + 17 % 3", 7),
        ("(3 << 5) >> 2", 24),
        ("-0x8000_0000_0000_0000", i128::from(i64::MIN)),
        ("0xffff_ffff_ffff_ffff", i128::from(u64::MAX)),
        ("(-0x8000_0000_0000_0000) / -1", 1i128 << 63),
        ("(-0x8000_0000_0000_0000) % -1", 0),
        ("(0x7fff_ffff_ffff_ffff + 1) - 1", i128::from(i64::MAX)),
    ] {
        assert_eq!(literal(text), Ok(expected), "{text}");
    }
}

#[test]
fn failures_are_spanned_and_do_not_panic() {
    for (text, expected) in [
        ("5 / 0", Reason::DivisionByZero),
        ("5 % 0", Reason::DivisionByZero),
        ("1 << -1", Reason::InvalidShift),
        ("1 << 128", Reason::InvalidShift),
        ("1 >> 4294967296", Reason::InvalidShift),
        (
            "0xffff_ffff_ffff_ffff * 0xffff_ffff_ffff_ffff",
            Reason::Overflow,
        ),
        ("(-1 << 127) / -1", Reason::Overflow),
        ("(-1 << 127) % -1", Reason::Overflow),
        ("-(-1 << 127)", Reason::Overflow),
        ("RUNTIME", Reason::UnknownConstant),
        ("3 and 2", Reason::Unsupported),
        ("not 0", Reason::Unsupported),
        ("3 == 3", Reason::Unsupported),
        ("function(2)", Reason::Unsupported),
    ] {
        let expression = expression(text);
        let error = integer(&expression, |_| None).unwrap_err();
        assert_eq!(error.reason, expected, "{text}");
        assert_eq!(error.span, expr_span(&expression));
    }
}

#[test]
fn declarations_are_supplied_by_identity_not_leaf_name() {
    let value = expression("6 * 7");
    let alias = expression("library::VALUE");
    let expr = expression("ALIAS + 1");
    let result = integer(&expr, |path| {
        let names: Vec<_> = path.segments.iter().map(|s| s.text.as_str()).collect();
        match names.as_slice() {
            ["ALIAS"] => Some(&alias),
            ["library", "VALUE"] => Some(&value),
            _ => None,
        }
    });
    assert_eq!(result, Ok(43));
    assert_eq!(
        integer(&expression("VALUE"), |_| None).unwrap_err().reason,
        Reason::UnknownConstant
    );
}

#[test]
fn cyclic_declarations_stop() {
    let a = expression("B");
    let b = expression("A");
    let result = integer(&a, |path| match path.segments[0].text.as_str() {
        "A" => Some(&a),
        "B" => Some(&b),
        _ => None,
    });
    assert_eq!(result.unwrap_err().reason, Reason::RecursionLimit);
}

#[test]
fn checker_literal_boundary_is_checked_after_evaluation() {
    use super::super::Checker;

    for (text, expected) in [
        ("-0x8000_0000_0000_0000", Some(i64::MIN)),
        ("(-0x8000_0000_0000_0000) / -1", None),
        ("(-0x8000_0000_0000_0000) % -1", Some(0)),
        ("0xffff_ffff_ffff_ffff", None),
        ("(0x7fff_ffff_ffff_ffff + 1) - 1", Some(i64::MAX)),
        ("-(2 + 3)", Some(-5)),
        ("5 / 0", None),
    ] {
        assert_eq!(
            Checker::const_literal(&expression(text)),
            expected,
            "{text}"
        );
    }
}

#[test]
fn checker_keeps_qualified_constants_and_parameter_shadowing() {
    use super::super::Checker;

    let mut sink = DiagnosticSink::default();
    let modules: Vec<_> = [
        "module alpha; pub const VALUE: integer = 300;",
        "module beta; pub const VALUE: integer = 2;",
        "module client; const TOTAL: integer = alpha::VALUE + beta::VALUE;
         fn identity(VALUE: integer) -> integer { return VALUE; }",
    ]
    .iter()
    .enumerate()
    .map(|(i, source)| parse_module(FileId(i as u32), source, &mut sink))
    .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let mut checker = Checker::new(&mut sink, &resolved, &modules);
    checker.collect(&modules);
    let Item::Const(total) = &modules[2].items[0] else {
        panic!("expected constant")
    };
    assert_eq!(checker.const_int(&total.value), Some(302));
    assert_eq!(checker.constant_value(&total.value), Some(302));
    assert_eq!(Checker::const_literal(&total.value), None);
    let Item::Fn(function) = &modules[2].items[1] else {
        panic!("expected function")
    };
    let crate::syntax::ast::Stmt::Return {
        value: Some(value), ..
    } = &function.body.as_ref().unwrap().stmts[0]
    else {
        panic!("expected return")
    };
    assert_eq!(checker.const_int(value), None);
    assert_eq!(checker.constant_value(value), None);
}

#[test]
fn failure_points_to_the_operand_that_could_not_be_evaluated() {
    let e = expression("1 + (2 / 0)");
    let Expr::Binary { rhs, .. } = &e else {
        unreachable!()
    };
    let error = integer(&e, |_| None).unwrap_err();
    assert_eq!(error.reason, Reason::DivisionByZero);
    assert_eq!(error.span, expr_span(rhs));
}

struct Functions {
    module: crate::syntax::Module,
    globals: std::collections::HashMap<String, i128>,
    domain: IntegerDomain,
}

impl Functions {
    fn new(source: &str) -> Self {
        let mut sink = DiagnosticSink::default();
        let module = parse_module(FileId(1), &format!("module functions; {source}"), &mut sink);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        Self {
            module,
            globals: Default::default(),
            domain: IntegerDomain::Analysis,
        }
    }

    fn eval(&self, source: &str) -> Result<i128, NotConstant> {
        evaluate(&expression(source), self, 0)
    }
}

impl Environment for Functions {
    fn binding(&self, path: &Path) -> Option<Binding<'_>> {
        let key = path
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("::");
        self.globals.get(&key).copied().map(Binding::Value)
    }

    fn domain(&self) -> IntegerDomain {
        self.domain
    }

    fn function(&self, callee: &Expr) -> Option<&FnDecl> {
        let Expr::Path(path) = callee else {
            return None;
        };
        let [name] = path.segments.as_slice() else {
            return None;
        };
        self.module.items.iter().find_map(|item| match item {
            Item::Fn(f) if f.name.text == name.text => Some(f),
            _ => None,
        })
    }

    fn binary(&self, expression: &Expr, a: i128, b: i128) -> Option<i128> {
        let Expr::Binary { op, .. } = expression else {
            return None;
        };
        Some(match op {
            BinOp::Eq => (a == b) as i128,
            BinOp::Le => (a <= b) as i128,
            BinOp::Gt => (a > b) as i128,
            _ => return None,
        })
    }

    fn condition(&self, value: i128) -> Option<bool> {
        Some(value != 0)
    }

    fn attribute(&self, base: &Expr, attr: &Ident) -> Option<i128> {
        let Expr::Path(path) = base else { return None };
        let [name] = path.segments.as_slice() else {
            return None;
        };
        self.globals
            .get(&format!("{}::{}", name.text, attr.text))
            .copied()
    }

    fn transparent_conversion(&self, callee: &Expr) -> bool {
        matches!(callee, Expr::Path(path) if matches!(path.segments.as_slice(), [name] if name.text == "integer"))
    }
}

#[test]
fn integer_domains_keep_checked_intermediates_and_native_shift_bits() {
    let analysis = Functions::new("");
    let mut word = Functions::new("");
    word.domain = IntegerDomain::NativeWord;
    let overflow = "(0x7fff_ffff_ffff_ffff + 1) - 1";
    assert_eq!(analysis.eval(overflow), Ok(i128::from(i64::MAX)));
    assert_eq!(word.eval(overflow).unwrap_err().reason, Reason::Overflow);
    assert_eq!(analysis.eval("(1 << 63) >> 63"), Ok(1));
    assert_eq!(word.eval("(1 << 63) >> 63"), Ok(-1));
    assert_eq!(word.eval("1 << 63"), Ok(i128::from(i64::MIN)));
    assert_eq!(
        word.eval("1 << 64").unwrap_err().reason,
        Reason::InvalidShift
    );
    assert_eq!(analysis.eval("1 << 64"), Ok(1i128 << 64));
    assert_eq!(
        word.eval("-(1 << 63)").unwrap_err().reason,
        Reason::Overflow
    );
    assert_eq!(
        word.eval("(1 << 63) % -1").unwrap_err().reason,
        Reason::Overflow
    );
    assert_eq!(analysis.eval("(1 << 63) % -1"), Ok(0));
    assert_eq!(
        word.eval("-0x8000_0000_0000_0000"),
        Ok(i128::from(i64::MIN))
    );
    assert_eq!(
        word.eval("0x8000_0000_0000_0000").unwrap_err().reason,
        Reason::Overflow
    );
    assert_eq!(
        word.eval("-(-0x8000_0000_0000_0000)").unwrap_err().reason,
        Reason::Overflow
    );
}

#[test]
fn minimum_literal_does_not_require_a_context_to_accept_its_positive_magnitude() {
    struct SignedLiterals;
    impl Environment for SignedLiterals {
        fn domain(&self) -> IntegerDomain {
            IntegerDomain::NativeWord
        }
        fn binding(&self, _: &Path) -> Option<Binding<'_>> {
            None
        }
        fn literal(&self, text: &str) -> Option<i128> {
            text.parse::<i64>().ok().map(i128::from)
        }
    }
    assert_eq!(
        evaluate(&expression("-9223372036854775808"), &SignedLiterals, 0),
        Ok(i128::from(i64::MIN))
    );
    assert_eq!(
        evaluate(&expression("9223372036854775808"), &SignedLiterals, 0)
            .unwrap_err()
            .reason,
        Reason::Overflow
    );
}

#[test]
fn recursive_functions_and_local_frames_share_the_interpreter() {
    let mut env = Functions::new(
        "fn fib(n: integer) -> integer {
        if n <= 1 { return n; }
        return fib(n - 1) + fib(n - 2);
    }
    fn outer(x: integer) -> integer { return inner(x + 1) + x; }
    fn inner(x: integer) -> integer { return x * 2; }
    fn bound(BASE: integer) -> integer { return library::BASE + BASE; }",
    );
    env.globals.insert("library::BASE".into(), 7);
    assert_eq!(env.eval("fib(10)"), Ok(55));
    assert_eq!(env.eval("outer(3)"), Ok(11));
    assert_eq!(env.eval("bound(90)"), Ok(97));
    assert_eq!(env.globals["library::BASE"], 7);
}

#[test]
fn failed_selected_branch_does_not_fall_through_to_a_later_return() {
    let env = Functions::new(
        "fn value(n: integer) -> integer {
        if n > 0 { return RUNTIME; }
        return 7;
    }
    fn statement(n: integer) -> integer {
        if n > 0 { let local: integer = 3; }
        return 7;
    }",
    );
    assert_eq!(
        env.eval("value(1)").unwrap_err().reason,
        Reason::UnknownConstant
    );
    assert_eq!(
        env.eval("statement(1)").unwrap_err().reason,
        Reason::Unsupported
    );
    assert_eq!(env.eval("value(0)"), Ok(7));
    assert_eq!(env.eval("statement(0)"), Ok(7));
}

#[test]
fn empty_selected_branches_and_early_returns_are_control_flow_not_errors() {
    let env = Functions::new(
        "fn empty(n: integer) -> integer {
        if n > 0 { }
        return 7;
    }
    fn early() -> integer { return 5; unsupported(); }",
    );
    assert_eq!(env.eval("empty(1)"), Ok(7));
    assert_eq!(env.eval("early()"), Ok(5));
    assert_eq!(evaluate_statements(&[], &env, 0), Ok(None));
}

#[test]
fn only_the_selected_expression_and_else_if_branch_is_evaluated() {
    let env = Functions::new(
        "fn select(n: integer) -> integer {
        if n == 0 { return 2; }
        else if n == 1 { return 3; }
        else { return 4; }
    }",
    );
    assert_eq!(env.eval("if 1 == 0 { 1 / 0 } else { 7 }"), Ok(7));
    assert_eq!(env.eval("select(0)"), Ok(2));
    assert_eq!(env.eval("select(1)"), Ok(3));
    assert_eq!(env.eval("select(2)"), Ok(4));
}

#[test]
fn argument_arity_and_recursion_are_bounded_without_executing_foreign_code() {
    let env = Functions::new(
        "fn identity(n: integer) -> integer { return n; }
        fn forever(n: integer) -> integer { return forever(n); }
        fn foreign(n: integer) -> integer;",
    );
    for source in ["identity()", "identity(1, 2)", "integer()", "integer(1, 2)"] {
        assert_eq!(
            env.eval(source).unwrap_err().reason,
            Reason::ArityMismatch,
            "{source}"
        );
    }
    assert_eq!(
        env.eval("forever(1)").unwrap_err().reason,
        Reason::RecursionLimit
    );
    assert_eq!(
        env.eval("foreign(1)").unwrap_err().reason,
        Reason::Unsupported
    );
    assert_eq!(env.eval("integer(7 + 1)"), Ok(8));
}

#[test]
fn function_frames_are_restored_after_errors() {
    let env = Functions::new(
        "fn fail(n: integer) -> integer { return n / 0; }
        fn identity(n: integer) -> integer { return n; }",
    );
    let mut interpreter = Interpreter {
        environment: &env,
        locals: Vec::new(),
    };
    assert_eq!(
        interpreter
            .expression(&expression("fail(3)"), 0)
            .unwrap_err()
            .reason,
        Reason::DivisionByZero
    );
    assert!(interpreter.locals.is_empty());
    assert_eq!(interpreter.expression(&expression("identity(8)"), 0), Ok(8));
    assert!(interpreter.locals.is_empty());
}

#[test]
fn range_attributes_are_supplied_by_the_binding_environment() {
    let mut env = Functions::new("");
    env.globals.insert("bus::length".into(), 8);
    assert_eq!(env.eval("bus'length + 1"), Ok(9));
    assert_eq!(
        env.eval("other'length").unwrap_err().reason,
        Reason::Unsupported
    );
}

#[test]
fn else_if_chains_do_not_consume_function_recursion_depth() {
    let mut source = "fn select(n: integer) -> integer { ".to_string();
    for n in 0..96 {
        if n != 0 {
            source.push_str("else ");
        }
        source.push_str(&format!("if n == {n} {{ return {n}; }} "));
    }
    source.push_str("else { return 96; } }");
    let env = Functions::new(&source);
    assert_eq!(env.eval("select(95)"), Ok(95));
    assert_eq!(env.eval("select(96)"), Ok(96));
}
