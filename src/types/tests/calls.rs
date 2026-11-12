//! Calls, returns, arity, and runtime-function contracts.

use super::*;

#[test]
fn generic_type_bindings_select_parameter_identity_not_its_spelling() {
    for reversed in [false, true] {
        let mut sink = DiagnosticSink::default();
        let mut modules: Vec<_> = [
            "module left;
             pub fn select<T>(value: T) -> T { return value; }
             pub fn element<T>(value: (T, T[2])) -> T { return value.0; }",
            "module right;
             pub fn select<T>(value: T) -> T { return value; }
             pub struct Holder<T> { pub value: T }",
        ]
        .iter()
        .enumerate()
        .map(|(index, source)| crate::syntax::parse_module(FileId(index as u32), source, &mut sink))
        .collect();
        if reversed {
            modules.reverse();
        }
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        let functions: Vec<_> = modules
            .iter()
            .flat_map(|module| &module.items)
            .filter_map(|item| match item {
                Item::Fn(function) if function.name.text == "select" => Some(function),
                _ => None,
            })
            .collect();
        assert_eq!(functions.len(), 2);
        let parameter = |function: &crate::syntax::ast::FnDecl| {
            resolved
                .declared(function.generics.params[0].name.span)
                .unwrap()
        };
        let first = parameter(functions[0]);
        let second = parameter(functions[1]);
        assert_ne!(first, second);
        let first_type = functions[0].params[0].ty.as_ref().unwrap();
        let second_type = functions[1].params[0].ty.as_ref().unwrap();
        let checker = Checker::new(&mut sink, &resolved, &modules);
        let plain_root = checker.push_generic_scope(&Params::default());
        assert!(!plain_root);
        assert!(!checker.in_generic_scope.get());
        checker.in_generic_scope.set(plain_root);
        let outer_scope = checker.push_generic_scope(&functions[0].generics);
        assert!(!outer_scope);
        assert!(checker.in_generic_scope.get());
        let plain_scope = checker.push_generic_scope(&Params::default());
        assert!(plain_scope);
        assert!(checker.in_generic_scope.get());
        checker.in_generic_scope.set(plain_scope);
        let shadow_scope = checker.push_generic_scope(&functions[1].generics);
        assert!(shadow_scope);
        checker.in_generic_scope.set(shadow_scope);
        checker.in_generic_scope.set(outer_scope);
        assert!(!checker.in_generic_scope.get());
        checker
            .type_bindings
            .borrow_mut()
            .insert(first, Ty::Integer);
        assert_eq!(checker.ast_ty(first_type), Ty::Integer);
        assert_eq!(checker.ast_ty(second_type), Ty::Error);
        assert_eq!(
            checker.bind_type_param(first_type, &Ty::Natural, first),
            Some(Ty::Natural)
        );
        assert_eq!(
            checker.bind_type_param(first_type, &Ty::Natural, second),
            None
        );
        let mut unresolved = first_type.clone();
        let Type::Path(path) = &mut unresolved else {
            panic!("fixture parameter must be a path");
        };
        path.span = Span::new(FileId(99), 0..1);
        assert_eq!(
            checker.bind_type_param(&unresolved, &Ty::Natural, first),
            None
        );
        assert_eq!(checker.ast_ty(&unresolved), Ty::Error);
        checker.type_bindings.borrow_mut().insert(second, Ty::Real);
        assert_eq!(checker.ast_ty(first_type), Ty::Integer);
        assert_eq!(checker.ast_ty(second_type), Ty::Real);

        for item in modules.iter().flat_map(|module| &module.items) {
            match item {
                Item::Fn(function) if function.name.text == "element" => {
                    let ty = function.params[0].ty.as_ref().unwrap();
                    let argument = Ty::Tuple(vec![
                        Ty::Natural,
                        Ty::Array {
                            elem: Box::new(Ty::Natural),
                            family: None,
                            len: 2,
                            low: 0,
                        },
                    ]);
                    assert_eq!(
                        checker.bind_type_param(ty, &argument, parameter(function)),
                        Some(Ty::Natural)
                    );
                    assert_eq!(checker.bind_type_param(ty, &argument, first), None);
                    let Type::Tuple { elems, .. } = ty else {
                        panic!("fixture parameter must be a tuple");
                    };
                    let Ty::Tuple(arguments) = argument else {
                        unreachable!();
                    };
                    assert_eq!(
                        checker.bind_type_param(&elems[1], &arguments[1], parameter(function)),
                        Some(Ty::Natural)
                    );
                    assert_eq!(
                        checker.bind_type_param(&elems[1], &arguments[1], second),
                        None
                    );
                }
                Item::Struct(structure) => {
                    assert_eq!(checker.ast_ty(&structure.fields[0].ty), Ty::Error);
                }
                _ => {}
            }
        }
    }
}

