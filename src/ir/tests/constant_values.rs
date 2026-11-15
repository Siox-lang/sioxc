//! Constant table evaluation must retain the source type and declaration IDs.

use super::*;

#[test]
fn typed_constants_materialize_nominal_fields_and_declared_labels() {
    let (_, sink) = lower_with_nonzero_false(
        "module model;
         struct Count(natural);
         struct Rate(real);
         struct Config { pub count: Count, pub gain: Rate }
         struct Envelope(Config);
         const TWO: natural = 2;
         const THREE: natural = 3;
         const FOUR: natural = 4;
         const COUNT: Count = Count(TWO);
         const GAIN: Rate = Rate(-0.0);
         const CONFIG: Envelope = Envelope(Config { .count = COUNT, .gain = GAIN });
         const WORDS: Count[6..4] = [COUNT, Count(THREE), Count(FOUR)];
         const PAIR: (integer, real) = (-7, 2.5);
         const DESCENDING: integer[2..0] = [7, 9, 11];
         entity Top { count: natural out, gain: real out, field: natural out,
                      selected: natural out, pair: real out, index: integer in, dynamic: integer out }
         impl Top {
             count = natural(COUNT);
             gain = real(GAIN);
             field = natural(CONFIG.count);
             selected = natural(WORDS[4]);
             pair = PAIR.1;
             dynamic = DESCENDING[index];
         }",
    );
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
}

#[test]
fn open_constant_arrays_materialize_checked_lengths_without_losing_declared_labels() {
    let (_, sink) = lower_with_nonzero_false(
        "module model;
         type Text = Char[];
         type Numbers = integer[];
         const TEXT: Text = \"é🦀\";
         const WORDS: Numbers = [11, 13, 17];
         const LABELLED: Char[6..4] = \"Aé🦀\";
         entity Top { index: integer in, glyph: Char out, word: integer out,
                      descending: Char out, bounds: integer[5] out,
                      ascending: core::primitive::Decision out }
         impl Top {
             glyph = TEXT[index];
             word = WORDS[index];
             descending = LABELLED[index + 4];
             bounds[0] = TEXT'length;
             bounds[1] = LABELLED'left;
             bounds[2] = LABELLED'right;
             bounds[3] = LABELLED'high;
             bounds[4] = LABELLED'low;
             ascending = TEXT'ascending;
         }",
    );
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
}

fn lower_with_nonzero_false(source: &str) -> (Design, DiagnosticSink) {
    let mut sink = DiagnosticSink::default();
    let sources = [
        source,
        "module core::primitive;
         pub enum Decision { false = 5, true = 17 }
         attr lang for Decision = \"bool\";
         impl core::ops::Boolean for Decision { fn as_bool(self) -> Decision { return self; } }",
        "module core::ops;
         pub trait Boolean { fn as_bool(self) -> core::primitive::Decision; }
         attr lang for Boolean = \"boolean\";
         pub struct Range { pub left: integer, pub right: integer }
         attr lang for Range = \"range\";",
    ];
    let modules: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(i, source)| crate::syntax::parse_module(FileId(i as u32), source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    // Inspect lowering's own diagnostics, independently of frontend lint suppression.
    let mut sink = DiagnosticSink::default();
    let design = lower(&modules, &resolved, &hierarchy, &mut sink);
    (design, sink)
}

#[test]
fn const_context_inactive_indices_use_boolean_meaning_not_encoded_word() {
    let (_, sink) = lower_with_nonzero_false(
        "module model;
        entity Top { values: integer[2] in, output: integer out }
        impl Top {
            for i in 0..0 {
                if false { output = values[i - 1]; }
                else { output = values[i]; }
            }
        }",
    );
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
}

#[test]
fn const_context_generated_lints_select_source_boolean_branch() {
    let source = "module model; entity Top { output: integer out }
        impl Top {
            process {
            if false { output = 1; } else { output = 2; }
            output = 3;
            }
        }";
    let (_, sink) = lower_with_nonzero_false(source);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let assignments: Vec<_> = sink
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code == Some(crate::diag::codes::DEAD_ASSIGNMENT))
        .collect();
    assert_eq!(assignments.len(), 1, "{:?}", sink.diagnostics());
    assert!(
        assignments[0].labels.iter().any(|label| {
            source[label.span.start as usize..label.span.end as usize].contains("output = 2")
        }),
        "{assignments:?}"
    );
    assert!(
        !assignments[0].labels.iter().any(|label| {
            source[label.span.start as usize..label.span.end as usize].contains("output = 1")
        }),
        "{assignments:?}"
    );
}

