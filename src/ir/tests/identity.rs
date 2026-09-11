//! Resolved declaration identity and module-disambiguation tests.

use super::*;

#[test]
/// Entities with the same leaf name in different modules lower their own
/// resolved bodies rather than one shadowing the other.
fn equal_entity_leaves_lower_the_resolved_bodies() {
    let sources = [
        (
            "module a; pub entity Cell { a: Bit in, y: Bit out } \
                 impl Cell { y = a; }",
            FileId(0),
        ),
        (
            "module b; pub entity Cell { b: Bit in, z: Bit out } \
                 impl Cell { z = b; }",
            FileId(1),
        ),
        (
            "module user; entity Top { a: Bit in, b: Bit in, y: Bit out, z: Bit out } \
                 impl Top { \
                   let left: a::Cell = { .a = a, .y = y }; \
                   let right: b::Cell = { .b = b, .z = z }; \
                 }",
            FileId(2),
        ),
    ];
    let mut sink = DiagnosticSink::new();
    let modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
    let design = lower(&modules, &resolved, &hierarchy, &mut sink);
    let names: HashSet<&str> = design
        .signals
        .iter()
        .map(|signal| signal.path.as_str())
        .collect();

    assert!(names.contains("Top.left.a"));
    assert!(names.contains("Top.left.y"));
    assert!(names.contains("Top.right.b"));
    assert!(names.contains("Top.right.z"));
    assert!(!names.contains("Top.left.z"));
    assert!(!names.contains("Top.right.y"));
}

#[test]
/// Equal root entity leaves get distinct qualified signal paths.
fn equal_root_entity_leaves_get_distinct_qualified_paths() {
    let sources = [
        (
            "module a; pub entity Root { value: integer out } \
                 impl Root { value = 11; }",
            FileId(0),
        ),
        (
            "module b; pub entity Root { value: integer out } \
                 impl Root { value = 22; }",
            FileId(1),
        ),
    ];
    let mut sink = DiagnosticSink::new();
    let modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
    let design = lower(&modules, &resolved, &hierarchy, &mut sink);
    assert_eq!(
        sink.error_count(),
        0,
        "diagnostics: {:#?}",
        sink.diagnostics()
    );

    let driven = |path: &str| {
        let signal = design
            .signals
            .iter()
            .position(|signal| signal.path == path)
            .expect("missing root output") as u32;
        design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(signal))
            .map(|driver| driver.expr.clone())
    };
    assert!(matches!(driven("a::Root.value"), Some(Expr::Const(11))));
    assert!(matches!(driven("b::Root.value"), Some(Expr::Const(22))));
    assert_eq!(hierarchy.to_tree_string(), "a::Root\nb::Root\n");
}

#[test]
/// Free functions with the same leaf name likewise lower their own bodies.
fn equal_free_function_leaves_lower_the_resolved_bodies() {
    let sources = [
        (
            "module a::math; pub fn select() -> integer { return 11; }",
            FileId(0),
        ),
        (
            "module b::math; pub fn select() -> integer { return 22; }",
            FileId(1),
        ),
        (
            "module user; entity Top { left: integer out, right: integer out } \
                 impl Top { left = a::math::select(); right = b::math::select(); }",
            FileId(2),
        ),
    ];
    let mut sink = DiagnosticSink::new();
    let modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
    let design = lower(&modules, &resolved, &hierarchy, &mut sink);
    assert_eq!(
        sink.error_count(),
        0,
        "diagnostics: {:#?}",
        sink.diagnostics()
    );

    let driven = |path: &str| {
        let signal = design
            .signals
            .iter()
            .position(|signal| signal.path == path)
            .expect("missing output signal") as u32;
        design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(signal))
            .map(|driver| driver.expr.clone())
    };
    assert!(matches!(driven("Top.left"), Some(Expr::Const(11))));
    assert!(matches!(driven("Top.right"), Some(Expr::Const(22))));
}

