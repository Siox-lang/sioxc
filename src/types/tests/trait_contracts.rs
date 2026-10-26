//! Trait implementations must implement the selected source signature.

use super::*;

fn assert_errors(sink: &DiagnosticSink, expected: usize) {
    let errors: Vec<_> = sink
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
        .collect();
    assert_eq!(errors.len(), expected, "{errors:?}");
    assert!(errors.iter().all(|d| d.code == Some(codes::TYPE_MISMATCH)));
}

#[test]
fn trait_methods_must_match_concrete_parameter_and_return_types() {
    for (requirement, implementation, expected) in [
        (
            "fn apply(self, value: integer) -> integer;",
            "fn apply(self, value: integer) -> integer { return value; }",
            0,
        ),
        (
            "fn apply(self, value: integer) -> integer;",
            "fn apply(self, value: real) -> integer { return 1; }",
            1,
        ),
        (
            "fn apply(self, value: integer) -> integer;",
            "fn apply(self, value: integer) -> real { return 1.0; }",
            1,
        ),
        (
            "fn apply(self, value: integer) -> integer;",
            "fn apply(self, value: integer) {}",
            1,
        ),
        (
            "fn apply(self, value: integer);",
            "fn apply(self, value: integer) -> integer { return value; }",
            1,
        ),
    ] {
        let source = format!(
            "module model; trait Contract {{ {requirement} }}
             struct Value(integer); impl Contract for Value {{ {implementation} }}"
        );
        let sink = check_modules(&[(&source, FileId(0))]);
        let errors: Vec<_> = sink
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
            .collect();
        assert_eq!(errors.len(), expected, "{implementation}: {errors:?}");
        assert!(errors.iter().all(|d| d.code == Some(codes::TYPE_MISMATCH)));
    }
}

#[test]
fn malformed_trait_arguments_do_not_silently_disable_contract_checks() {
    for application in [
        "Contract<integer, real>",
        "Contract",
        "Contract<Missing = integer>",
        "Contract<1>",
    ] {
        let source = format!(
            "module model; trait Contract<T> {{ fn inspect(self); }}
             struct Value {{}} impl {application} for Value {{ fn inspect(self) {{}} }}"
        );
        let sink = check_modules(&[(&source, FileId(0))]);
        assert_eq!(
            sink.error_count(),
            1,
            "{application}: {:?}",
            sink.diagnostics()
        );
        assert_errors(&sink, 1);
    }
}

#[test]
fn trait_arguments_validate_duplicate_names_and_type_value_kinds() {
    for application in [
        "Contract<A = integer, A = real, N = 3>",
        "Contract<integer, real, integer>",
        "Contract<1, real, 3>",
        "Contract<integer, real, Self>",
    ] {
        let source = format!(
            "module model; trait Contract<A, B, N: integer> {{ fn inspect(self); }}
             struct Value {{}} impl {application} for Value {{ fn inspect(self) {{}} }}"
        );
        assert_errors(&check_modules(&[(&source, FileId(0))]), 1);
    }
}

#[test]
fn trait_argument_kinds_follow_explicit_parameter_ids() {
    for (binder, application, expected) in [
        ("<T, N: integer>", "Contract<T, N>", 0),
        (
            "<Element, Width: integer>",
            "Contract<N = Width, T = Element>",
            0,
        ),
        ("<N: integer>", "Contract<N, 3>", 1),
        ("<N: integer>", "Contract<(N, integer), 3>", 1),
        ("<N: integer>", "Contract<N[2], 3>", 1),
        ("<T>", "Contract<integer, T>", 1),
        ("", "Contract<Self, 3>", 0),
    ] {
        let source = format!(
            "module model; trait Contract<T, N: integer> {{ fn inspect(self); }}
             struct Value {{}} impl{binder} {application} for Value {{ fn inspect(self) {{}} }}"
        );
        assert_errors(&check_modules(&[(&source, FileId(0))]), expected);
    }
}

#[test]
fn trait_arguments_accept_aliases_constants_and_enum_values() {
    let source = "module model;
        enum Mode { fast, slow }
        const WIDTH: integer = 3;
        type Row<T, N: integer> = T[N];
        trait Contract<T, N: integer, M: Mode> { fn inspect(self); }
        struct Value {}
        impl Contract<M = Mode::fast, T = Row<integer, 3>, N = WIDTH> for Value {
            fn inspect(self) {}
        }";
    assert_errors(&check_modules(&[(source, FileId(0))]), 0);
}