#[test]
fn const_context_substitution_folds_indices_without_forging_boolean_literals() {
    let mut sink = DiagnosticSink::default();
    let module = crate::syntax::parse_module(
        FileId(0),
        "module model;
        fn arithmetic(choice: integer) -> integer { return choice + 1; }
        fn equal(choice: integer) -> Bool { return choice == 0; }
        fn negated(choice: integer) -> Bool { return not (choice == 0); }",
        &mut sink,
    );
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    for item in &module.items {
        let ast::Item::Fn(function) = item else {
            continue;
        };
        let ast::Stmt::Return {
            value: Some(expression),
            ..
        } = &function.body.as_ref().unwrap().stmts[0]
        else {
            panic!("return")
        };
        let substituted = subst_expr(expression, "choice", 0);
        match function.name.text.as_str() {
            "arithmetic" => {
                assert!(matches!(substituted, ast::Expr::Int { ref text, .. } if text == "1"))
            }
            "equal" => assert!(matches!(
                substituted,
                ast::Expr::Binary {
                    op: ast::BinOp::Eq,
                    ..
                }
            )),
            "negated" => assert!(matches!(
                substituted,
                ast::Expr::Unary {
                    op: ast::UnOp::Not,
                    ..
                }
            )),
            _ => unreachable!(),
        }
    }
}

#[test]
fn const_context_ir_calls_bind_real_arguments_without_erasing_result_types() {
    let mut sink = DiagnosticSink::default();
    let modules = [crate::syntax::parse_module(
        FileId(0),
        "module model;
         type Float = real;
         fn discard(value: Float) -> integer { return 1; }
         const RESULT: integer = discard(2);
         fn fraction() -> Float { return 1.5; }
         const REAL: Float = fraction();
         const CONVERTED: integer = integer(fraction());
         const BAD: integer = discard('a');",
        &mut sink,
    )];
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let mut functions = FunctionIndex::new(&resolved);
    for item in &modules[0].items {
        functions.insert_declaration(item);
    }
    let ast::Item::Const(constant) = &modules[0].items[2] else {
        panic!("constant")
    };
    assert_eq!(
        eval_const_fns(&constant.value, &HashMap::new(), &functions, 0),
        Some(1)
    );
    for (name, expected) in [("REAL", None), ("CONVERTED", Some(1)), ("BAD", None)] {
        let constant = modules[0]
            .items
            .iter()
            .find_map(|item| match item {
                ast::Item::Const(constant) if constant.name.text == name => Some(constant),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            eval_const_fns(&constant.value, &HashMap::new(), &functions, 0),
            expected,
            "{name}"
        );
    }
}

#[test]
fn packed_initializers_fold_typed_call_results_without_losing_widths() {
    let source = "module model;
        fn twice(value: unsigned[8]) -> unsigned[8] { return value * 2; }
        entity Top { output: unsigned[8] out }
        impl Top {
            let small: unsigned[8] = twice(4);
            let wrapped: unsigned[8] = twice(200);
            output = small + wrapped;
        }";
    assert!(
        lower_diagnostics(source)
            .iter()
            .all(|diagnostic| diagnostic.code != Some(crate::diag::codes::NON_CONSTANT_INITIALIZER)),
        "{:?}",
        lower_diagnostics(source)
    );
    let design = lower_src(source);
    for (name, expected) in [("small", 8), ("wrapped", 144)] {
        let signal = design
            .signals
            .iter()
            .find(|signal| signal.path.ends_with(&format!(".{name}")))
            .expect("initialized signal");
        assert_eq!(signal.init, [expected], "{}", signal.path);
    }
}

#[test]
fn packed_local_constant_calls_keep_the_declared_storage_boundary() {
    let source = "module model;
        fn wrapped(value: unsigned[3]) -> integer {
            let next: unsigned[3] = value + 1;
            return integer(next);
        }
        const RESULT: integer = wrapped(7);
        entity Top { output: integer out }
        impl Top { let seed: integer = RESULT; output = seed; }";
    let diagnostics = lower_diagnostics(source);
    assert!(
        diagnostics.iter().all(|diagnostic| diagnostic.code
            != Some(crate::diag::codes::NON_CONSTANT_INITIALIZER)),
        "{diagnostics:?}"
    );
    let design = lower_src(source);
    let seed = design
        .signals
        .iter()
        .find(|signal| signal.path.ends_with(".seed"))
        .unwrap();
    assert_eq!(seed.init, [0]);
}

#[test]
fn typed_initializer_calls_cannot_discard_dynamic_or_foreign_inputs() {
    for input in ["source", "foreign()", "\"0000X001\""] {
        let source = format!(
            "module model;
             extern \"C\" {{ fn foreign() -> integer; }}
             fn discard(value: unsigned[8]) -> unsigned[8] {{ return 3; }}
             entity Top {{ source: unsigned[8] in, output: unsigned[8] out }}
             impl Top {{ let value: unsigned[8] = discard({input}); output = value; }}"
        );
        let diagnostics = lower_diagnostics(&source);
        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code
                    == Some(crate::diag::codes::NON_CONSTANT_INITIALIZER))
                .count(),
            1,
            "{input}: {diagnostics:?}"
        );
    }
}