#[test]
/// An entity associated function keeps its resolved owner, so two entities
/// with the same leaf name do not share one.
fn entity_associated_functions_keep_resolved_owner_identity() {
    let sources = [
        (
            "module a; pub entity Device {} \
                 impl Device { pub fn tag() -> integer { return 11; } }",
            FileId(0),
        ),
        (
            "module b; pub entity Device {} \
                 impl Device { pub fn tag() -> integer { return 22; } }",
            FileId(1),
        ),
        (
            "module user; entity Top { left: integer out, right: integer out } \
                 impl Top { left = a::Device::tag(); right = b::Device::tag(); }",
            FileId(2),
        ),
    ];
    let mut sink = DiagnosticSink::new();
    let modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let index = FunctionIndex::new(&resolved);
    let owner_keys: Vec<String> = modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            ast::Item::Impl(im) => index.type_head_key(&im.target),
            _ => None,
        })
        .collect();
    assert_eq!(owner_keys, ["a::Device", "b::Device", "user::Top"]);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
    let design = lower(&modules, &resolved, &hierarchy, &mut sink);
    assert_eq!(
        sink.error_count(),
        0,
        "diagnostics: {:#?}",
        sink.diagnostics()
    );

    let driven = |path: &str| {
        let signal = design
            .signals
            .iter()
            .position(|signal| signal.path == path)
            .expect("missing output signal") as u32;
        design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(signal))
            .map(|driver| driver.expr.clone())
    };
    let left = driven("Top.left");
    let right = driven("Top.right");
    assert!(
        matches!(left, Some(Expr::Const(11))),
        "wrong left driver: {left:?}"
    );
    assert!(
        matches!(right, Some(Expr::Const(22))),
        "wrong right driver: {right:?}"
    );
}

#[test]
/// Type aliases with the same leaf name keep their own representations.
fn equal_type_alias_leaves_keep_the_resolved_representation() {
    let sources = [
        (
            "module a; pub using Scalar = integer<-16..15>; pub using Value = Scalar;",
            FileId(0),
        ),
        (
            "module b; pub using Scalar = integer<-128..127>; pub using Value = Scalar;",
            FileId(1),
        ),
        (
            "module user; entity Top { left: a::Value in, right: b::Value in }",
            FileId(2),
        ),
    ];
    let mut sink = DiagnosticSink::new();
    let modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
    let design = lower(&modules, &resolved, &hierarchy, &mut sink);
    assert_eq!(
        sink.error_count(),
        0,
        "diagnostics: {:#?}",
        sink.diagnostics()
    );

    let signal = |suffix: &str| {
        design
            .signals
            .iter()
            .find(|signal| signal.path.ends_with(suffix))
            .expect("missing aliased signal")
    };
    assert_eq!(signal(".left").width, 5);
    assert_eq!(signal(".left").range, Some((-16, 15)));
    assert_eq!(signal(".right").width, 8);
    assert_eq!(signal(".right").range, Some((-128, 127)));
    assert!(signal(".left").integer && signal(".right").integer);
}

#[test]
/// Enums with the same leaf name keep distinct variants, widths and symbols.
fn equal_enum_leaves_keep_variants_widths_and_symbols_distinct() {
    let sources = [
        (
            "module a; pub enum Base { Idle = 3, Run = 7 } pub enum State(Base);",
            FileId(0),
        ),
        (
            "module b; pub enum Base { Low = 1, High = 9 } pub enum State(Base);",
            FileId(1),
        ),
        (
            "module user; entity Top { left: a::State out, right: b::State out } \
                 impl Top { left = a::State::Run; right = b::State::High; }",
            FileId(2),
        ),
    ];
    let mut sink = DiagnosticSink::new();
    let modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
    let design = lower(&modules, &resolved, &hierarchy, &mut sink);
    assert_eq!(
        sink.error_count(),
        0,
        "diagnostics: {:#?}",
        sink.diagnostics()
    );

    let signal = |suffix: &str| {
        design
            .signals
            .iter()
            .find(|signal| signal.path.ends_with(suffix))
            .expect("missing enum signal")
    };
    assert_eq!(signal(".left").width, 3);
    assert_eq!(signal(".left").enum_type.as_deref(), Some("a::State"));
    assert_eq!(signal(".right").width, 4);
    assert_eq!(signal(".right").enum_type.as_deref(), Some("b::State"));
    assert_eq!(
        design.enum_syms["a::State"].get(&7).map(String::as_str),
        Some("Run")
    );
    assert_eq!(
        design.enum_syms["b::State"].get(&9).map(String::as_str),
        Some("High")
    );
    let driven = |suffix: &str| {
        let target = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(suffix))
            .expect("missing driven enum signal") as u32;
        design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(target))
            .map(|driver| driver.expr.clone())
    };
    assert!(matches!(driven(".left"), Some(Expr::Const(7))));
    assert!(matches!(driven(".right"), Some(Expr::Const(9))));
}