#[test]
fn tuple_value_parameter_bounds_keep_their_existing_argument_kind() {
    let source = "module model;
        trait Contract<Pair: (integer, integer)> { fn inspect(self); }
        struct Value {}
        impl Contract<(1, 2)> for Value { fn inspect(self) {} }";
    assert_errors(&check_modules(&[(source, FileId(0))]), 0);
}

#[test]
fn invalid_trait_arguments_do_not_cascade_into_method_contract_errors() {
    let source = "module model;
        trait Contract<T> { fn inspect(self, value: T) -> integer; }
        struct Value {}
        impl Contract<integer, real> for Value { fn inspect() {} }";
    assert_errors(&check_modules(&[(source, FileId(0))]), 1);
}

#[test]
fn unresolved_trait_parameter_bounds_do_not_invent_value_kind_errors() {
    let source = "module model;
        trait Contract<T: Missing> { fn inspect(self); }
        struct Value {}
        impl Contract<integer> for Value { fn inspect(self) {} }";
    let sink = check_modules(&[(source, FileId(0))]);
    assert!(sink
        .diagnostics()
        .iter()
        .any(|d| d.code == Some(codes::UNKNOWN_NAME)));
    assert!(!sink
        .diagnostics()
        .iter()
        .any(|d| d.message.contains("requires a value argument")));
}

#[test]
fn trait_argument_diagnostics_keep_definition_and_use_site_identity() {
    let left = "module left; pub trait Contract<T> { fn inspect(self); }";
    let right = "module right; pub trait Contract<T: integer> { fn inspect(self); }";
    let user = "module user; use Types = left::Contract; use Values = right::Contract;
        struct A {} struct B {}
        impl Types<integer> for A { fn inspect(self) {} }
        impl Values<real> for B { fn inspect(self) {} }";
    for sources in [
        [(left, FileId(0)), (right, FileId(1)), (user, FileId(2))],
        [(user, FileId(2)), (right, FileId(1)), (left, FileId(0))],
    ] {
        let sink = check_modules(&sources);
        assert_errors(&sink, 1);
        let error = sink
            .diagnostics()
            .iter()
            .find(|d| d.severity == crate::diag::Severity::Error)
            .unwrap();
        assert_eq!(error.primary.unwrap().file, FileId(2));
        assert_eq!(error.labels[0].span.file, FileId(1));
        assert!(error.message.contains("requires a value argument"));
    }
}

#[test]
fn invalid_method_shapes_do_not_cascade_into_signature_type_errors() {
    for method in [
        "fn apply(value: real) -> real { return value; }",
        "fn apply(self, value: real, extra: integer) -> real { return value; }",
    ] {
        let source = format!(
            "module model; trait Contract {{ fn apply(self, value: integer) -> integer; }}
             struct Value {{}} impl Contract for Value {{ {method} }}"
        );
        assert_errors(&check_modules(&[(&source, FileId(0))]), 1);
    }
}

#[test]
fn trait_arguments_and_aliases_are_substituted_by_parameter_identity() {
    for (input, output, body, expected) in [
        ("integer", "real", "1.0", 0),
        ("real", "real", "1.0", 1),
        ("integer", "integer", "1", 1),
    ] {
        let source = format!(
            "module model; trait Contract<Input, Output> {{
                 fn apply(self, value: Input) -> Output;
             }}
             type In = integer; type Out = real; struct Value(integer);
             impl Contract<Output = Out, Input = In> for Value {{
                 fn apply(self, value: {input}) -> {output} {{ return {body}; }}
             }}"
        );
        assert_errors(&check_modules(&[(&source, FileId(0))]), expected);
    }
}

#[test]
fn trait_value_arguments_normalize_aliases_and_known_array_widths() {
    for (width, expected) in [(3, 0), (5, 2)] {
        let source = format!(
            "module model; trait Contract<T, N: integer> {{
                 fn apply(self, value: T[N]) -> T[N];
             }}
             type Row<T, N: integer> = T[N]; struct Value(integer);
             impl Contract<N = 3, T = integer> for Value {{
                 fn apply(self, value: Row<integer, {width}>) -> Row<integer, {width}> {{
                     return value;
                 }}
             }}"
        );
        assert_errors(&check_modules(&[(&source, FileId(0))]), expected);
    }
}