#[test]
fn const_context_ir_attribute_bindings_keep_compound_paths() {
    for (expression, expected) in [
        ("array'length", Some(3)),
        ("bus.data'length", Some(8)),
        ("array[2]'length", Some(8)),
        ("bus.data'left", Some(15)),
        ("bus.data'right", Some(8)),
        ("bus.data'high", None),
        ("array[1]'length", None),
        ("array[position]'length", None),
        ("(1 + 2)'length", None),
    ] {
        let mut sink = DiagnosticSink::default();
        let source = format!(
            "module attributes;
             struct Payload {{ pub data: integer[15..8] }}
             fn read(bus: Payload, array: integer[8][3], position: integer) -> integer {{
                 return {expression};
             }}"
        );
        let modules = [crate::syntax::parse_module(FileId(0), &source, &mut sink)];
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        assert!(!sink.has_errors(), "{expression}: {:?}", sink.diagnostics());
        let ast::Item::Fn(function) = &modules[0].items[1] else {
            panic!("function")
        };
        let ast::Stmt::Return {
            value: Some(value), ..
        } = &function.body.as_ref().unwrap().stmts[0]
        else {
            panic!("return")
        };
        let functions = FunctionIndex::new(&resolved);
        let env = HashMap::from([
            ("array::length".into(), 3),
            ("bus.data::length".into(), 8),
            ("array[2]::length".into(), 8),
            ("bus.data::left".into(), 15),
            ("bus.data::right".into(), 8),
        ]);
        assert_eq!(
            eval_const_fns(value, &env, &functions, 0),
            expected,
            "{expression}"
        );
    }
}

#[test]
fn const_context_pure_scalar_locals_do_not_fall_through_to_bound_environment() {
    let mut sink = DiagnosticSink::default();
    let modules = [crate::syntax::parse_module(
        FileId(0),
        "module model;
         fn early() -> integer { return value; let value: integer = 7; }
         fn initialized() -> integer { let value: integer = 8; return value; }
         const EARLY: integer = early();
         const INITIALIZED: integer = initialized();",
        &mut sink,
    )];
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let mut functions = FunctionIndex::new(&resolved);
    for item in &modules[0].items {
        functions.insert_declaration(item);
    }
    let env = HashMap::from([("value".into(), 99)]);
    for (index, expected) in [(2, None), (3, Some(8))] {
        let ast::Item::Const(constant) = &modules[0].items[index] else {
            panic!("constant")
        };
        assert_eq!(
            eval_const_fns(&constant.value, &env, &functions, 0),
            expected
        );
    }
}

