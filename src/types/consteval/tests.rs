use super::*;
use crate::diag::{DiagnosticSink, FileId};
use crate::syntax::{ast::Item, parse_module};

mod aggregate;
mod derived;
mod numeric;

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
fn physical_and_symbolic_enum_values_are_semantically_equivalent() {
    let mut sink = DiagnosticSink::default();
    let sources = [
        "module model; use core::primitive::Bool;
         impl Eq<Bool> for Bool { fn eq(self, rhs: Bool) -> Bool { return self; } }
         fn compare(value: Bool) -> Bool { return value == value; }",
        "module core::primitive;
         pub enum Bool { false = 5, true = 17 } attr lang for Bool = \"bool\";",
    ];
    let modules: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(i, source)| parse_module(FileId(i as u32), source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let enums = crate::types::enum_values::EnumValues::collect(&declarations);
    let source = Source::new(&declarations).with_enums(&enums);
    let Item::Fn(function) = &modules[0].items[2] else {
        panic!("function")
    };
    let value = Value::Enum {
        owner: resolved.lang("bool").unwrap(),
        discriminant: 17,
    };
    let result = evaluate_function(function, vec![value.clone()], &source, 0);
    assert_eq!(result, Ok(value.clone()));
    assert_eq!(
        source.equivalent(
            &value,
            &Value::Variant {
                owner: resolved.lang("bool").unwrap(),
                variant: resolved
                    .enum_variant(resolved.lang("bool").unwrap(), "true")
                    .unwrap(),
            }
        ),
        Some(true),
        "raw and symbolic true mean the same source value"
    );
}

#[test]
fn typed_character_values_do_not_masquerade_as_integers() {
    let source = expression("'A'");
    let environment = ConstantExpressions(|_: &Path| None, std::marker::PhantomData);
    assert_eq!(
        evaluate_value(&source, &environment, None, 0),
        Ok(Value::Character('A'))
    );
    assert_eq!(
        evaluate(&source, &environment, 0).unwrap_err(),
        NotConstant {
            span: expr_span(&source),
            reason: Reason::Unsupported
        }
    );
}

#[test]
fn string_literal_values_count_code_points_without_becoming_integer_constants() {
    let environment = ConstantExpressions(|_: &Path| None, std::marker::PhantomData);
    for text in ["", "é🦀", "é", "A_0"] {
        let expression = expression(&format!("\"{text}\""));
        let expected: Vec<_> = text.chars().map(Value::Character).collect();
        let Value::Array {
            element,
            values,
            left,
            descending,
        } = evaluate_value(&expression, &environment, None, 0).unwrap()
        else {
            panic!("strings are character arrays")
        };
        assert_eq!(*element, Ty::Char);
        assert_eq!(values.as_ref(), expected);
        assert_eq!(left, 0);
        assert!(!descending);
        assert_eq!(
            evaluate(&expression, &environment, 0).unwrap_err().reason,
            Reason::Unsupported
        );
    }
}

#[test]
fn source_receiver_and_named_parameters_use_their_resolved_declaration_ids() {
    let source = "module model; struct Counter { value: integer }
        impl Counter { fn add(self, amount: integer) -> integer { return self.value + amount; } }";
    let mut sink = DiagnosticSink::new();
    let module = parse_module(FileId(0), source, &mut sink);
    let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let Item::Impl(implementation) = &module.items[1] else {
        panic!("impl");
    };
    let crate::syntax::ast::ImplItem::Fn(function) = &implementation.items[0] else {
        panic!("function");
    };
    let declarations = DeclarationIndex::collect(&resolved, std::slice::from_ref(&module));
    let receiver = declarations.parameter_id(&function.params[0]).unwrap();
    let amount = declarations.parameter_id(&function.params[1]).unwrap();
    assert_ne!(receiver, amount);
    let mut seen = 0;
    crate::syntax::visit::read::function(function, &mut |_| {}, &mut |expression| {
        if let Expr::Path(path) = expression {
            match path.segments.last().unwrap().text.as_str() {
                "self" => {
                    assert_eq!(declarations.local_id(path), Some(receiver));
                    seen += 1;
                }
                "amount" => {
                    assert_eq!(declarations.local_id(path), Some(amount));
                    seen += 1;
                }
                _ => {}
            }
        }
    });
    assert_eq!(seen, 2);
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
fn literals_use_the_analysis_domain_before_word_projection() {
    for (text, expected) in [
        ("18446744073709551616", 1i128 << 64),
        ("0x1_0000_0000_0000_0000", 1i128 << 64),
        ("0X7fff_ffff_ffff_ffff_ffff_ffff_ffff_ffff", i128::MAX),
        ("-170141183460469231731687303715884105728", i128::MIN),
        ("-0x8000_0000_0000_0000_0000_0000_0000_0000", i128::MIN),
    ] {
        assert_eq!(literal(text), Ok(expected), "{text}");
    }
    for text in [
        "170141183460469231731687303715884105728",
        "340282366920938463463374607431768211456",
        "-170141183460469231731687303715884105729",
    ] {
        assert_eq!(
            literal(text).unwrap_err().reason,
            Reason::Overflow,
            "{text}"
        );
    }
    let environment = Functions::new("");
    let word = Functions {
        domain: IntegerDomain::NativeWord,
        ..environment
    };
    assert_eq!(
        word.eval("18446744073709551616").unwrap_err().reason,
        Reason::Overflow
    );
    assert_eq!(word.eval("-9223372036854775808"), Ok(i128::from(i64::MIN)));
}

#[test]
fn extreme_analysis_ranges_do_not_overflow_or_fabricate_native_labels() {
    use super::super::Checker;

    let mut sink = DiagnosticSink::default();
    let resolved = crate::resolve::resolve(&[], &mut sink);
    let checker = Checker::new(&mut sink, &resolved, &[]);
    for text in [
        "(-170141183460469231731687303715884105728)..170141183460469231731687303715884105727",
        "170141183460469231731687303715884105727..170141183460469231731687303715884105727",
    ] {
        assert_eq!(checker.index_shape(&expression(text)), (0, 0), "{text}");
    }
    assert_eq!(
        checker.index_shape(&expression("(-9223372036854775808)..9223372036854775807")),
        (0, i64::MIN)
    );
    assert_eq!(checker.index_shape(&expression("11..10")), (2, 10));
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
        self.globals
            .get(&key)
            .copied()
            .map(|value| Binding::Value(Value::Integer(value)))
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
        if n > 0 { unsupported(); }
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
    assert_eq!(evaluate_value_statements(&[], &env, None, 0), Ok(None));
}

#[test]
fn resolved_matches_keep_enum_owners_imports_and_source_defined_order() {
    let sources = [
        "module model;
         use core::ops::Order;
         use core::primitive::Decision;
         enum State { Idle = 90, Busy = 4, Done = 17, Outside = 40, Unordered = 53 }
         type Alias = State;
         enum Glyph { 'a' = 13, 'b' = 7 }
         type Letter = Glyph;
         fn letter(value: Letter) -> integer {
             return match value { 'a' => 1, 'b' => 2, _ => 3 };
         }
         fn rank(value: State) -> integer {
             return match value {
                 State::Idle => 0, State::Busy => 1,
                 State::Done => 2, State::Outside => 3, State::Unordered => 4
             };
         }
         impl Order<State> for State {
             fn le(self, rhs: State) -> Decision {
                 if rank(self) == 4 { return false; }
                 if rank(rhs) == 4 { return false; }
                 return rank(self) <= rank(rhs);
             }
         }
         fn classify(value: Alias) -> integer {
             return match value { (Alias::Idle)..(Alias::Done) => 9, _ => 3 };
         }
         fn local_import(value: State, Done: integer) -> integer {
             use Selected = model::Alias;
             match value {
                 Selected::Busy | Selected::Done => { return 12; }
                 _ => { }
             }
             return 7;
         }
         const INSIDE: integer = classify(State::Busy);
         const OUTSIDE: integer = classify(State::Outside);
         const IMPORTED: integer = local_import(State::Done, 99);
         const OTHER: integer = local_import(State::Idle, 99);
         fn unordered_bound(value: State) -> integer {
             return match value { (State::Unordered)..(State::Done) => 9, _ => 3 };
         }
         const UNORDERED: integer = classify(State::Unordered);
         const UNORDERED_BOUND: integer = unordered_bound(State::Busy);
         const LETTER: integer = letter('b');",
        "module core::primitive;
         pub enum Decision { false = 5, true = 17 }
         attr lang for Decision = \"bool\";",
        "module core::ops;
         use core::primitive::Decision;
         pub trait Order<Rhs> { fn le(self, rhs: Rhs) -> Decision; }
         attr lang for Order = \"ord\";",
    ];
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
    for (name, expected) in [
        ("INSIDE", 9),
        ("OUTSIDE", 3),
        ("IMPORTED", 12),
        ("OTHER", 7),
        ("UNORDERED", 3),
        ("UNORDERED_BOUND", 3),
        ("LETTER", 2),
    ] {
        let value = modules[0]
            .items
            .iter()
            .find_map(|item| match item {
                Item::Const(constant) if constant.name.text == name => Some(&constant.value),
                _ => None,
            })
            .unwrap();
        assert_eq!(evaluate(value, &source, 0), Ok(expected), "{name}");
    }
    for (name, expected) in [
        ("classify", RangeOrder::Forward),
        ("unordered_bound", RangeOrder::Unordered),
    ] {
        let function = modules[0]
            .items
            .iter()
            .find_map(|item| match item {
                Item::Fn(function) if function.name.text == name => Some(function),
                _ => None,
            })
            .unwrap();
        let Stmt::Return {
            value: Some(Expr::Match { arms, .. }),
            ..
        } = &function.body.as_ref().unwrap().stmts[0]
        else {
            panic!("expected range match");
        };
        let Pattern::Bounds {
            lo: Some(left),
            hi: Some(right),
            ..
        } = &arms[0].pattern
        else {
            panic!("expected bounds");
        };
        assert_eq!(
            evaluate_range_order(left, right, &source, None, 0),
            Ok(expected)
        );
        if expected == RangeOrder::Forward {
            assert_eq!(
                evaluate_range_order(right, left, &source, None, 0),
                Ok(RangeOrder::Reversed)
            );
        }
    }
    let (owner, _) = declarations
        .enumerations()
        .find(|(_, declaration)| declaration.name.text == "Glyph")
        .unwrap();
    let function = modules[0]
        .items
        .iter()
        .find_map(|item| match item {
            Item::Fn(function) if function.name.text == "letter" => Some(function),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        evaluate_function(
            function,
            vec![Value::Enum {
                owner,
                discriminant: 7
            }],
            &source,
            0,
        ),
        Ok(Value::Integer(2)),
        "stored enum values and symbolic members match the same character pattern"
    );
}

#[test]
fn scalar_matches_select_only_the_first_matching_arm() {
    let mut env = Functions::new("");
    env.globals.insert("DEPTH".into(), 6);
    for (source, expected) in [
        ("match 4 { 9..3 => 7, 4 => 99, _ => 1 / 0 }", 7),
        ("match 4 { 0 | 2 | 4 => 8, _ => UNKNOWN }", 8),
        ("match 4 { 0..DEPTH - 1 => 9, _ => UNKNOWN }", 9),
        ("match 4 { DEPTH..(DEPTH - 4) => 10, _ => 0 }", 10),
        ("match 4 { ..3 => UNKNOWN, 4.. => 11 }", 11),
        ("match 'λ' { 'a' => UNKNOWN, 'λ' => 12, _ => 0 }", 12),
        ("match 'λ' { ('α')..('ω') => 13, _ => 0 }", 13),
    ] {
        assert_eq!(env.eval(source), Ok(expected), "{source}");
    }
}

#[test]
fn range_direction_keeps_exact_native_integer_endpoints() {
    let mut env = Functions::new("");
    env.domain = IntegerDomain::NativeWord;
    let high = expression("9223372036854775807");
    let lower = expression("9223372036854775806");
    assert_eq!(
        evaluate_range_order(&high, &lower, &env, None, 0),
        Ok(RangeOrder::Reversed)
    );
    assert_eq!(
        evaluate_range_order(&lower, &high, &env, None, 0),
        Ok(RangeOrder::Forward)
    );
    let missing = expression("MISSING");
    assert_eq!(
        evaluate_range_order(&high, &missing, &env, None, 0)
            .unwrap_err()
            .reason,
        Reason::UnknownConstant
    );
}

#[test]
fn failed_match_conditions_and_selected_values_never_fall_through() {
    let env = Functions::new("");
    for (source, reason) in [
        ("match UNKNOWN { _ => 7 }", Reason::UnknownConstant),
        (
            "match 2 { 0..UNKNOWN => 3, _ => 7 }",
            Reason::UnknownConstant,
        ),
        ("match 2 { 2 => 1 / 0, _ => 7 }", Reason::DivisionByZero),
        ("match 2 { 1 => 3 }", Reason::Unsupported),
    ] {
        let error = env.eval(source).unwrap_err();
        assert_eq!(error.reason, reason, "{source}: {error:?}");
        assert!(error.span.end > error.span.start);
    }
}

#[test]
fn match_statements_preserve_scope_mutations_and_early_returns() {
    let env = Functions::new(
        "fn select(n: integer) -> integer {
            let result: integer = 2;
            match n {
                0 => { let temporary: integer = 3; result = result + temporary; }
                1 => { return 11; }
                2 => { }
            }
            return result;
        }
        fn shadow(n: integer) -> integer {
            let value: integer = 8;
            match n {
                0 => { let value: integer = 99; }
                _ => { unsupported(); }
            }
            return value;
        }
        fn fail(n: integer) -> integer {
            match n { 0 => { return UNKNOWN; } _ => { } }
            return 7;
        }",
    );
    for (source, expected) in [
        ("select(0)", 5),
        ("select(1)", 11),
        ("select(2)", 2),
        ("select(3)", 2),
        ("shadow(0)", 8),
        ("fail(1)", 7),
    ] {
        assert_eq!(env.eval(source), Ok(expected), "{source}");
    }
    assert_eq!(
        env.eval("fail(0)").unwrap_err().reason,
        Reason::UnknownConstant
    );
    assert_eq!(
        env.eval("shadow(1)").unwrap_err().reason,
        Reason::Unsupported
    );
}

#[test]
fn discarded_pure_expressions_are_evaluated_not_skipped() {
    let env = Functions::new(
        "fn divide(n: integer) -> integer { return 8 / n; }
         fn discard(n: integer) -> integer { divide(n); return 7; }
         fn foreign(n: integer) -> integer;
         fn effect() -> integer { foreign(1); return 7; }",
    );
    assert_eq!(env.eval("discard(2)"), Ok(7));
    assert_eq!(
        env.eval("discard(0)").unwrap_err().reason,
        Reason::DivisionByZero
    );
    assert_eq!(
        env.eval("effect()").unwrap_err().reason,
        Reason::Unsupported
    );
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
        "fn fail(n: integer) -> integer { let local: integer = n; return local / 0; }
        fn match_fail(n: integer) -> integer {
            match n { 0 => { let local: integer = 7; return local / 0; } }
            return 8;
        }
        fn identity(n: integer) -> integer { return n; }",
    );
    let mut interpreter = Interpreter {
        environment: &env,
        locals: Vec::new(),
        operators: Vec::new(),
    };
    assert_eq!(
        interpreter
            .expression(&expression("fail(3)"), None, 0)
            .unwrap_err()
            .reason,
        Reason::DivisionByZero
    );
    assert!(interpreter.locals.is_empty());
    assert_eq!(
        interpreter
            .expression(&expression("match_fail(0)"), None, 0)
            .unwrap_err()
            .reason,
        Reason::DivisionByZero
    );
    assert!(interpreter.locals.is_empty());
    assert_eq!(
        interpreter.expression(&expression("identity(8)"), None, 0),
        Ok(Value::Integer(8))
    );
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
fn aggregate_attributes_keep_owning_spans_after_cross_file_substitution() {
    fn parse_value(file: FileId, text: &str) -> Expr {
        let mut sink = DiagnosticSink::default();
        let module = parse_module(
            file,
            &format!("module attributes; const VALUE: integer = {text};"),
            &mut sink,
        );
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        let Item::Const(value) = module.items.into_iter().next().unwrap() else {
            panic!("constant")
        };
        value.value
    }

    let environment = ConstantExpressions(|_: &Path| None, std::marker::PhantomData);
    for (attribute_file, base_file) in [(FileId(0), FileId(2)), (FileId(2), FileId(0))] {
        for (attribute, text, expected) in [
            ("length", "\"é🦀\"", Some(2)),
            ("high", "\"é🦀\"", Some(1)),
            ("low", "\"é🦀\"", Some(0)),
            ("length", "runtime", None),
            ("length", "17", None),
            ("high", "\"\"", None),
            ("missing", "\"é🦀\"", None),
        ] {
            let mut expanded = parse_value(attribute_file, &format!("self'{attribute}"));
            let owning_span = expr_span(&expanded);
            let Expr::SysAttr { base, attr, .. } = &mut expanded else {
                panic!("attribute")
            };
            *base = Box::new(parse_value(base_file, text));
            assert_ne!(expr_span(base).file, attr.span.file);
            assert_eq!(
                evaluate_value(&expanded, &environment, None, 0),
                expected.map(Value::Integer).ok_or(NotConstant {
                    span: owning_span,
                    reason: Reason::Unsupported,
                }),
                "{text}'{attribute}"
            );
        }

        let mut expanded = parse_value(attribute_file, "self'length");
        let Expr::SysAttr { base, .. } = &mut expanded else {
            panic!("attribute")
        };
        *base = Box::new(parse_value(base_file, "1 / 0"));
        let arithmetic_span = expr_span(base);
        assert_eq!(
            evaluate_value(&expanded, &environment, None, 0),
            Err(NotConstant {
                span: arithmetic_span,
                reason: Reason::DivisionByZero,
            }),
            "base-expression failures keep their original reason and source"
        );
    }
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