#[test]
/// Structs with the same leaf name keep distinct fields, layouts and
/// drivers.
fn equal_struct_leaves_keep_fields_layouts_and_drivers_distinct() {
    let sources = [
        (
            "module a; pub struct Pair { pub left: integer<0..7> }",
            FileId(0),
        ),
        (
            "module b; pub struct Pair { pub right: integer<0..31> }",
            FileId(1),
        ),
        (
            "module user; entity Top { a_pair: a::Pair out, b_pair: b::Pair out } \
                 impl Top { a_pair = { .left = 5 }; b_pair = { .right = 17 }; }",
            FileId(2),
        ),
    ];
    let mut sink = DiagnosticSink::new();
    let modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
    let design = lower(&modules, &resolved, &hierarchy, &mut sink);
    assert_eq!(
        sink.error_count(),
        0,
        "diagnostics: {:#?}",
        sink.diagnostics()
    );

    let left = design
        .signals
        .iter()
        .find(|signal| signal.path.ends_with(".a_pair.left"))
        .expect("module a field");
    let right = design
        .signals
        .iter()
        .find(|signal| signal.path.ends_with(".b_pair.right"))
        .expect("module b field");
    assert_eq!(left.width, 3);
    assert_eq!(right.width, 5);
    assert!(design
        .signals
        .iter()
        .all(|signal| !signal.path.ends_with(".a_pair.right")));
    assert!(design
        .signals
        .iter()
        .all(|signal| !signal.path.ends_with(".b_pair.left")));
    assert!(matches!(
        &design.source_layouts["Top.a_pair"].kind,
        LayoutKind::Struct { name, .. } if name == "a::Pair"
    ));
    assert!(matches!(
        &design.source_layouts["Top.b_pair"].kind,
        LayoutKind::Struct { name, .. } if name == "b::Pair"
    ));
    let driver_value = |signal: &Signal| {
        let id = design
            .signals
            .iter()
            .position(|candidate| std::ptr::eq(candidate, signal))
            .expect("signal index") as u32;
        design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(id))
            .map(|driver| driver.expr.clone())
    };
    assert!(matches!(driver_value(left), Some(Expr::Const(5))));
    assert!(matches!(driver_value(right), Some(Expr::Const(17))));
}

#[test]
/// View method dispatch uses both the view and the backing type's identity.
fn applied_view_methods_dispatch_on_view_and_backing_identity() {
    let source = "module m; \
            struct Stream { value: integer } struct Queue { value: integer } \
            view Port for Stream { value out } view Port for Queue { value out } \
            impl Stream Port { fn drive(self) { self.value = 11; } } \
            impl Queue Port { fn drive(self) { self.value = 22; } } \
            entity Top { stream: Stream Port, queue: Queue Port } \
            impl Top { stream.drive(); queue.drive(); }";
    let mut sink = DiagnosticSink::new();
    let module = crate::syntax::parse_module(FileId(0), source, &mut sink);
    let modules = [module];
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
    let design = lower(&modules, &resolved, &hierarchy, &mut sink);
    assert_eq!(
        sink.error_count(),
        0,
        "diagnostics: {:#?}",
        sink.diagnostics()
    );

    let driven = |suffix: &str| {
        let target = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(suffix))
            .expect("missing applied-view field") as u32;
        design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(target))
            .map(|driver| driver.expr.clone())
    };
    assert!(matches!(driven(".stream.value"), Some(Expr::Const(11))));
    assert!(matches!(driven(".queue.value"), Some(Expr::Const(22))));
}