#[test]
fn const_context_ir_queries_keep_enum_parameters_and_boolean_encodings() {
    let mut sink = DiagnosticSink::default();
    let sources = [
        "module model; use core::primitive::Decision;
         enum Code { First = 9, Next }
         fn width(value: Code) -> integer { return integer(value); }
         fn enabled() -> Decision { return false; }
         const WIDTH: integer = width(Code::Next);
         const FLAG: Decision = enabled();
         const MODE: Code = Code::Next;
         const COPY: Code = MODE;",
        "module core::primitive;
         pub enum Decision { false = 5, true = 17 }
         attr lang for Decision = \"bool\";",
    ];
    let modules: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(i, source)| crate::syntax::parse_module(FileId(i as u32), source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let mut functions = FunctionIndex::new(&resolved);
    for item in modules.iter().flat_map(|module| &module.items) {
        functions.insert_declaration(item);
    }
    let constants: Vec<_> = modules[0]
        .items
        .iter()
        .filter_map(|item| {
            if let ast::Item::Const(constant) = item {
                Some(constant)
            } else {
                None
            }
        })
        .collect();
    functions.set_enum_values(std::sync::Arc::clone(&typed.enum_values));
    let env = HashMap::new();
    assert_eq!(
        eval_const_fns(&constants[0].value, &env, &functions, 0),
        Some(10)
    );
    let ast::Expr::Call { callee, args, .. } = &constants[0].value else {
        panic!("call")
    };
    assert_eq!(eval_call_fns(callee, args, &env, &functions, 0), Some(10));
    assert_eq!(
        eval_const_fns(&constants[1].value, &env, &functions, 0),
        Some(5)
    );
    assert_eq!(
        eval_condition_fns(&constants[1].value, &env, &functions, 0),
        Some(false)
    );
    let env = HashMap::from([(functions.constant_decl_key(constants[2]), 10)]);
    assert_eq!(
        eval_const_fns(&constants[3].value, &env, &functions, 0),
        Some(10)
    );
}

#[test]
fn standalone_enum_helpers_borrow_supplied_function_contracts() {
    let mut sink = DiagnosticSink::default();
    let modules = [
        crate::syntax::parse_module(
            FileId(0),
            "module provider; pub fn first(n: integer) -> integer { return n + 9; }",
            &mut sink,
        ),
        crate::syntax::parse_module(
            FileId(1),
            "module model; use provider::first; enum Palette { Low = first(0), High }",
            &mut sink,
        ),
    ];
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let ast::Item::Fn(function) = &modules[0].items[0] else {
        panic!("function")
    };
    let mut functions = FunctionIndex::new(&resolved);
    functions.insert_free(function);
    let values = enum_discriminants(&modules[1..], &functions);
    assert_eq!(values["Palette"].get("Low"), Some(&9));
    assert_eq!(values["Palette"].get("High"), Some(&10));
    assert_eq!(
        enum_first_discriminants(&modules[1..], &functions).get("Palette"),
        Some(&9)
    );
}

#[test]
fn standalone_enum_helpers_can_branch_without_the_boolean_provider_ast() {
    let mut sink = DiagnosticSink::default();
    let modules = [
        crate::syntax::parse_module(
            FileId(0),
            "module provider; pub fn first(n: integer) -> integer {
                if n == 0 { return 9; } return 3;
            }",
            &mut sink,
        ),
        crate::syntax::parse_module(
            FileId(1),
            "module model; use provider::first; enum Palette { Low = first(0), High }",
            &mut sink,
        ),
        crate::syntax::parse_module(
            FileId(2),
            include_str!("../../../core/primitive.siox"),
            &mut sink,
        ),
    ];
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let ast::Item::Fn(function) = &modules[0].items[0] else {
        panic!("function")
    };
    let mut functions = FunctionIndex::new(&resolved);
    functions.insert_free(function);
    let values = enum_discriminants(&modules[1..2], &functions);
    assert_eq!(values["Palette"].get("Low"), Some(&9));
    assert_eq!(values["Palette"].get("High"), Some(&10));
}

#[test]
fn enum_discriminant_helpers_keep_their_definition_site_constants() {
    let design = lower_src(
        "module model;
        const OFFSET: integer = 5;
        fn first() -> integer { return OFFSET + 4; }
        enum Palette { Low = first(), High }
        enum Derived(Palette);
        entity Top { low: Palette out, high: Derived out }
        impl Top { low = Palette::Low; high = Derived::High; }",
    );
    for owner in ["Palette", "Derived"] {
        assert_eq!(
            design.enum_syms[owner].get(&9).map(String::as_str),
            Some("Low")
        );
        assert_eq!(
            design.enum_syms[owner].get(&10).map(String::as_str),
            Some("High")
        );
    }
    for name in ["low", "high"] {
        let signal = design
            .signals
            .iter()
            .find(|signal| signal.path == format!("Top.{name}"))
            .unwrap();
        assert_eq!(signal.width, 4, "{name}");
    }
}