#[test]
fn trait_self_types_are_rebound_inside_arrays_and_results() {
    for (element, expected) in [("Value", 0), ("Other", 1)] {
        let source = format!(
            "module model; trait Contract {{
                 fn apply(self, values: Self[2]) -> Self;
             }}
             struct Value {{}} struct Other {{}}
             impl Contract for Value {{
                 fn apply(self, values: {element}[2]) -> Value {{ return self; }}
             }}"
        );
        assert_errors(&check_modules(&[(&source, FileId(0))]), expected);
    }
}

#[test]
fn trait_signatures_keep_equal_module_leaves_and_import_renames_separate() {
    let left = "module left; pub type Datum = integer;
        pub trait Contract { fn apply(self, value: Datum); }";
    let right = "module right; pub type Datum = real;
        pub trait Contract { fn apply(self, value: Datum); }";
    for (argument, expected) in [("integer", 0), ("real", 1)] {
        let user = format!(
            "module user; use L = left::Contract; use R = right::Contract;
             type Datum = real; struct A {{}} struct B {{}}
             impl L for A {{ fn apply(self, value: {argument}) {{}} }}
             impl R for B {{ fn apply(self, value: Datum) {{}} }}"
        );
        for sources in [
            [
                (left, FileId(0)),
                (right, FileId(1)),
                (user.as_str(), FileId(2)),
            ],
            [
                (user.as_str(), FileId(2)),
                (right, FileId(1)),
                (left, FileId(0)),
            ],
        ] {
            let sink = check_modules(&sources);
            assert_errors(&sink, expected);
            if expected != 0 {
                let error = sink
                    .diagnostics()
                    .iter()
                    .find(|d| d.severity == crate::diag::Severity::Error)
                    .unwrap();
                assert_eq!(error.primary.unwrap().file, FileId(2));
                assert_eq!(error.labels[0].span.file, FileId(0));
            }
        }
    }
}

#[test]
fn trait_signatures_do_not_use_assignment_coercions() {
    for (required, actual, expected) in [
        ("integer", "U[3]", 1),
        ("U[3]", "S[3]", 1),
        ("U[3]", "U[4]", 1),
        ("U[3]", "U[6..4]", 0),
        ("(integer, U[3])", "(real, U[3])", 1),
    ] {
        let source = format!(
            "module model; enum Bit {{ '0', '1' }} struct U(Bit[]); struct S(Bit[]);
             struct Value {{}}
             trait Contract {{ fn apply(self, value: {required}); }}
             impl Contract for Value {{ fn apply(self, value: {actual}) {{}} }}"
        );
        assert_errors(&check_modules(&[(&source, FileId(0))]), expected);
    }
}

#[test]
fn trait_signatures_leave_unbound_widths_unknown() {
    let source = "module model; trait Contract<N: integer> {
        fn apply(self, value: integer[N]) -> integer[N];
    }
    struct Value {}
    impl<N: integer> Contract<N> for Value {
        fn apply(self, value: integer[N]) -> integer[N] { return value; }
    }";
    assert_errors(&check_modules(&[(source, FileId(0))]), 0);
}

#[test]
fn source_from_contract_names_the_source_and_target_types() {
    for (input, expected) in [("real", 0), ("integer", 1)] {
        let user = format!(
            "module model; struct Value(integer);
             impl core::convert::From<real> for Value {{
                 fn from(value: {input}) -> Value {{ return Value(1); }}
             }}"
        );
        assert_errors(
            &check_modules(&[
                (include_str!("../../../core/convert.siox"), FileId(0)),
                (&user, FileId(1)),
            ]),
            expected,
        );
    }
}

#[test]
fn trait_self_arguments_keep_their_impl_owner_identity() {
    for (input, expected) in [("Value", 0), ("Other", 1)] {
        let source = format!(
            "module model; trait Contract<T> {{ fn apply(self, value: T); }}
             struct Value {{}} struct Other {{}}
             impl Contract<Self> for Value {{ fn apply(self, value: {input}) {{}} }}"
        );
        assert_errors(&check_modules(&[(&source, FileId(0))]), expected);
    }
}