#[test]
/// Views and traits with the same leaf name keep module-specific semantics.
fn equal_view_and_trait_leaves_keep_module_specific_semantics() {
    let sources = [
        (
            "module common; pub struct Bus { pub data: integer, pub ready: integer }",
            FileId(0),
        ),
        (
            "module a; \
                 pub trait Measure { fn tag() -> integer { return 11; } } \
                 pub struct Device(integer); impl Measure for Device {} \
                 pub view Endpoint for common::Bus { data out, ready in }",
            FileId(1),
        ),
        (
            "module b; \
                 pub trait Measure { fn tag() -> integer { return 22; } } \
                 pub struct Device(integer); impl Measure for Device {} \
                 pub view Endpoint for common::Bus { data in, ready out }",
            FileId(2),
        ),
        (
            "module user; entity Top { \
                   left_tag: integer out, right_tag: integer out, \
                   left: common::Bus a::Endpoint, right: common::Bus b::Endpoint, \
                   left_seen: integer out, right_seen: integer out \
                 } \
                 impl Top { \
                   left_tag = a::Device::tag(); right_tag = b::Device::tag(); \
                   left.data = 1; left_seen = left.ready; \
                   right.ready = 1; right_seen = right.data; \
                 }",
            FileId(3),
        ),
    ];
    let mut sink = DiagnosticSink::new();
    let modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
    let design = lower(&modules, &resolved, &hierarchy, &mut sink);
    assert_eq!(
        sink.error_count(),
        0,
        "diagnostics: {:#?}",
        sink.diagnostics()
    );

    let driven = |suffix: &str| {
        let target = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(suffix))
            .expect("missing driven signal") as u32;
        design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(target))
            .map(|driver| driver.expr.clone())
    };
    assert!(matches!(driven(".left_tag"), Some(Expr::Const(11))));
    assert!(matches!(driven(".right_tag"), Some(Expr::Const(22))));

    let view = |path: &str| match &design.source_layouts[path].kind {
        LayoutKind::Struct {
            view: Some(view),
            fields,
            ..
        } => (
            view.as_str(),
            fields
                .iter()
                .map(|field| field.direction.clone())
                .collect::<Vec<_>>(),
        ),
        other => panic!("{path} is not an applied view: {other:#?}"),
    };
    assert_eq!(
        view("Top.left"),
        (
            "a::Endpoint@Bus",
            vec![Some(LayoutDirection::Out), Some(LayoutDirection::In)]
        )
    );
    assert_eq!(
        view("Top.right"),
        (
            "b::Endpoint@Bus",
            vec![Some(LayoutDirection::In), Some(LayoutDirection::Out)]
        )
    );
}

#[test]
/// A user trait named like a compiler hook keeps its own module identity and
/// does not become the hook.
fn custom_traits_named_like_hooks_keep_their_module_identity() {
    let sources = [
        (
            "module std::ops; pub trait From { fn from(value: Self) -> Self; }",
            FileId(0),
        ),
        (
            "module a; pub trait From { fn tag() -> integer { return 11; } } \
                 pub struct Item(integer); impl From for Item {}",
            FileId(1),
        ),
        (
            "module b; pub trait From { fn tag() -> integer { return 22; } } \
                 pub struct Item(integer); impl From for Item {}",
            FileId(2),
        ),
        (
            "module user; entity Top { left: integer out, right: integer out } \
                 impl Top { left = a::Item::tag(); right = b::Item::tag(); }",
            FileId(3),
        ),
    ];
    let mut sink = DiagnosticSink::new();
    let modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
    let design = lower(&modules, &resolved, &hierarchy, &mut sink);
    assert_eq!(
        sink.error_count(),
        0,
        "diagnostics: {:#?}",
        sink.diagnostics()
    );

    let driven = |suffix: &str| {
        let target = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(suffix))
            .expect("missing trait-default output") as u32;
        design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(target))
            .map(|driver| driver.expr.clone())
    };
    assert!(matches!(driven(".left"), Some(Expr::Const(11))));
    assert!(matches!(driven(".right"), Some(Expr::Const(22))));
}

#[test]
/// A user trait named like the logic encoding cannot manufacture backend
/// metadata; the encoding comes from std's declaration.
fn custom_logic_encoding_trait_cannot_create_backend_metadata() {
    let sources = [
        ("module std::logic; pub trait LogicEncoding {}", FileId(0)),
        (
            "module custom; pub trait LogicEncoding { \
                    fn to_bool(self) -> integer; \
                    fn is_binary(self) -> integer; \
                    fn is_high_impedance(self) -> integer; \
                    fn to_x01(self) -> Self; \
                 } \
                 pub enum State { A, B } \
                 impl LogicEncoding for State { \
                    fn to_bool(self) -> integer { return 0; } \
                    fn is_binary(self) -> integer { return 1; } \
                    fn is_high_impedance(self) -> integer { return 0; } \
                    fn to_x01(self) -> State { return self; } \
                 }",
            FileId(1),
        ),
        (
            "module user; entity Top { y: integer out } impl Top { y = 1; }",
            FileId(2),
        ),
    ];
    let mut sink = DiagnosticSink::new();
    let modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
    let design = lower(&modules, &resolved, &hierarchy, &mut sink);
    assert_eq!(
        sink.error_count(),
        0,
        "diagnostics: {:#?}",
        sink.diagnostics()
    );
    assert!(
        !design.logic_encodings.contains_key("custom::State"),
        "a same-leaf user trait must not become the canonical std hook"
    );
}