#[test]
fn scalar_constants_use_declared_aliases_and_helper_return_types() {
    let design = lower_src(
        "module model;
        enum Palette { 'r' = 3, 'b' = 21 }
        type Pixel = Palette;
        fn paint() -> Pixel { return 'r'; }
        fn letter() -> Char { return 'λ'; }
        const RED: Pixel = paint();
        const BLUE: Pixel = 'b';
        const COPY: Pixel = BLUE;
        const GLYPH: Char = letter();
        entity Top { red: Palette out, blue: Palette out, copy: Palette out, glyph: Char out }
        impl Top { red = RED; blue = BLUE; copy = COPY; glyph = GLYPH; }",
    );
    for (name, expected) in [
        ("red", 3),
        ("blue", 21),
        ("copy", 21),
        ("glyph", 'λ' as u64),
    ] {
        let signal = design
            .signals
            .iter()
            .position(|signal| signal.path == format!("Top.{name}"))
            .unwrap();
        let value = design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(signal as u32))
            .map(|driver| &driver.expr);
        assert!(
            matches!(value, Some(Expr::Const(value)) if *value == expected),
            "{name}: {value:?}"
        );
    }
}

#[test]
fn implementation_constants_publish_the_same_identity_their_reads_select() {
    let (design, sink) = lower_with_nonzero_false(
        "module model;
         const LIMIT: integer = 100;
         fn module_limit() -> integer { return LIMIT; }
         struct Pair { pub a: integer, pub b: integer }
         entity Alpha { value: integer out, outer: integer out, field: integer out }
         impl Alpha {
             const LIMIT: integer = 5;
             const PAIR: Pair = { .a = 11, .b = 13 };
             value = LIMIT; outer = module_limit(); field = PAIR.a;
         }
         entity Beta { value: integer out, outer: integer out, field: integer out }
         impl Beta {
             const LIMIT: integer = 9;
             const PAIR: Pair = { .a = 17, .b = 19 };
             value = LIMIT; outer = module_limit(); field = PAIR.b;
         }
         entity Sized<W: integer> { value: integer out }
         impl<V: integer> Sized<V> {
             const LIMIT: integer = INNER + 1;
             const INNER: integer = V * 3;
             value = LIMIT;
         }
         entity Top { value: integer out }
         impl Top {
             let a: Alpha = {}; let b: Beta = {};
             let small: Sized<W = 2> = {}; let big: Sized<W = 4> = {};
             value = a.value + b.value + small.value + big.value;
         }",
    );
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    for (path, expected) in [
        ("Top.a.value", 5),
        ("Top.a.outer", 100),
        ("Top.a.field", 11),
        ("Top.b.value", 9),
        ("Top.b.outer", 100),
        ("Top.b.field", 19),
        ("Top.small.value", 7),
        ("Top.big.value", 13),
    ] {
        let signal = design
            .signals
            .iter()
            .position(|signal| signal.path == path)
            .unwrap();
        let driver = design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(signal as u32))
            .unwrap();
        let value = match &driver.expr {
            Expr::Canonical { value, .. } => {
                crate::ir::derive::materialize_digital_expression(&design.process_ir, *value)
                    .unwrap()
            }
            value => value.clone(),
        };
        assert!(
            matches!(value, Expr::Const(word) if word == expected),
            "{path}: {value:?}"
        );
    }
}

fn encoding(source: &str, owner: &str) -> LogicEncoding {
    encoding_with(
        source,
        owner,
        include_str!("../../../core/primitive.siox"),
        CLK_PRELUDE,
    )
}

