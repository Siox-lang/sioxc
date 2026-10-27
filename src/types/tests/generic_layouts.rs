//! Applied type widths must use the same declaration-aware scalar evaluation.

use super::*;

#[test]
fn generic_widths_check_resolved_constants_and_function_values() {
    for argument in ["ZERO", "(zero())", "N = ZERO"] {
        let source = format!(
            "module model; const ZERO: integer = 0;
             fn zero() -> integer {{ return ZERO; }}
             struct Packet<N: integer> {{ data: integer[N] }}
             fn inspect(value: Packet<{argument}>) {{}}"
        );
        let sink = check_modules(&[(&source, FileId(0))]);
        assert_eq!(
            sink.error_count(),
            1,
            "{argument}: {:?}",
            sink.diagnostics()
        );
        assert!(sink
            .diagnostics()
            .iter()
            .any(|d| d.code == Some(codes::TYPE_MISMATCH)));
    }
}

#[test]
fn generic_width_overflow_is_diagnostic_not_a_resolver_panic() {
    let source = "module model;
        struct Packet<N: integer> { data: integer[(N - 1) / (0 - 1)] }
        fn inspect(value: Packet<-9223372036854775807>) {}";
    let sink = check_modules(&[(source, FileId(0))]);
    assert_eq!(sink.error_count(), 1, "{:?}", sink.diagnostics());
    assert!(sink
        .diagnostics()
        .iter()
        .any(|d| d.code == Some(codes::TYPE_MISMATCH)));
}