#[test]
fn generic_static_rejection_does_not_leak_into_plain_implementations() {
    let generic = "struct Generic<T> { value: T }
        impl<T> Generic<T> {
            fn state() { static let generic_state: integer = 0; }
        }";
    let concrete = "struct Concrete { value: integer }
        impl Concrete {
            fn state() { static let concrete_state: integer = 0; }
        }";
    for declarations in [[generic, concrete], [concrete, generic]] {
        let source = format!("module scope; {} {}", declarations[0], declarations[1]);
        let sink = check_modules(&[(&source, FileId(0))]);
        let errors: Vec<_> = sink
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "{:?}", sink.diagnostics());
        assert_eq!(errors[0].code, Some(codes::UNSUPPORTED_EXPR));
        assert_eq!(
            errors[0].message,
            "`static let` in a generic function is not supported yet"
        );
        let span = errors[0].primary.unwrap();
        assert!(source[span.start as usize..span.end as usize].contains("generic_state"));
    }
}

#[test]
fn missing_generic_binder_identity_disables_caching_not_other_body_checks() {
    let mut sink = DiagnosticSink::default();
    let mut modules = vec![crate::syntax::parse_module(
        FileId(0),
        "module caller;
         fn inspect<T, U>(left: T, right: U) -> integer { return right.no_such_method(); }
         fn check(a: integer, b: real) { inspect(a, b); inspect(a, b); }",
        &mut sink,
    )];
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let Item::Fn(function) = &mut modules[0].items[0] else {
        panic!("fixture must begin with the generic function");
    };
    function.generics.params[0].name.span = Span::new(FileId(99), 0..1);
    let mut checker = Checker::new(&mut sink, &resolved, &modules);
    checker.collect(&modules);
    checker.check_item(&modules[0].items[1]);
    assert!(checker.instantiated.borrow().is_empty());
    assert!(checker.type_bindings.borrow().is_empty());
    drop(checker);
    let errors: Vec<_> = sink
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
        .collect();
    assert_eq!(errors.len(), 2, "{:?}", sink.diagnostics());
    assert!(errors.iter().all(|diagnostic| diagnostic
        .message
        .contains("has no method `no_such_method`")));
}