fn encoding_with(source: &str, owner: &str, primitive: &str, prelude: &str) -> LogicEncoding {
    let source = format!("{source}\n{prelude}");
    let mut sink = DiagnosticSink::new();
    let module = crate::syntax::parse_module(FileId(0), &source, &mut sink);
    let core = crate::syntax::parse_module(FileId(1), primitive, &mut sink);
    let modules = [module, core];
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
    assert!(std::sync::Arc::ptr_eq(
        &typed.enum_values,
        &hierarchy.enum_values
    ));
    let design = lower(&modules, &resolved, &hierarchy, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    design
        .logic_encodings
        .get(owner)
        .unwrap_or_else(|| panic!("missing source encoding for {owner}"))
        .clone()
}

#[test]
fn const_context_encoding_tables_follow_source_order_instead_of_discriminants() {
    let source = "module model;
        enum Palette { Low = 3, Mid = 6, High = 9 }
        impl Ord<Palette> for Palette {
            fn lt(self, rhs: Palette) -> Bool { return integer(self) > integer(rhs); }
            fn le(self, rhs: Palette) -> Bool { return integer(self) >= integer(rhs); }
        }
        impl LogicEncoding for Palette {
            fn to_bool(self) -> Bool { return self < Palette::Mid; }
            fn is_binary(self) -> Bool { return self != Palette::Mid; }
            fn is_high_impedance(self) -> Bool { return false; }
            fn to_x01(self) -> Palette {
                if self < Palette::Mid { return Palette::High; }
                return Palette::Low;
            }
        }
        entity Top { value: Palette out } impl Top { value = Palette::Low; }";
    let encoding = encoding(source, "Palette");
    assert_eq!(encoding.value_bits.get(&3), Some(&false));
    assert_eq!(encoding.value_bits.get(&9), Some(&true));
    assert_eq!(encoding.x01.get(&3), Some(&3));
    assert_eq!(encoding.x01.get(&9), Some(&9));
}

#[test]
fn enum_tables_evaluate_resolved_constants_and_helper_calls() {
    for value in ["HIGH", "high()"] {
        let source = format!(
            "module model;
            enum Palette {{ Low = 3, High = 9 }}
            const HIGH: Palette = Palette::High;
            fn high() -> Palette {{ return HIGH; }}
            impl LogicEncoding for Palette {{
                fn to_bool(self) -> Bool {{ return self == {value}; }}
                fn is_binary(self) -> Bool {{ return true; }}
                fn is_high_impedance(self) -> Bool {{ return false; }}
                fn to_x01(self) -> Palette {{ return self; }}
            }}
            entity Top {{ value: Palette out }} impl Top {{ value = Palette::Low; }}"
        );
        let encoding = encoding(&source, "Palette");
        assert_eq!(encoding.value_bits.get(&3), Some(&false));
        assert_eq!(encoding.value_bits.get(&9), Some(&true));
        assert_eq!(encoding.x01.get(&3), Some(&3));
        assert_eq!(encoding.x01.get(&9), Some(&9));
    }
}

#[test]
fn boolean_spelling_does_not_erase_an_unrelated_enum_identity() {
    let source = "module model;
        enum Palette { false = 3, true = 9 }
        impl LogicEncoding for Palette {
            fn to_bool(self) -> Bool { return self == Palette::true; }
            fn is_binary(self) -> Bool { return true; }
            fn is_high_impedance(self) -> Bool { return false; }
            fn to_x01(self) -> Palette { return self; }
        }
        entity Top { value: Palette out } impl Top { value = Palette::false; }";
    let encoding = encoding(source, "Palette");
    assert_eq!(encoding.value_bits.get(&3), Some(&false));
    assert_eq!(encoding.value_bits.get(&9), Some(&true));
}

#[test]
fn source_boolean_discriminants_and_operator_bodies_are_not_integer_flags() {
    let source = "module model;
        enum Palette { Low = 3, High = 9 }
        impl LogicEncoding for Palette {
            fn to_bool(self) -> Bool { return self == Palette::High or false; }
            fn is_binary(self) -> Bool { return not false; }
            fn is_high_impedance(self) -> Bool { return not true; }
            fn to_x01(self) -> Palette { if self == Palette::Low { return Palette::Low; } return Palette::High; }
        }
        entity Top { value: Palette out } impl Top { value = Palette::Low; }";
    let primitive = "module core::primitive;
        pub enum Bool { false = 5, true = 17 } attr lang for Bool = \"bool\";";
    let encoding = encoding_with(source, "Palette", primitive, CLK_PRELUDE);
    assert_eq!(encoding.value_bits.get(&3), Some(&false));
    assert_eq!(encoding.value_bits.get(&9), Some(&true));
    assert_eq!(encoding.binary.len(), 2);
    assert!(encoding.high_impedance.is_empty());
}

#[test]
fn boolean_operator_evaluation_executes_its_source_body() {
    let source = "module model;
        enum Palette { Low = 3, High = 9 }
        impl LogicEncoding for Palette {
            fn to_bool(self) -> Bool { return self == Palette::Low or self == Palette::High; }
            fn is_binary(self) -> Bool { return true; }
            fn is_high_impedance(self) -> Bool { return false; }
            fn to_x01(self) -> Palette { return self; }
        }
        entity Top { value: Palette out } impl Top { value = Palette::Low; }";
    let prelude = CLK_PRELUDE.replace(
        "fn or(self, rhs: Bool) -> Bool { if self { return true; } return rhs; }",
        "fn or(self, rhs: Bool) -> Bool { return rhs; }",
    );
    assert_ne!(prelude, CLK_PRELUDE);
    let encoding = encoding_with(
        source,
        "Palette",
        include_str!("../../../core/primitive.siox"),
        &prelude,
    );
    assert_eq!(encoding.value_bits.get(&3), Some(&false));
    assert_eq!(encoding.value_bits.get(&9), Some(&true));
}

#[test]
fn enum_helpers_and_boolean_conversion_use_their_own_call_frames() {
    let source = "module model;
        enum Palette { Low = 3, High = 9 }
        type Pixel = Palette;
        const LOW: Pixel = Palette::Low;
        fn choose(value: Palette) -> Palette { return value; }
        impl Palette { fn canonical(self, value: Palette) -> Palette { if self == LOW { return LOW; } return choose(value); } }
        impl Boolean for Palette { fn as_bool(self) -> Bool { return self != LOW; } }
        impl LogicEncoding for Palette {
            fn to_bool(self) -> Bool { if self { return true; } return false; }
            fn is_binary(self) -> Bool { return true; }
            fn is_high_impedance(self) -> Bool { return false; }
            fn to_x01(self) -> Pixel { return self.canonical(Palette::High); }
        }
        entity Top { value: Palette out } impl Top { value = Palette::Low; }";
    let encoding = encoding(source, "Palette");
    assert_eq!(encoding.value_bits.get(&3), Some(&false));
    assert_eq!(encoding.value_bits.get(&9), Some(&true));
    assert_eq!(encoding.x01.get(&3), Some(&3));
    assert_eq!(encoding.x01.get(&9), Some(&9));
}

#[test]
fn a_qualified_variant_cannot_be_captured_by_a_same_named_parameter() {
    let source = "module model;
        enum Palette { Low = 3, High = 9 }
        impl Boolean for Palette { fn as_bool(self) -> Bool { return self == Palette::High; } }
        impl And<Palette, Palette> for Palette { fn and(self, High: Palette) -> Palette { return Palette::High; } }
        impl LogicEncoding for Palette {
            fn to_bool(self) -> Bool { return self == Palette::High; }
            fn is_binary(self) -> Bool { return true; }
            fn is_high_impedance(self) -> Bool { return false; }
            fn to_x01(self) -> Palette { return self; }
        }
        entity Top { value: Palette out } impl Top { value = Palette::Low; }";
    let encoding = encoding(source, "Palette");
    for left in [3, 9] {
        for right in [3, 9] {
            assert_eq!(encoding.binary_ops["and"].get(&(left, right)), Some(&9));
        }
    }
}

#[test]
fn inherited_variants_and_alias_contracts_keep_the_selected_enum_owner() {
    let source = "module model;
        enum Base { Low = 3, High = 9 }
        enum Palette(Base);
        type Pixel = Palette;
        const HIGH: Pixel = Palette::High;
        fn high() -> Pixel { return HIGH; }
        impl LogicEncoding for Palette {
            fn to_bool(self) -> Bool { return self == high(); }
            fn is_binary(self) -> Bool { return true; }
            fn is_high_impedance(self) -> Bool { return false; }
            fn to_x01(self) -> Palette { return self; }
        }
        entity Top { value: Palette out } impl Top { value = Palette::Low; }";
    let encoding = encoding(source, "Palette");
    assert_eq!(encoding.value_bits.get(&3), Some(&false));
    assert_eq!(encoding.value_bits.get(&9), Some(&true));
    assert_eq!(encoding.x01.get(&3), Some(&3));
    assert_eq!(encoding.x01.get(&9), Some(&9));
}