#[test]
fn constructor_qualifier_and_trait_argument_types_check_applied_widths() {
    for use_site in [
        "fn inspect() -> Packet<1> { return Packet<0> {}; }",
        "fn inspect<T>() -> Packet<1> { return Packet<0> {}; }",
        "fn inspect() -> integer { return Packet<0>::size(); }",
        "trait Contract<T> { fn inspect(self); } struct Holder {} impl Contract<Packet<0>> for Holder { fn inspect(self) {} }",
    ] {
        let source = format!("module model;
            struct Packet<N: integer> {{ pub data: integer[N] }}
            impl<N: integer> Packet<N> {{ pub fn size() -> integer {{ return 1; }} }}
            {use_site}");
        let sink = check_modules(&[(&source, FileId(0))]);
        assert_eq!(sink.error_count(), 1, "{source}: {:?}", sink.diagnostics());
        assert!(sink.diagnostics().iter().any(|diagnostic| diagnostic.message.contains("a width of 0")), "{source}: {:?}", sink.diagnostics());
    }
}

#[test]
fn layouts_check_named_forwarding_nested_types_aliases_and_newtypes() {
    for shape in [
        "struct Packet<N: integer> { data: integer[N] }",
        "struct Inner<M: integer> { data: integer[M] } struct Packet<N: integer> { inner: Inner<M = N> }",
        "struct Inner<M: integer> { data: integer[M] } type Packet<N: integer> = Inner<N>;",
        "enum Bit { '0', '1' } struct Packet<N: integer>(Bit[N]);",
        "entity Packet<N: integer> { data: integer[N] out }",
    ] {
        for width in ["0", "-2", "4294967296"] {
            let source = format!("module model; {shape} fn inspect(value: Packet<N = {width}>) {{}}");
            let sink = check_modules(&[(&source, FileId(0))]);
            assert_eq!(sink.error_count(), 1, "{source}: {:?}", sink.diagnostics());
            let diagnostic = sink.diagnostics().iter().find(|d| d.code == Some(codes::TYPE_MISMATCH)).unwrap();
            let application = source.find("Packet<N =").unwrap();
            assert!(diagnostic.primary.unwrap().start as usize >= application, "{:?}", diagnostic);
            assert!(diagnostic.labels.iter().any(|label| (label.span.start as usize) < application), "{:?}", diagnostic);
        }
    }
}

#[test]
fn layout_checks_leave_valid_directional_ranges_and_symbolic_values_alone() {
    for argument in ["-4", "0", "4", "N = -4"] {
        let source = format!(
            "module model;
            struct Packet<N: integer> {{ data: integer[N..0] }}
            fn inspect(value: Packet<{argument}>) {{}}"
        );
        let sink = check_modules(&[(&source, FileId(0))]);
        assert_eq!(sink.error_count(), 0, "{source}: {:?}", sink.diagnostics());
    }
    let sink = check_modules(&[(
        "module model;
        struct Packet<N: integer> { data: integer[N] }
        struct Outer<M: integer> { value: Packet<M> }",
        FileId(0),
    )]);
    assert_eq!(sink.error_count(), 0, "{:?}", sink.diagnostics());
}

#[test]
fn static_bad_layouts_are_not_reported_again_at_each_application() {
    for field in ["integer[0]", "Inner<0>"] {
        let source = format!(
            "module model;
            struct Inner<N: integer> {{ data: integer[N] }}
            struct Outer<M: integer> {{ data: {field} }}
            fn inspect(a: Outer<1>, b: Outer<2>) {{}}"
        );
        let sink = check_modules(&[(&source, FileId(0))]);
        assert_eq!(sink.error_count(), 1, "{source}: {:?}", sink.diagnostics());
    }
}

#[test]
fn type_arguments_check_expression_shaped_and_explicit_array_types() {
    for argument in ["integer[0]", "T = integer[0]", "(integer[0], integer)"] {
        let source = format!(
            "module model;
            struct Packet<T> {{ data: T }}
            fn inspect(value: Packet<{argument}>) {{}}"
        );
        let sink = check_modules(&[(&source, FileId(0))]);
        assert_eq!(sink.error_count(), 1, "{source}: {:?}", sink.diagnostics());
    }
}

#[test]
fn nested_applications_of_one_type_still_check_their_arguments() {
    for argument in [
        "integer[0]",
        "Packet<integer[0]>",
        "Packet<Packet<integer[0]>>",
    ] {
        let source = format!(
            "module model;
            struct Packet<T> {{ data: T }}
            fn inspect(value: Packet<{argument}>) {{}}"
        );
        let sink = check_modules(&[(&source, FileId(0))]);
        assert_eq!(sink.error_count(), 1, "{source}: {:?}", sink.diagnostics());
    }
    let sink = check_modules(&[(
        "module model;
        struct Packet<T> { data: T }
        fn inspect(value: Packet<Packet<Packet<integer>>>) {}",
        FileId(0),
    )]);
    assert_eq!(sink.error_count(), 0, "{:?}", sink.diagnostics());
}

#[test]
fn applied_layouts_validate_argument_names_and_categories() {
    for argument in ["Wrong = 3, M = 4", "N = integer, M = 4", "N = 2, N = 3"] {
        let source = format!(
            "module model;
            struct Packet<N: integer, M: integer> {{ data: integer[N] }}
            fn inspect(value: Packet<{argument}>) {{}}"
        );
        let sink = check_modules(&[(&source, FileId(0))]);
        assert_eq!(sink.error_count(), 1, "{source}: {:?}", sink.diagnostics());
    }
}

#[test]
fn applied_width_values_retain_provider_and_parameter_identity() {
    let provider = "module provider;
        const SIZE: integer = 0;
        pub fn width() -> integer { return SIZE; }
        pub struct Packet<N: integer> { pub data: integer[N + SIZE] }";
    let consumer = "module consumer;
        use Renamed = provider::Packet;
        use provider::width;
        const SIZE: integer = 8;
        fn inspect(value: Renamed<N = (width())>) {}";
    for sources in [
        vec![(provider, FileId(0)), (consumer, FileId(1))],
        vec![(consumer, FileId(1)), (provider, FileId(0))],
    ] {
        let sink = check_modules(&sources);
        assert_eq!(sink.error_count(), 1, "{:?}", sink.diagnostics());
    }
}