#[test]
fn nested_generic_calls_keep_caller_types_and_recursive_binders_isolated() {
    let mut sources = [
        (
            "module left; pub fn select<T>(value: T) -> T { return value; }",
            FileId(0),
        ),
        (
            "module right; pub fn select<T>(value: T) -> T { return value; }",
            FileId(1),
        ),
        (
            "module caller;
          fn nested<T>(value: T) -> T { return left::select(right::select(value)); }
          fn recursive<T>(value: T) { recursive((value, value)); }
          fn check(a: integer, b: real) {
              let integer_value: integer = nested(a);
              let real_value: real = nested(b);
              recursive(a);
              recursive(b);
              let wrong: integer = nested(b);
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
            errors[0].message.contains("integer") && errors[0].message.contains("real"),
            "{:?}",
            errors[0]
        );
        sources.reverse();
    }
}

#[test]
fn borrowed_generic_signatures_keep_inferred_returns_scoped_to_the_selected_function() {
    let mut sources = [
        (
            "module left;
          pub fn select<T>(value: T) -> T { return value; }
          pub fn pair<T>(value: T) -> (T, T) { return (value, value); }",
            FileId(0),
        ),
        (
            "module right;
          pub fn select<T>(value: T) -> T[2] { return [value, value]; }",
            FileId(1),
        ),
        (
            "module caller;
          fn check(a: integer, b: real) {
              let scalar: integer = left::select(a);
              let pair: (integer, integer) = left::pair(a);
              let array: real[2] = right::select(b);
              let wrong: integer = right::select(b)[0];
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
            errors[0].message.contains("integer") && errors[0].message.contains("real"),
            "{:?}",
            errors[0]
        );
        sources.reverse();
    }
}

#[test]
fn generic_instantiation_cache_keeps_nominal_arguments_distinct() {
    for arrays in [false, true] {
        let declarations = if arrays {
            "pub enum Cell { Low, High } pub struct Token(Cell[]);"
        } else {
            "pub struct Token {}"
        };
        let left = format!(
            "module left; {declarations}
            impl Token {{ pub fn value() -> integer {{ return 1; }} }}"
        );
        let right = format!("module right; {declarations}");
        let shape = if arrays { "[2]" } else { "" };
        for bad_first in [false, true] {
            let calls = if bad_first {
                "inspect(bad); inspect(good); inspect(again);"
            } else {
                "inspect(good); inspect(bad); inspect(again);"
            };
            let caller = format!(
                "module caller;
                use Good = left::Token; use Bad = right::Token;
                type Again = Bad;
                fn inspect<T>(value: T) -> integer {{ return value.value(); }}
                fn check(good: Good{shape}, bad: Bad{shape}, again: Again{shape}) {{ {calls} }}"
            );
            for reversed in [false, true] {
                let mut sources = [
                    (caller.as_str(), FileId(0)),
                    (left.as_str(), FileId(1)),
                    (right.as_str(), FileId(2)),
                ];
                if reversed {
                    sources.reverse();
                }
                let sink = check_modules(&sources);
                let errors: Vec<_> = sink
                    .diagnostics()
                    .iter()
                    .filter(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
                    .collect();
                assert_eq!(
                    errors.len(),
                    1,
                    "arrays={arrays} bad_first={bad_first} reversed={reversed}: {:?}",
                    sink.diagnostics()
                );
                assert!(
                    errors[0].message.contains("has no method `value`"),
                    "{:?}",
                    errors[0]
                );
            }
        }
    }
}

#[test]
fn aliased_enum_conversions_check_the_selected_domain() {
    let provider = "module model;
        pub enum Base { Low = 3, High = 9 }
        pub enum Derived(Base);
        pub type Alias = Derived;";
    let unrelated = "module other; pub enum Base { Low = 3, High = 9 }";
    let caller = "module caller;
        use Local = model::Alias;
        use Original = model::Base;
        use Remote = other::Base;
        fn up(value: Local) -> Original { return Original(value); }
        fn down(value: Original) -> Local { return Local(value); }
        fn invalid(value: Remote) -> Local { return Local(value); }";
    for sources in [
        [
            (provider, FileId(0)),
            (unrelated, FileId(1)),
            (caller, FileId(2)),
        ],
        [
            (caller, FileId(2)),
            (unrelated, FileId(1)),
            (provider, FileId(0)),
        ],
    ] {
        let sink = check_modules(&sources);
        let errors: Vec<_> = sink
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "{:?}", sink.diagnostics());
        assert_eq!(errors[0].code, Some(codes::TYPE_MISMATCH));
        assert!(
            errors[0].message.contains("no conversion from"),
            "{errors:?}"
        );
    }
}

#[test]
fn default_enum_new_does_not_override_a_declared_constructor() {
    let sink = check_modules(&[(
        "module m;
        enum DefaultMode { Low = 9, High = 17 }
        enum CustomMode { Low = 3, High = 4 }
        impl CustomMode {
            pub static fn new(value: integer) -> CustomMode { return CustomMode::High; }
            pub static fn forwarded(value: integer) -> Self { return Self::new(value); }
        }
        type Alias = CustomMode;
        fn default_value() -> DefaultMode { return DefaultMode::new(); }
        fn custom_value() -> CustomMode { return CustomMode::new(42); }
        fn alias_value() -> Alias { return Alias::new(42); }
        fn forwarded_value() -> Alias { return Alias::forwarded(42); }",
        FileId(0),
    )]);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    assert_eq!(
        check_src(
            "module m;
        enum Mode { Low = 9, High = 17 }
        fn invalid() -> Mode { return Mode::new(42); }"
        ),
        1
    );
}

/// A call to a function nothing declares passed every stage and failed in
/// the backend as "unsupported call `abs` in testbench expression",
/// blaming the emitter for a missing `use`.
#[test]
fn an_undeclared_call_is_reported() {
    let errors = check_src(
        "module m;\n\
             entity E { a: unsigned[8] in, y: unsigned[8] out }\n\
             impl E { y = nosuchfn(a); }\n",
    );
    assert_eq!(errors, 1, "the unknown function is reported");
}

/// The categories that legitimately have no `fn` declaration: a declared
/// function, a type used as a conversion, a runtime-provided std function,
/// a compiler primitive, and the width builtin. The corpus caught
/// `resize` and `finish` missing from this list.
#[test]
fn calls_without_a_declaration_are_not_all_mistakes() {
    let errors = check_src(
        "module m;\n\
             fn twice(x: unsigned[8]) -> unsigned[8] { return x + x; }\n\
             entity E { a: unsigned[8] in, y: unsigned[8] out }\n\
             impl E { y = twice(unsigned[8](resize(a, 8))); }\n\
             #[test] entity T {}\n\
             impl T {\n\
               let v: integer = randint(0, 3);\n\
               print!(\"{}\", v);\n\
               finish();\n\
             }\n",
    );
    assert_eq!(errors, 0, "none of these is an unknown function");
}

#[test]
/// A returned value must match the declared result type.
fn return_values_match_the_function_signature() {
    let errors = check_src(
        "module m;\n\
             fn missing() -> unsigned[8] { return; }\n\
             fn unexpected() { return 1; }\n",
    );
    assert_eq!(
        errors, 2,
        "bare and valued returns must agree with the declared signature"
    );
}

#[test]
/// Call arguments are checked against the declared parameter types.
fn local_function_arguments_use_their_declared_types() {
    let errors = check_src(
        "module m;\n\
             fn take_byte(value: unsigned[8]) {}\n\
             fn take_real(value: real) {}\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let r: real = 1.5;\n\
               let i: integer = 2;\n\
               take_byte(r);\n\
               take_byte(i + r);\n\
               take_real(i);\n\
               y = '0';\n\
             }\n",
    );
    assert_eq!(
        errors, 2,
        "local and mixed-real arguments must use their actual promoted types"
    );
}

#[test]
/// Method calls check both argument count and argument types.
fn method_calls_check_argument_count_and_types() {
    let errors = check_src(
        "module m;\n\
             struct Device { pub value: unsigned[8] }\n\
             impl Device { pub fn take(value: unsigned[8]) {} }\n\
             struct DefaultDevice { pub value: unsigned[8] }\n\
             trait Takes {\n\
               fn take(value: unsigned[8]) {\n\
                 let copy: unsigned[8] = value;\n\
               }\n\
             }\n\
             impl Takes for DefaultDevice {}\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let device: Device = { .value = 0 };\n\
               let default_device: DefaultDevice = { .value = 0 };\n\
               let r: real = 1.5;\n\
               device.take(r);\n\
               device.take();\n\
               default_device.take(r);\n\
               default_device.take();\n\
               y = '0';\n\
             }\n",
    );
    assert_eq!(
        errors, 4,
        "inherent and inherited-default methods enforce parameter type and arity"
    );
}

#[test]
/// Associated and instance call forms are distinct: neither substitutes for
/// the other.
fn associated_and_instance_method_call_forms_are_distinct() {
    let errors = check_src(
        "module m;\n\
             struct Thing { pub value: Bit }\n\
             struct Other { pub value: Bit }\n\
             trait Factory {\n\
               static fn make(value: integer) -> integer { return value; }\n\
             }\n\
             impl Factory for Other {}\n\
             impl Thing {\n\
               pub static fn static_value(value: integer) -> integer { return value; }\n\
               pub static fn logic_value() -> Logic { return 'X'; }\n\
               pub fn instance_value() -> integer { return 1; }\n\
             }\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let thing: Thing = { .value = '0' };\n\
               let r: real = 1.5;\n\
               Thing::static_value(r);\n\
               Thing::static_value();\n\
               Thing::instance_value();\n\
               thing.static_value(1);\n\
               Other::make(r);\n\
               if Thing::logic_value() { y = '1'; } else { y = '0'; }\n\
             }\n",
    );
    assert_eq!(
        errors, 6,
        "associated calls enforce signatures, receiver form, and return typing"
    );
}

#[test]
/// A function with a declared result must return on every path, including
/// through matches and numeric domains.
fn value_returning_functions_return_on_every_path() {
    let missing = check_src(
        "module m;\n\
             fn empty() -> unsigned[8] {}\n\
             fn partial(flag: Bool) -> unsigned[8] {\n\
               if flag { return 1; }\n\
             }\n\
             fn numeric_gap(value: unsigned[2]) -> unsigned[8] {\n\
               match value { 0..2 => { return 1; } }\n\
             }\n\
             fn generic_gap<T>(value: T) -> T {\n\
               let copy: T = value;\n\
             }\n",
    );
    assert_eq!(
        missing, 4,
        "empty, one-sided, non-exhaustive numeric, and generic bodies fall through"
    );

    let complete = check_src(
        "module m;\n\
             enum State { A, B, C }\n\
             fn branch(flag: Bool) -> unsigned[8] {\n\
               if flag { return 1; } else { return 2; }\n\
             }\n\
             fn choose(state: State) -> unsigned[8] {\n\
               match state {\n\
                 State::A => { return 1; }\n\
                 State::B => { return 2; }\n\
                 State::C => { return 3; }\n\
               }\n\
             }\n\
             fn numeric(value: unsigned[2]) -> unsigned[8] {\n\
               match value { 0..3 => { return 4; } }\n\
             }\n",
    );
    assert_eq!(
        complete, 0,
        "two-sided branches and exhaustive matches return on every path"
    );
}

#[test]
/// The same rule applies to methods: none may fall through.
fn value_returning_methods_cannot_fall_through() {
    let errors = check_src(
        "module m;\n\
             struct S { value: unsigned[8] }\n\
             impl S { fn bad() -> unsigned[8] {} }\n\
             trait Defaulted {\n\
               fn partial(flag: Bool) -> unsigned[8] {\n\
                 if flag { return 1; }\n\
               }\n\
             }\n",
    );
    assert_eq!(
        errors, 2,
        "implementation and non-empty trait-default methods are inlined expressions"
    );
}

#[test]
/// A method's declared return type propagates to its call site.
fn method_return_type_propagates() {
    // A method returning `Logic` used directly as a condition must error
    // (Logic isn't Boolean), proving the return type flows into checks.
    let bad = "module m;\n\
            struct S { v: Logic, }\n\
            impl S { pub fn ready() -> Logic { return self.v; } }\n\
            entity E { o: Logic out }\n\
            impl E { let s: S; if s.ready() { o = '1'; } }\n";
    assert_eq!(
        check_src(bad),
        1,
        "Logic-returning method as a condition should error"
    );

    // A `Bool`-returning method is a valid condition — no error.
    let good = "module m;\n\
            struct S { v: Logic, }\n\
            impl S { pub fn ready() -> Bool { return true; } }\n\
            entity E { o: Logic out }\n\
            impl E { let s: S; if s.ready() { o = '1'; } }\n";
    assert_eq!(
        check_src(good),
        0,
        "Bool-returning method as a condition should pass"
    );
}

/// Nothing checked call arity: a short call to a module `fn` left a
/// parameter unbound, and a wrong-arity `extern "C"` call handed garbage
/// to real native code.
#[test]
fn call_arity_must_match_the_declaration() {
    let base = "module m;\nfn add2(a: integer, b: integer) -> integer { return a + b; }\n\
                    extern \"C\" { fn ext(a: integer, b: integer) -> integer; }\n\
                    entity E { a: unsigned[8] in, y: unsigned[8] out }\nimpl E { y = ";
    assert_eq!(check_src(&format!("{base}add2(a); }}\n")), 1, "too few");
    assert_eq!(
        check_src(&format!("{base}add2(a, a, a); }}\n")),
        1,
        "too many"
    );
    assert_eq!(check_src(&format!("{base}add2(a, a); }}\n")), 0, "exact");
    assert_eq!(
        check_src(&format!("{base}ext(a); }}\n")),
        1,
        "extern too few"
    );
    assert_eq!(
        check_src(&format!("{base}ext(a, a); }}\n")),
        0,
        "extern exact"
    );
    // A conversion is a call shape but not a declared fn.
    assert_eq!(
        check_src(&format!("{base}unsigned[8](a); }}\n")),
        0,
        "conversion"
    );
}

#[test]
/// A type constructor takes at most one argument.
fn type_constructors_accept_at_most_one_argument() {
    let fixture = |call: &str| {
        format!(
            "module m;\nenum Phase {{ Idle, Run }}\n\
                 entity Child {{ y: Bit out }}\nimpl Child {{ y = '0'; }}\n\
                 #[test] entity T {{}}\nimpl T {{ {call}; }}\n"
        )
    };
    assert_eq!(check_src(&fixture("integer(1, 2)")), 1);
    assert_eq!(check_src(&fixture("unsigned[8](1, 2)")), 1);
    assert_eq!(check_src(&fixture("Phase(1, 2)")), 1);
    assert_eq!(check_src(&fixture("Phase()")), 0, "explicit default");
    assert_eq!(
        check_src(&fixture("Phase(Phase::Idle)")),
        0,
        "one conversion input"
    );
    assert_eq!(
        check_src(&fixture("Phase::new(1)")),
        1,
        "associated default construction is nullary"
    );
    assert_eq!(
        check_src(&fixture("Child()")),
        1,
        "an entity is instantiated with a struct literal, not called as a value"
    );
}

#[test]
/// `extern "C"` call arguments must match the declaration.
fn extern_call_arguments_must_match_the_declaration() {
    let src = "module m;\n\
                   extern \"C\" { fn take_int(value: integer) -> integer; }\n\
                   entity E { y: Bit out }\n\
                   impl E { let value: real = 1.5; take_int(value); y = '0'; }\n";
    assert_eq!(
        check_src(src),
        1,
        "an extern call must not reinterpret a real argument as an integer"
    );
}

#[test]
/// `extern "C"` signatures are limited to the scalar ABI actually
/// implemented.
fn extern_c_signatures_are_limited_to_the_implemented_scalar_abi() {
    assert_eq!(
        check_src(
            "module m;\n\
                 extern \"C\" {\n\
                   fn mixed(x: real, y: integer, bits: unsigned[64]) -> integer;\n\
                 }\n"
        ),
        0,
        "real, integer, and one-word packed values are supported"
    );
    assert_eq!(
        check_src("module m;\nextern \"C\" { fn wide(x: unsigned[65]) -> integer; }\n"),
        0,
        "a packed argument wider than the C ABI word crosses as words by pointer"
    );
    assert_eq!(
        check_src("module m;\nextern \"C\" { fn aggregate(x: unsigned[8][2]) -> integer; }\n"),
        0,
        "an array of C scalars crosses by pointer"
    );
    assert_eq!(
        check_src(
            "module m;\nstruct Pair { a: integer, b: integer }\n\
                 extern \"C\" { fn record() -> Pair; }\n"
        ),
        0,
        "a struct return follows the platform's struct-return rules"
    );
    assert_eq!(
        check_src("module m;\nextern \"C\" { fn side_effect(x: integer); }\n"),
        0,
        "a C `void` function is declared like any other; its calls are checked"
    );
}

#[test]
/// Generic call arguments obey both concrete and repeated type parameters.
fn generic_call_arguments_obey_concrete_and_repeated_types() {
    let src = "module m;\n\
                   fn select<T>(tag: integer, first: T, second: T) -> T { return first; }\n\
                   entity E { y: Bit out }\n\
                   impl E {\n\
                     let i: integer = 1;\n\
                     let r: real = 1.5;\n\
                     select(r, i, i);\n\
                     select(i, i, r);\n\
                     y = '0';\n\
                   }\n";
    assert_eq!(
        check_src(src),
        2,
        "generic calls keep concrete parameters and one consistent inferred T"
    );
}

#[test]
/// A free function's return type propagates to its call site.
fn free_function_return_types_propagate_to_the_call_site() {
    let src = "module m;\n\
                   fn logic_value() -> Logic { return 'X'; }\n\
                   fn real_value() -> real { return 1.5; }\n\
                   entity E { y: Logic out }\n\
                   impl E {\n\
                     let i: integer = real_value();\n\
                     if logic_value() { y = '1'; } else { y = '0'; }\n\
                   }\n";
    assert_eq!(
        check_src(src),
        2,
        "a call has its declaration's return type in every surrounding check"
    );
}

#[test]
/// A function with no return type cannot be used as a value.
fn procedures_cannot_be_used_as_values() {
    let src = "module m;\n\
                   fn procedure() {}\n\
                   fn consume(value: integer) {}\n\
                   struct Device { pub value: Bit }\n\
                   impl Device { pub fn procedure() {} }\n\
                   entity E { y: Bit out }\n\
                   impl E {\n\
                     let device: Device = { .value = '0' };\n\
                     let a: integer = procedure();\n\
                     let b: integer = device.procedure();\n\
                     consume(procedure());\n\
                     let same: Bool = procedure() == procedure();\n\
                     if procedure() { y = '1'; } else { y = '0'; }\n\
                   }\n";
    assert_eq!(
        check_src(src),
        5,
        "a procedure call is valid as a statement, never as a value"
    );
    assert_eq!(
        check_src(
            "module m;\nfn procedure() {}\nentity E { y: Bit out }\n\
                 impl E { procedure(); y = '0'; }\n"
        ),
        0,
        "a procedure call remains valid in statement position"
    );
}

#[test]
/// Runtime intrinsics check their arity.
fn runtime_function_arity_is_checked() {
    let tb = |expression: &str| {
        format!(
            "module m;\n#[test] entity T {{}}\n\
                 impl T {{ {expression}; }}\n"
        )
    };
    assert_eq!(check_src(&tb("rand(1)")), 1, "rand is nullary");
    assert_eq!(check_src(&tb("uniform(1)")), 1, "uniform is nullary");
    assert_eq!(check_src(&tb("randint(1)")), 1, "randint needs two bounds");
    assert_eq!(check_src(&tb("randint(1, 2, 3)")), 1, "too many bounds");
    assert_eq!(check_src(&tb("rand()")), 0);
    assert_eq!(check_src(&tb("randint(1, 2)")), 0);
}

#[test]
/// Runtime intrinsics check argument types and results, and reject forms
/// that have been removed.
fn runtime_functions_enforce_types_results_and_removed_forms() {
    let tb = |body: &str| format!("module m;\n#[test] entity T {{}}\nimpl T {{ {body} }}\n");
    assert_eq!(
        check_src(&tb("let value: integer = uniform();")),
        1,
        "uniform returns real"
    );
    assert_eq!(
        check_src(&tb("let value: integer = std::rand::uniform();")),
        1,
        "qualification keeps a runtime primitive's return contract"
    );
    assert_eq!(
        check_src(&tb("let value: integer = seed(1);")),
        1,
        "seed is a procedure"
    );
    assert_eq!(
        check_src(&tb("randint(1.5, 2);")),
        1,
        "randint needs integer bounds"
    );
    assert_eq!(check_src(&tb("seed(1.5);")), 1, "seed needs an integer");
    assert_eq!(
        check_src(&tb("read<integer>(7);")),
        1,
        "file primitives need literal paths"
    );
    assert_eq!(
        check_src(&tb(
            "let value: unsigned[16] = read<unsigned[16]>(\"word.bin\");"
        )),
        0,
        "a numeric read constructs its requested scalar type"
    );
    assert_eq!(
        check_src(&tb(
            "let value: unsigned[8] = read<unsigned[16]>(\"word.bin\");"
        )),
        1,
        "the destination must contain the requested constructed type"
    );
    assert_eq!(
        check_src(&tb("let value: integer = read_to_string(\"word.bin\");")),
        1,
        "the old split text-read primitive is removed"
    );
    assert_eq!(check_src(&tb("finish(1);")), 1, "finish is nullary");
    assert_eq!(
        check_src(&tb("clock('0', 1ns);")),
        1,
        "removed clock sugar is rejected before code generation"
    );
    assert_eq!(check_src(&tb("assert!();")), 1, "assert needs a condition");
    assert_eq!(check_src(&tb("print!();")), 1, "print needs a format");
    assert_eq!(
        check_src(&tb("print!(123);")),
        1,
        "the format must be a string literal"
    );
    assert_eq!(
        check_src(&tb("assert!(1);")),
        1,
        "assert needs a Boolean condition"
    );
    assert_eq!(
        check_src(&tb("let flag: Bool = exists(\"fixture\");")),
        0,
        "exists returns Bool"
    );
    assert_eq!(
        check_src(
            "module m;\nfn read<T>(value: T) -> T { return value; }\n\
                 #[test] entity T {}\nimpl T { let value: integer = read(1); }\n"
        ),
        0,
        "a declared function shadows a runtime primitive with the same name"
    );
}

/// A miscounted `print!` silently rendered an empty slot or dropped an
/// argument — the worst place for that is a testbench you are debugging.
#[test]
fn format_argument_count_must_match() {
    let tb = |body: &str| {
        format!("module m;\n#[test] entity T {{}}\nimpl T {{ let a: unsigned[8] = 1; {body} }}\n")
    };
    assert_eq!(check_src(&tb(r#"print!("{} {}", a);"#)), 1, "too few");
    assert_eq!(check_src(&tb(r#"print!("{}", a, a);"#)), 1, "too many");
    assert_eq!(
        check_src(&tb(r#"print!("none", a);"#)),
        1,
        "no placeholders"
    );
    assert_eq!(check_src(&tb(r#"print!("{} {}", a, a);"#)), 0, "exact");
    // `{{}}` is an escaped brace pair and consumes nothing.
    assert_eq!(
        check_src(&tb(r#"print!("{{}} {}", a);"#)),
        0,
        "escaped braces"
    );
    // `assert!` takes its format string second.
    assert_eq!(check_src(&tb(r#"assert!(a == 1, "ok {}", a);"#)), 0);
    assert_eq!(check_src(&tb(r#"assert!(a == 1, "ok {}");"#)), 1);
}

/// `return` in an entity body was dropped by lowering without a word: an
/// entity describes hardware that is always active, so there is nothing to
/// return from. It stays legal inside a function.
#[test]
fn return_outside_a_function_is_reported() {
    let hw = check_src(
        "module m;\nentity E { y: unsigned[8] out, }\nimpl E { bad: process { y = 1; return; } }\n",
    );
    assert_eq!(hw, 1, "hardware statement position");
    let free_fn = check_src(
        "module m;\nfn f(x: unsigned[8]) -> unsigned[8] { return x + 1; }\n\
             entity E { y: unsigned[8] out }\nimpl E { y = f(1); }\n",
    );
    assert_eq!(free_fn, 0, "a free function may return");
    let method = check_src(
        "module m;\nstruct S { pub v: unsigned[8] }\n\
             impl S { pub fn get() -> unsigned[8] { return self.v; } }\n\
             entity E { y: unsigned[8] out }\nimpl E { let s: S = { .v = 3 }; y = s.get(); }\n",
    );
    assert_eq!(method, 0, "a method may return");
    let nested = check_src(
        "module m;\nfn f(x: unsigned[8]) -> unsigned[8] { if x == 0 { return 1; } return x; }\n\
             entity E { y: unsigned[8] out }\nimpl E { y = f(1); }\n",
    );
    assert_eq!(nested, 0, "including inside a nested block");
}

#[test]
/// Every formatted macro checks its argument count against the format
/// string.
fn every_formatted_macro_checks_arity() {
    let fixture = |call: &str| format!("module m;\n#[test] entity T {{}}\nimpl T {{ {call}; }}\n");
    for call in [
        "print!(\"{}\")",
        "assert!(true, \"{}\")",
        "warn!(true, \"{}\")",
    ] {
        assert_eq!(
            check_src(&fixture(call)),
            1,
            "`{call}` should diagnose its missing format argument"
        );
    }
    assert_eq!(
        check_src(&fixture("warn!(true, \"{}\", 1)")),
        0,
        "a matching warning format argument is valid"
    );
}