#[test]
/// Array-family recognition is structural: it follows the nominal shape, not
/// a trait name.
fn nominal_array_shape_selects_array_family_not_a_trait_name() {
    let sources = [
        ("module scalar; pub struct Word(integer);", FileId(0)),
        ("module arrays; pub struct Word(integer[]);", FileId(1)),
    ];
    let mut sink = DiagnosticSink::new();
    let modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let fns = FunctionIndex::new(&resolved);
    let families = array_families(&modules, &fns);
    assert_eq!(
        sink.error_count(),
        0,
        "diagnostics: {:#?}",
        sink.diagnostics()
    );
    assert!(!families.contains("scalar::Word"));
    assert!(families.contains("arrays::Word"));
}

#[test]
/// An operator with same-leaf operand types selects the resolved overload.
fn equal_operator_operand_leaves_select_the_resolved_overload() {
    let sources = [
        ("module common; pub struct Acc(integer);", FileId(0)),
        ("module a; pub struct Token(integer);", FileId(1)),
        ("module b; pub struct Token(integer);", FileId(2)),
        (
            "module ops; \
                 impl Operator<\"+\", a::Token, integer> for common::Acc { \
                   fn apply(self, rhs: a::Token) -> integer { return 11; } \
                 } \
                 impl Operator<\"+\", b::Token, integer> for common::Acc { \
                   fn apply(self, rhs: b::Token) -> integer { return 22; } \
                 }",
            FileId(3),
        ),
        (
            "module user; entity Top { left: integer out, right: integer out } \
                 impl Top { \
                   let acc: common::Acc; let a_token: a::Token; let b_token: b::Token; \
                   left = acc + a_token; right = acc + b_token; \
                 }",
            FileId(4),
        ),
    ];
    let mut sink = DiagnosticSink::new();
    let modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
    let design = lower(&modules, &resolved, &hierarchy, &mut sink);
    assert_eq!(
        sink.error_count(),
        0,
        "diagnostics: {:#?}",
        sink.diagnostics()
    );

    let driven = |suffix: &str| {
        let target = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(suffix))
            .expect("missing driven output") as u32;
        design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(target))
            .map(|driver| driver.expr.clone())
    };
    let left = driven(".left");
    let right = driven(".right");
    assert!(matches!(left, Some(Expr::Const(11))), "left: {left:#?}");
    assert!(matches!(right, Some(Expr::Const(22))), "right: {right:#?}");
}

#[test]
/// Module constants with the same leaf name lower their own values.
fn equal_module_constant_leaves_lower_the_resolved_values() {
    let sources = [
        ("module a; pub const VALUE: integer = 11;", FileId(0)),
        ("module b; pub const VALUE: integer = 22;", FileId(1)),
        (
            "module user; entity Top { left: integer out, right: integer out } \
                 impl Top { left = a::VALUE; right = b::VALUE; }",
            FileId(2),
        ),
    ];
    let mut sink = DiagnosticSink::new();
    let modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    let typed = crate::types::check(&modules, &resolved, &mut sink);
    let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
    let design = lower(&modules, &resolved, &hierarchy, &mut sink);
    assert_eq!(
        sink.error_count(),
        0,
        "diagnostics: {:#?}",
        sink.diagnostics()
    );

    let driven = |path: &str| {
        let signal = design
            .signals
            .iter()
            .position(|signal| signal.path == path)
            .expect("missing output signal") as u32;
        design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(signal))
            .map(|driver| driver.expr.clone())
    };
    assert!(matches!(driven("Top.left"), Some(Expr::Const(11))));
    assert!(matches!(driven("Top.right"), Some(Expr::Const(22))));
}

#[test]
/// A ranged module constant keeps its qualified width identity.
fn module_range_constant_keeps_its_qualified_width_identity() {
    let design = lower_src(
        "module widths; const SPAN: range = 7..0; \
             entity Top { len: integer out } \
             impl Top { let bits: unsigned[SPAN]; len = bits'length; }",
    );
    let bits = design
        .signals
        .iter()
        .find(|signal| signal.path == "Top.bits")
        .expect("missing range-sized signal");
    assert_eq!(bits.width, 8);

    let len = design
        .signals
        .iter()
        .position(|signal| signal.path == "Top.len")
        .expect("missing length output") as u32;
    assert!(matches!(
        design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(len))
            .map(|driver| &driver.expr),
        Some(Expr::Const(8))
    ));
}
