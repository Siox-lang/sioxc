//! Tests for frontend lowering and the public IR contract.

use super::*;
use crate::diag::FileId;

/// A minimal `ClockLike` impl so self-contained test sources can use the
/// `clk.rising()` edge methods (std provides these for real designs).
const CLK_PRELUDE: &str = "\n\
        enum Bool { false, true }\n\
        enum Bit { '0', '1' }\n\
        enum ULogic { 'U' = 4, 'X' = 3, '0' = 0, '1' = 1, 'Z' = 2, 'W' = 5, 'L' = 6, 'H' = 7, '-' = 8 }\n\
        enum Logic(ULogic);\n\
        impl LogicEncoding for Bit { fn to_bool(self) -> Bool { return self == '1'; } fn is_binary(self) -> Bool { return true; } fn is_high_impedance(self) -> Bool { return false; } fn to_x01(self) -> Bit { return self; } }\n\
        impl LogicEncoding for Logic { fn to_bool(self) -> Bool { return self == '1' or self == 'H'; } fn is_binary(self) -> Bool { return self == '0' or self == '1'; } fn is_high_impedance(self) -> Bool { return self == 'Z'; } fn to_x01(self) -> Logic { if self == '0' or self == 'L' { return '0'; } if self == '1' or self == 'H' { return '1'; } return 'X'; } }\n\
        impl Boolean for Bit { fn as_bool(self) -> Bool { return true; } }\n\
        impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } }\n\
        impl Operator<\"and\", Bool, Bool> for Bool { fn apply(self, rhs: Bool) -> Bool { return self; } }\n\
        impl Operator<\"or\", Bool, Bool> for Bool { fn apply(self, rhs: Bool) -> Bool { return self; } }\n\
        impl Operator<\"not\", Bool, Bool> for Bool { fn apply(self) -> Bool { return self; } }\n\
        trait ClockLike { fn rising(self) -> Bool; fn falling(self) -> Bool; fn edge(self) -> Bool; }\n\
        impl ClockLike for Bit { fn rising(self) -> Bool { return self'event and self'old == '0' and self == '1'; } fn falling(self) -> Bool { return self'event and self'old == '1' and self == '0'; } fn edge(self) -> Bool { return self'event; } }\n";

/// A match naming every variant of its scrutinee is as complete as one
/// ending in `_`, so a signal every arm assigns is not a latch. Only the
/// wildcard half was implemented, so the natural spelling of an exhaustive
/// decode drew an inferred-latch warning whose suggested fix is a
/// redundant `_` arm.
/// An initializer is a signal's power-on value, folded at elaboration. One
/// that reads another signal cannot fold, and was dropped in silence: the
/// signal kept its type's default, so `let i: unsigned[8] = a + 1;` read 0
/// while `i = a + 1;` — a driver, and a different thing — read 201.
#[test]
fn a_non_constant_initializer_is_reported() {
    let count = |src: &str| {
        lower_diags(src)
            .into_iter()
            .filter(|d| d.contains("is not a constant"))
            .count()
    };
    assert_eq!(
        count(
            "module m;\nentity E { y: unsigned[8] out }\n\
                 impl E { let a: unsigned[8] = 200; let i: unsigned[8] = a + 1; y = i; }\n"
        ),
        1,
        "an initializer reading another signal"
    );
    // Driving it is the spelling that means "compute this", and is fine.
    assert_eq!(
        count(
            "module m;\nentity E { y: unsigned[8] out }\n\
                 impl E { let a: unsigned[8] = 200; let i: unsigned[8]; i = a + 1; y = i; }\n"
        ),
        0,
        "the driver spelling is not an initializer"
    );
    // Everything that can fold still seeds without complaint: a literal, a
    // module constant, an arithmetic fold, and a const-evaluable call.
    assert_eq!(
        count(
            "module m;\nconst K: unsigned[8] = 5;\n\
                 fn twice(n: unsigned[8]) -> unsigned[8] { return n * 2; }\n\
                 entity E { y: unsigned[8] out }\n\
                 impl E { let a: unsigned[8] = 200; let b: unsigned[8] = K;\n\
                 let c: unsigned[8] = 3 * 7; let d: unsigned[8] = twice(4);\n\
                 y = a + b + c + d; }\n"
        ),
        0,
        "literals, constants, folds and const calls all seed"
    );

    // The aggregate sites seed inits the same way and dropped a
    // non-constant the same way — and there, no undriven lint reaches a
    // struct leaf or an array element, so nothing was reported at all.
    assert_eq!(
        count(
            "module m;\nstruct P { x: unsigned[8], y: unsigned[8] }\n\
                 entity E { src: unsigned[8] in, y: unsigned[8] out }\n\
                 impl E { let p: P = { .x = 7, .y = src + 1 }; y = p.y; }\n"
        ),
        1,
        "a struct-field initializer reading a signal"
    );
    assert_eq!(
        count(
            "module m;\nentity E { src: unsigned[8] in, y: unsigned[8] out }\n\
                 impl E { let arr: unsigned[8][2] = [9, src + 2]; y = arr[1]; }\n"
        ),
        1,
        "an array-element initializer reading a signal"
    );
    // Constant aggregates keep seeding.
    assert_eq!(
        count(
            "module m;\nconst K: unsigned[8] = 5;\n\
                 struct P { x: unsigned[8], y: unsigned[8] }\n\
                 entity E { y: unsigned[8] out }\n\
                 impl E { let p: P = { .x = K + 2, .y = 3 };\n\
                 let arr: unsigned[8][2] = [1, 3 * 4]; y = p.x + arr[1]; }\n"
        ),
        0,
        "a constant struct literal and array literal still seed"
    );
}

#[test]
/// An exhaustive match assigns on every path, so it must not be reported as
/// an inferred latch.
fn an_exhaustive_match_is_not_an_inferred_latch() {
    let latches = |src: &str| {
        lower_diags(src)
            .into_iter()
            .filter(|d| d.contains("inferred latch"))
            .count()
    };
    const ENUM: &str = "module m;\nenum State { Idle, Run }\n";

    // Every variant named, every arm assigning `a`.
    assert_eq!(
        latches(&format!(
            "{ENUM}entity E {{ s: State in, a: unsigned[8] out }}\n\
                 impl E {{ match s {{ State::Idle => a = 10, State::Run => a = 20, }} }}\n"
        )),
        0,
        "a match over every variant drives on every path"
    );

    // The same over a character-valued enum.
    assert_eq!(
        latches(
            "module m;\nentity E { b: Bit in, a: unsigned[8] out }\n\
                 impl E { match b { '0' => a = 10, '1' => a = 20, } }\n"
        ),
        0,
        "and over `Bit`, whose variants are character literals"
    );

    // A variant left out is a genuine latch.
    assert_eq!(
        latches(&format!(
            "{ENUM}entity E {{ s: State in, a: unsigned[8] out }}\n\
                 impl E {{ match s {{ State::Idle => a = 10, }} }}\n"
        )),
        1,
        "an unmatched variant still holds the previous value"
    );

    // Exhaustive, but one arm does not assign the signal.
    assert_eq!(
        latches(&format!(
            "{ENUM}entity E {{ s: State in, a: unsigned[8] out, k: unsigned[8] out }}\n\
                 impl E {{ a = 0; match s {{ State::Idle => k = 1, State::Run => a = 2, }} }}\n"
        )),
        1,
        "a signal only one arm assigns is a latch even when the match is complete"
    );
}

/// Lower `src` with the minimal library types the tests need.
fn lower_src(src: &str) -> Design {
    // unsigned/signed are library types (attribute-marked vectors), not seeded.
    let src = format!("{src}\nstruct unsigned(Logic[]);\nstruct signed(Logic[]);\n{CLK_PRELUDE}");
    let src = src.as_str();
    let mut sink = DiagnosticSink::new();
    let module = crate::syntax::parse_module(FileId(0), src, &mut sink);
    assert_eq!(sink.error_count(), 0, "parse errors:\n{src}");
    let modules = std::slice::from_ref(&module);
    let resolved = crate::resolve::resolve(modules, &mut sink);
    let typed = crate::types::check(modules, &resolved, &mut sink);
    let hier = crate::elab::elaborate(modules, &resolved, &typed, &mut sink);
    lower(modules, &resolved, &hier, &mut sink)
}

/// Lower `src` and return the diagnostics it produced.
fn lower_diagnostics(src: &str) -> Vec<crate::diag::Diagnostic> {
    let src = format!("{src}\nstruct unsigned(Logic[]);\nstruct signed(Logic[]);\n{CLK_PRELUDE}");
    let mut sink = DiagnosticSink::new();
    let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
    let modules = std::slice::from_ref(&module);
    let resolved = crate::resolve::resolve(modules, &mut sink);
    let typed = crate::types::check(modules, &resolved, &mut sink);
    let hier = crate::elab::elaborate(modules, &resolved, &typed, &mut sink);
    let _ = lower(modules, &resolved, &hier, &mut sink);
    sink.diagnostics().to_vec()
}

/// Lower `src` and return its diagnostic messages as strings.
fn lower_diags(src: &str) -> Vec<String> {
    lower_diagnostics(src)
        .iter()
        .map(|d| format!("{:?}: {}", d.code, d.message))
        .collect()
}

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

#[test]
/// Late IR lints point at the signal's declaration, which is the only source
/// location a synthesized driver has.
fn late_ir_lints_point_at_the_signal_declaration() {
    let source = "module m;\n\
            entity L { c: Logic in, looped: unsigned[8] out, latched: unsigned[8] out, forgotten: unsigned[8] out }\n\
            impl L {\n\
              let discarded: unsigned[8];\n\
              looped = looped;\n\
              if c == '1' { latched = 1; }\n\
              discarded = 2;\n\
            }\n\
            entity Top {}\n\
            impl Top {\n\
              let c: Logic = '0';\n\
              let looped: unsigned[8]; let latched: unsigned[8]; let forgotten: unsigned[8];\n\
              let dut: L = { .c = c, .looped = looped, .latched = latched, .forgotten = forgotten };\n\
            }\n";
    let diagnostics = lower_diagnostics(source);
    let cases = [
        (crate::diag::codes::COMBINATIONAL_LOOP, "looped", "looped:"),
        (crate::diag::codes::POSSIBLE_LATCH, "latched", "latched:"),
        (
            crate::diag::codes::UNDRIVEN_OUTPUT,
            "forgotten",
            "forgotten:",
        ),
        (
            crate::diag::codes::UNUSED_SIGNAL,
            "discarded",
            "let discarded:",
        ),
    ];
    for (code, signal, declaration) in cases {
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == Some(code) && diagnostic.message.contains(signal))
            .unwrap_or_else(|| panic!("missing {code} for {declaration}: {diagnostics:#?}"));
        let span = diagnostic
            .primary
            .unwrap_or_else(|| panic!("{code} has no primary span: {diagnostic:#?}"));
        let rendered = &source[span.start as usize..span.end as usize];
        assert!(
            rendered.contains(declaration),
            "{code} points at {rendered:?}, not declaration {declaration:?}"
        );
    }
}

#[test]
/// Late IR errors keep their stable codes and real source spans rather than
/// reporting against generated shapes.
fn late_ir_errors_keep_stable_codes_and_source_spans() {
    let cases = [
            (
                "module m;\nentity E {}\nimpl E { let data: unsigned[8][2] = read<unsigned[8]>(\"__siox_missing_span_fixture__.bin\"); }\n",
                crate::diag::codes::COMPILE_TIME_IO,
                "let data:",
            ),
            (
                "module m;\nfn recurse(v: unsigned[8]) -> unsigned[8] { return recurse(v); }\nentity E { a: unsigned[8] in, y: unsigned[8] out }\nimpl E { y = recurse(a); }\n",
                crate::diag::codes::UNBOUNDED_RECURSION,
                "recurse",
            ),
            (
                "module m;\nentity E { a: unsigned[8] in, y: unsigned[8] out }\nimpl E { y = a after 1; }\n",
                crate::diag::codes::TYPE_MISMATCH,
                "y = a after 1",
            ),
        ];
    for (source, code, source_text) in cases {
        let diagnostics = lower_diagnostics(source);
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == Some(code))
            .unwrap_or_else(|| panic!("missing {code}: {diagnostics:#?}"));
        let span = diagnostic
            .primary
            .unwrap_or_else(|| panic!("{code} has no primary span: {diagnostic:#?}"));
        let rendered = &source[span.start as usize..span.end as usize];
        assert!(
            rendered.contains(source_text),
            "{code} points at {rendered:?}, not {source_text:?}"
        );
    }
}

/// Generate loops are unrolled after type checking, so the source-level
/// dead-assignment lint could not see two iterations/loops resolve to the
/// same concrete target.
#[test]
fn generated_dead_assignments_warn_after_specialization() {
    let warning_count = |source: &str| {
        lower_diags(source)
            .iter()
            .filter(|diagnostic| diagnostic.starts_with("Some(\"W-P014\")"))
            .count()
    };
    let overlapping = "module m;
             entity E { y: unsigned[8] out, }
             impl E {
                 let values: unsigned[8][4];
                 process {
                     for i in 0..2 { values[i] = 11; }
                     for i in 2..3 { values[i] = 22; }
                 }
                 y = values[2];
             }";
    assert_eq!(
        warning_count(overlapping),
        1,
        "the generated writes to values[2] overlap"
    );

    let repeated_instance = "module m;
             entity E { y: unsigned[8] out, }
             impl E {
                 let values: unsigned[8][4];
                 process {
                     for i in 0..2 { values[i] = 11; }
                     for i in 2..3 { values[i] = 22; }
                 }
                 y = values[2];
             }
             entity H { a: unsigned[8] out, b: unsigned[8] out, }
             impl H {
                 let first: E = { .y = a };
                 let second: E = { .y = b };
             }";
    assert_eq!(
        warning_count(repeated_instance),
        1,
        "a source warning must not repeat for every instance"
    );

    let direct = "module m;
             entity E { y: unsigned[8] out, }
             impl E { process { y = 1; y = 2; } }";
    assert_eq!(
        warning_count(direct),
        1,
        "the IR lint must not duplicate the frontend warning"
    );

    let selected_block = "module m;
             entity E { y: unsigned[8] out, }
             impl E { process { if true { y = 1; y = 2; } } }";
    assert_eq!(
        warning_count(selected_block),
        1,
        "specializing a block must not repeat its frontend warning"
    );

    let disjoint = "module m;
             entity E { y: unsigned[8] out, }
             impl E {
                 let values: unsigned[8][4];
                 process {
                     for i in 0..1 { values[i] = 11; }
                     for i in 2..3 { values[i] = 22; }
                 }
                 y = values[2];
             }";
    assert_eq!(
        warning_count(disjoint),
        0,
        "disjoint generated targets are independent"
    );
}

#[test]
/// A multi-word integer literal keeps every word; there is no one-word
/// ceiling.
fn integer_literals_keep_all_words() {
    let d = lower_src(
        "module m;
             entity E { y: unsigned[192] out, }
             impl E { y = 1020847100762815390427017310442723737601; }",
    );
    assert!(matches!(
        &d.drivers[0].expr,
        Expr::WideConst(words) if words == &[1, 2, 3]
    ));
}

#[test]
/// Signals retain their kernel-`integer` identity, so signed operators and
/// formatting apply.
fn signals_retain_kernel_integer_identity() {
    let design = lower_src(
        "module m;
             entity E {
                 plain: integer out,
                 constrained: integer<-10..10> out,
                 bits: unsigned[8] out,
             }
             impl E { plain = -8; constrained = -3; bits = 255; }",
    );
    let signal = |suffix: &str| {
        design
            .signals
            .iter()
            .find(|signal| signal.path.ends_with(suffix))
            .unwrap_or_else(|| panic!("no signal {suffix}"))
    };
    assert!(signal(".plain").integer);
    assert!(signal(".constrained").integer);
    assert!(!signal(".bits").integer);
}

#[test]
/// A deep but acyclic derivation chain has no arbitrary depth limit.
fn deep_acyclic_type_derivation_has_no_magic_depth_limit() {
    let mut src = String::from("module m;\nstruct S0(Bit);\n");
    for i in 1..80 {
        src.push_str(&format!("struct S{i}(S{});\n", i - 1));
    }
    src.push_str(
        "entity E { y: S79 out, }
             impl E { y = S79(); }",
    );
    let d = lower_src(&src);
    assert_eq!(d.signals.iter().find(|s| s.path == "E.y").unwrap().width, 1);
}

/// A struct-literal initializer on an entity-level `let` silently powered
/// on at 0 — the testbench interpreter honoured it, hardware lowering did
/// not, so the two engines disagreed about the same declaration.
#[test]
fn struct_literal_initializer_seeds_field_inits() {
    let d = lower_src(
        "module m; struct P { a: unsigned[8], b: unsigned[8] }\n\
             entity E { x: unsigned[8] out, y: unsigned[8] out }\n\
             impl E { let p: P = { .a = 11, .b = 22 }; x = p.a; y = p.b; }\n\
             entity H { x: unsigned[8] out, y: unsigned[8] out, }\n\
             impl H { let d: E = { .x = x, .y = y }; }",
    );
    let init = |suffix: &str| {
        d.signals
            .iter()
            .find(|s| s.path.ends_with(suffix))
            .unwrap_or_else(|| panic!("no signal {suffix}"))
            .init
            .first()
            .copied()
            .unwrap_or(0)
    };
    assert_eq!(init(".p.a"), 11);
    assert_eq!(init(".p.b"), 22);
}

/// A concat target has an exact width, so the source must match it — the
/// lowering otherwise just sliced whatever it was given and zero-filled.
#[test]
fn concat_assignment_target_width_must_match() {
    let src = |rhs: &str| {
        format!(
                "module m;\nentity E {{ a: unsigned[8] in, y: unsigned[4] out, z: unsigned[4] out, }}\n\
                 impl E {{ {{y, z}} = {rhs}; }}\n\
                 entity H {{ a: unsigned[8] in, y: unsigned[4] out, z: unsigned[4] out, }}\n\
                 impl H {{ let d: E = {{ .a = a, .y = y, .z = z }}; }}\n"
            )
    };
    let mismatched = lower_diags(&src("a[3..0]"));
    assert!(
        mismatched
            .iter()
            .any(|d| d.contains("concatenation target is 8 bits")),
        "expected a width mismatch, got: {mismatched:?}"
    );
    let exact = lower_diags(&src("a"));
    assert!(
        !exact.iter().any(|d| d.contains("concatenation target")),
        "an exact-width source is fine: {exact:?}"
    );
}

/// Two producers wired to one bus net is the classic miswiring. It must
/// name the conflict (not the missing `Resolve` impl it happens to hit),
/// carry a code, and point at each contributing connection — this is the
/// only guard left now that views carry no coarse endpoint role.
#[test]
fn conflicting_drivers_name_the_conflict_and_its_sites() {
    let src = "module m;\n\
            struct Stream { valid: Bit, data: unsigned[8] }\n\
            view Source for Stream { valid out, data out }\n\
            entity Producer { bus: Stream Source, value: unsigned[8] in }\n\
            impl Producer { bus.valid = '1'; bus.data = value; }\n\
            entity BadLink { a: unsigned[8] in, b: unsigned[8] in }\n\
            impl BadLink {\n\
              let wire: Stream;\n\
              let p1: Producer = { .bus = wire, .value = a };\n\
              let p2: Producer = { .bus = wire, .value = b };\n\
            }\n";
    let mut sink = DiagnosticSink::new();
    let full = format!("{src}\nstruct unsigned(Logic[]);\nstruct signed(Logic[]);\n{CLK_PRELUDE}");
    let module = crate::syntax::parse_module(FileId(0), &full, &mut sink);
    let modules = std::slice::from_ref(&module);
    let resolved = crate::resolve::resolve(modules, &mut sink);
    let typed = crate::types::check(modules, &resolved, &mut sink);
    let hier = crate::elab::elaborate(modules, &resolved, &typed, &mut sink);
    let _ = lower(modules, &resolved, &hier, &mut sink);

    let conflicts: Vec<_> = sink
        .diagnostics()
        .iter()
        .filter(|d| d.code == Some(crate::diag::codes::CONFLICTING_DRIVERS))
        .collect();
    assert!(
        !conflicts.is_empty(),
        "expected a conflicting-drivers error"
    );
    for d in &conflicts {
        assert!(
            d.message.contains("conflicting sources"),
            "should name the conflict, got: {}",
            d.message
        );
        assert!(d.primary.is_some(), "should point at a connection site");
        assert!(!d.labels.is_empty(), "should label the other source(s)");
        assert!(d.help.is_some(), "should say how to fix it");
    }
}

#[test]
/// Parallel drivers on a type with a `Resolve` impl are legal and must not
/// warn.
fn resolved_parallel_drivers_are_legal_without_a_warning() {
    let diagnostics = lower_diags(
        "module m;\n\
             enum Wire { '0', '1' }\n\
             impl Resolve for Wire {\n\
                 fn resolve(self, rhs: Wire) -> Wire { return self; }\n\
             }\n\
             entity Net { a: Wire in, b: Wire in, y: Wire out }\n\
             impl Net { y = a; }\n\
             impl Net { y = b; }\n",
    );
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.contains("driver") || diagnostic.contains("W-P001") }),
        "a type-defined Resolve fold is intentional, not suspicious: {diagnostics:?}"
    );
}

#[test]
/// Bit patterns lower to the mask and match pair they imply.
fn bit_pattern_masks() {
    // Bare strings are per-bit with `-` as the don't-care.
    assert_eq!(
        crate::syntax::bit_pattern_mask("\"01--\""),
        Some((vec![0b1100], vec![0b0100]))
    );
    assert_eq!(
        crate::syntax::bit_pattern_mask("\"0000_11--\""),
        Some((vec![0b11111100], vec![0b00001100]))
    );
    // Radix prefixes mask a whole group with `?`.
    assert_eq!(
        crate::syntax::bit_pattern_mask("x\"A?\""),
        Some((vec![0xF0], vec![0xA0]))
    );
    assert_eq!(
        crate::syntax::bit_pattern_mask("x\"?3\""),
        Some((vec![0x0F], vec![0x03]))
    );
    assert_eq!(
        crate::syntax::bit_pattern_mask("o\"7?\""),
        Some((vec![0o70], vec![0o70]))
    );
    let wide = format!("\"1{}\"", "-".repeat(128));
    assert_eq!(
        crate::syntax::bit_pattern_mask(&wide),
        Some((vec![0, 0, 1], vec![0, 0, 1]))
    );
    assert_eq!(crate::syntax::bit_pattern_mask("\"2\""), None); // bad binary digit
}

#[test]
/// An applied view flattens the backing struct's fields.
fn applied_view_flattens_its_backing_struct_fields() {
    let d = lower_src(
        "module m;\n\
             struct HandshakeBus { valid: Bit, ready: Bit }\n\
             view Handshake for HandshakeBus { valid out, ready in }\n\
             impl HandshakeBus Handshake { fn assert_valid(self) { self.valid = '1'; } }\n\
             entity Producer { bus: HandshakeBus Handshake, observed: Bit out, }\n\
             impl Producer { bus.assert_valid(); observed = bus.ready; }",
    );
    assert!(d.signals.iter().any(|s| s.path.ends_with(".bus.valid")));
    assert!(d.signals.iter().any(|s| s.path.ends_with(".bus.ready")));
    assert!(matches!(
        d.source_layouts.get("Producer.bus").map(|layout| &layout.kind),
        Some(LayoutKind::Struct {
            name,
            view: Some(view),
            fields,
        }) if name == "HandshakeBus"
            && view == "Handshake@HandshakeBus"
            && fields.iter().map(|field| field.direction.clone()).collect::<Vec<_>>()
                == [Some(LayoutDirection::Out), Some(LayoutDirection::In)]
    ));
}

#[test]
/// A generic trait function reaches an applied view's backing fields.
fn generic_trait_functions_access_applied_view_backing_fields() {
    let d = lower_src(
        "module m;\n\
             trait Readable<T> { fn read(self) -> T; }\n\
             trait Writable<T> { fn write(self, value: T); }\n\
             fn read<Bus: Readable, Value>(bus: Bus) -> Value { return bus.read(); }\n\
             fn write<Bus: Writable, Value>(bus: Bus, value: Value) { bus.write(value); }\n\
             struct Spi { tx: unsigned[8], rx: unsigned[8] }\n\
             view Controller for Spi { tx out, rx in }\n\
             impl Readable<unsigned[8]> for Spi Controller {\n\
               fn read(self) -> unsigned[8] { return self.rx; }\n\
             }\n\
             impl Writable<unsigned[8]> for Spi Controller {\n\
               fn write(self, value: unsigned[8]) { self.tx = value; }\n\
             }\n\
             entity Device {\n\
               bus: Spi Controller, source: unsigned[8] in, sampled: unsigned[8] out,\n\
             }\n\
             impl Device { write(bus, source); sampled = read(bus); }\n\
             entity Link {}\n\
             impl Link {\n\
               let wire: Spi;\n\
               let source: unsigned[8];\n\
               let sampled: unsigned[8];\n\
               let device: Device = { .bus = wire, .source = source, .sampled = sampled };\n\
             }",
    );
    assert!(
        d.validate().is_empty(),
        "a view method must bind `self` to the backing struct fields"
    );
    let sampled = d
        .signals
        .iter()
        .position(|s| s.path.ends_with(".device.sampled"))
        .expect("sampled signal");
    let driver = d
        .drivers
        .iter()
        .find(|driver| driver.target.0 as usize == sampled)
        .expect("sampled driver");
    assert!(
        matches!(driver.expr, Expr::Current(_)),
        "read() should lower to the selected backing signal: {:?}",
        driver.expr
    );
    let tx = d
        .signals
        .iter()
        .position(|s| s.path.ends_with(".device.bus.tx"))
        .expect("view tx signal");
    assert!(
        d.drivers
            .iter()
            .any(|driver| driver.target.0 as usize == tx),
        "generic write() should inline its nested trait method as a driver"
    );
}

#[test]
/// An enum signal with no initializer takes its first variant, so it is
/// always initialized.
fn enum_signal_inits_to_first_variant() {
    // Derived `new()` default: an uninitialized enum signal powers on
    // holding its *first* variant. With a non-zero-based first
    // discriminant, that is a valid member — a bare `0` would not be.
    let d = lower_src(
        "module m;\n\
             enum Phase { Idle = 2, Run = 3, Done = 4 }\n\
             enum Step  { A, B, C }\n\
             entity T {}\n\
             impl T { let p: Phase; let s: Step; }\n",
    );
    let init = |suffix: &str| {
        d.signals
            .iter()
            .find(|s| s.path.ends_with(suffix))
            .unwrap_or_else(|| panic!("no {suffix}"))
            .init
            .first()
            .copied()
            .unwrap_or(0)
    };
    assert_eq!(init(".p"), 2, "Phase defaults to Idle = 2, not 0");
    assert_eq!(init(".s"), 0, "0-based Step still defaults to A = 0");
}

#[test]
/// An explicit initializer overrides the first-variant default.
fn explicit_enum_init_overrides_first_variant() {
    // An explicit `let p = Run` beats the first-variant default.
    let d = lower_src(
        "module m;\n\
             enum Phase { Idle = 2, Run = 3, Done = 4 }\n\
             entity T {}\n\
             impl T { let p: Phase = Phase::Run; }\n",
    );
    let p = d
        .signals
        .iter()
        .find(|s| s.path.ends_with(".p"))
        .expect("no .p");
    assert_eq!(p.init, vec![3], "explicit initializer Run = 3 wins");
}

#[test]
/// `T()` lowers to the type's structural default rather than a call.
fn nullary_constructor_lowers_to_default() {
    // `T()` in expression position is the type's derived default: an enum →
    // its first variant, a numeric/vector → 0. Same rule as the implicit
    // signal init, now writable.
    let d = lower_src(
        "module m;\n\
             enum Phase { Idle = 2, Run = 3 }\n\
             entity E { y: Phase out, z: unsigned[8] out }\n\
             impl E { y = Phase(); z = unsigned[8](); }\n",
    );
    let drv = |suffix: &str| -> u64 {
        let sig = d
            .signals
            .iter()
            .position(|s| s.path.ends_with(suffix))
            .expect("sig");
        let dr = d
            .drivers
            .iter()
            .find(|dr| dr.target.0 as usize == sig)
            .expect("driver");
        match &dr.expr {
            Expr::Const(c) => *c,
            other => panic!("{suffix} not a const: {other:?}"),
        }
    };
    assert_eq!(drv(".y"), 2, "Phase() == first variant Idle = 2");
    assert_eq!(drv(".z"), 0, "unsigned[8]() == 0");
}

#[test]
/// `T()` on a struct fills every field with its default.
fn nullary_constructor_defaults_struct_fields() {
    // `S()` on a struct defaults each field structurally: an enum field to
    // its first variant, a numeric field to 0 — through a composed struct
    // field as well as a direct one.
    let d = lower_src(
        "module m;\n\
             enum Phase { Idle = 2, Run = 3 }\n\
             struct Header { flag: Bit, ph: Phase }\n\
             struct Packet { header: Header, data: unsigned[8] }\n\
             entity E { o: Packet out }\n\
             impl E { o = Packet::new(); }\n",
    );
    let drv = |suffix: &str| -> u64 {
        let sig = d
            .signals
            .iter()
            .position(|s| s.path.ends_with(suffix))
            .expect("sig");
        let dr = d
            .drivers
            .iter()
            .find(|dr| dr.target.0 as usize == sig)
            .expect("driver");
        match &dr.expr {
            Expr::Const(c) => *c,
            other => panic!("{suffix} not a const: {other:?}"),
        }
    };
    assert_eq!(
        drv(".o.header.ph"),
        2,
        "enum field → first variant Idle = 2"
    );
    assert_eq!(drv(".o.header.flag"), 0, "composed Bit field → 0");
    assert_eq!(drv(".o.data"), 0, "numeric field → 0");
}

#[test]
/// The range attributes read the declared bounds, in the declared direction.
fn range_attributes_read_declared_bounds() {
    // A descending `[7..0]` and an ascending width-only `[8]` expose the
    // VHDL range attributes; direction is preserved.
    let d = lower_src(
        "module m;\n\
             entity E {\n\
               dn: unsigned[7..0] in, up: unsigned[8] in,\n\
               a: unsigned[8] out, b: unsigned[8] out, c: unsigned[8] out, e: unsigned[8] out,\n\
               f: unsigned[8] out, g: unsigned[8] out, h: unsigned[8] out,\n\
             }\n\
             impl E {\n\
               a = dn'left; b = dn'right; c = dn'high; e = dn'low;\n\
               f = dn'ascending; g = dn'length; h = up'ascending;\n\
             }\n",
    );
    let drv = |suffix: &str| -> u64 {
        let sig = d
            .signals
            .iter()
            .position(|s| s.path.ends_with(suffix))
            .expect("sig");
        let dr = d
            .drivers
            .iter()
            .find(|dr| dr.target.0 as usize == sig)
            .expect("driver");
        match &dr.expr {
            Expr::Const(c) => *c,
            other => panic!("{suffix} not const: {other:?}"),
        }
    };
    assert_eq!(drv(".a"), 7, "dn'left");
    assert_eq!(drv(".b"), 0, "dn'right");
    assert_eq!(drv(".c"), 7, "dn'high");
    assert_eq!(drv(".e"), 0, "dn'low");
    assert_eq!(drv(".f"), 0, "dn'ascending (descending → false)");
    assert_eq!(drv(".g"), 8, "dn'length");
    assert_eq!(drv(".h"), 1, "up'ascending (width-only → true)");
}

#[test]
/// An output port nothing drives warns (W-P011).
fn undriven_output_port_warns() {
    // `forgotten` is never assigned; `driven` is. Only the former warns.
    let diags = lower_diags(
        "module m;\n\
             entity E { a: unsigned[8] in, driven: unsigned[8] out, forgotten: unsigned[8] out }\n\
             impl E { driven = a + 1; }\n\
             entity T {}\n\
             impl T { let a: unsigned[8]; let d: unsigned[8]; let f: unsigned[8];\n\
               let dut: E = { .a = a, .driven = d, .forgotten = f }; }\n",
    );
    let undriven: Vec<&String> = diags.iter().filter(|d| d.contains("W-P011")).collect();
    assert_eq!(
        undriven.len(),
        1,
        "one undriven-output warning: {undriven:?}"
    );
    assert!(
        undriven[0].contains("forgotten"),
        "flags forgotten: {undriven:?}"
    );
}

#[test]
/// An internal signal nothing drives warns.
fn undriven_internal_signal_warns() {
    // `dead` (value-less, never assigned) warns; `used` is driven and
    // `konst` has an initializer, so neither does.
    let diags = lower_diags(
            "module m;\n\
             entity E { a: unsigned[8] in, y: unsigned[8] out }\n\
             impl E {\n  let used: unsigned[8];\n  let dead: unsigned[8];\n  let konst: unsigned[8] = 5;\n\
               used = a + 1;\n  y = used + konst;\n }\n\
             entity T {}\n\
             impl T { let a: unsigned[8]; let y: unsigned[8]; let dut: E = { .a = a, .y = y }; }\n",
        );
    let undriven: Vec<&String> = diags
        .iter()
        .filter(|d| d.contains("W-P011") && d.contains("never driven"))
        .collect();
    assert_eq!(undriven.len(), 1, "one undriven signal: {undriven:?}");
    assert!(undriven[0].contains("dead"), "flags dead: {undriven:?}");
}

#[test]
/// An unused internal signal warns, without the test runner's own signals
/// producing false positives.
fn unused_internal_signal_warns_without_runner_false_positives() {
    let diags = lower_diags(
        "module m;\n\
             entity E { a: unsigned[8] in, y: unsigned[8] out }\n\
             impl E { let dead: unsigned[8]; dead = a + 1; y = a; }\n\
             #[test] entity T {}\n\
             impl T { let a: unsigned[8]; let observed: unsigned[8];\n\
               let dut: E = { .a = a, .y = observed }; assert!(observed == a); }\n",
    );
    let unused: Vec<&String> = diags.iter().filter(|d| d.contains("W-P003")).collect();
    assert_eq!(unused.len(), 1, "one unused internal signal: {unused:?}");
    assert!(unused[0].contains("dead"), "flags dead: {unused:?}");
}

#[test]
/// An `if`/`else` mux assigns on both paths, so it is not a latch.
fn if_else_mux_is_not_a_latch() {
    // A signal assigned in both the `if` and the `else` is fully covered —
    // no possible-latch warning — but one assigned only in the `if` is.
    let covered = lower_diags(
        "module m;\n\
             entity M { c: Bit in, a: unsigned[8] in, b: unsigned[8] in, y: unsigned[8] out }\n\
             impl M { if c { y = a; } else { y = b; } }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let c: Bit; let a: unsigned[8]; let b: unsigned[8]; let y: unsigned[8];\n\
               let dut: M = { .c = c, .a = a, .b = b, .y = y };\n\
             }\n",
    );
    assert!(
        !covered.iter().any(|d| d.contains("inferred latch")),
        "if/else mux wrongly flagged: {covered:?}"
    );

    let latch = lower_diags(
        "module m;\n\
             entity M { c: Bit in, a: unsigned[8] in, y: unsigned[8] out }\n\
             impl M { if c { y = a; } }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let c: Bit; let a: unsigned[8]; let y: unsigned[8];\n\
               let dut: M = { .c = c, .a = a, .y = y };\n\
             }\n",
    );
    assert!(
        latch.iter().any(|d| d.contains("inferred latch")),
        "true latch (no else) should warn: {latch:?}"
    );
}

#[test]
/// Assignment widths are strict: a mismatch is reported rather than
/// silently resized.
fn strict_assignment_width_mismatch() {
    // A parameterized width (`unsigned[W]`) the type checker can't see resolves
    // at elaboration; assigning a 16-bit signal to an 8-bit target is then a
    // width mismatch surfaced by IR lowering.
    let bad = lower_diags(
        "module m;\n\
             entity E { b: unsigned[W] in, y: unsigned[8] out }\n\
             impl E { y = b; }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let b: unsigned[16]; let y: unsigned[8];\n\
               let dut: E<W=16> = { .b = b, .y = y };\n\
             }\n",
    );
    assert!(bad.iter().any(|d| d.contains("width mismatch")), "{bad:?}");

    // A matching-width slice of the same signal is fine — the value width
    // (8) equals the target (8).
    let ok = lower_diags(
        "module m;\n\
             entity E { b: unsigned[W] in, y: unsigned[8] out }\n\
             impl E { y = b[7..0]; }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let b: unsigned[16]; let y: unsigned[8];\n\
               let dut: E<W=16> = { .b = b, .y = y };\n\
             }\n",
    );
    assert!(!ok.iter().any(|d| d.contains("width mismatch")), "{ok:?}");

    // Indexing remains scalar even when the vector itself is one element
    // wide. Retained checker types keep it distinct from the vector.
    let one = lower_diags(
        "module m;\n\
             entity E { b: unsigned[1] in, y: Bit out }\n\
             impl E { y = b[0]; }\n",
    );
    assert!(!one.iter().any(|d| d.contains("width mismatch")), "{one:?}");
}

#[test]
/// A combinational cycle with no register warns (W-P010).
fn combinational_loop_lint() {
    // `t = t + a;` is a zero-delay self-cycle -> flagged; a plain chain
    // (`y = x + 1`) is not.
    let diags = lower_diags(
        "module m;\n\
             entity L { a: unsigned[8] in, y: unsigned[8] out }\n\
             impl L { let t: unsigned[8]; t = t + a; y = t; }\n\
             entity Top {}\n\
             impl Top { let a: unsigned[8]; let y: unsigned[8]; let d: L = { .a = a, .y = y }; }\n",
    );
    let loops: Vec<&String> = diags.iter().filter(|d| d.contains("W-P010")).collect();
    assert!(!loops.is_empty(), "self-cycle flagged: {diags:?}");
    assert!(loops.iter().any(|d| d.contains(".t")), "names t: {loops:?}");

    let ok = lower_diags(
        "module m;\n\
             entity C { x: unsigned[8] in, y: unsigned[8] out }\n\
             impl C { y = x + 1; }\n\
             entity Top {}\n\
             impl Top { let x: unsigned[8]; let y: unsigned[8]; let d: C = { .x = x, .y = y }; }\n",
    );
    assert!(
        !ok.iter().any(|d| d.contains("W-P010")),
        "no false positive: {ok:?}"
    );
}

#[test]
/// A branch that does not assign on every path warns as a possible latch.
fn possible_latch_lint() {
    // `y` is only assigned under a condition (inferred latch); `z` has an
    // unconditional default and must not be flagged.
    let diags = lower_diags(
        "module m;\n\
             entity L { c: Logic in, a: unsigned[8] in, y: unsigned[8] out, z: unsigned[8] out }\n\
             impl L { if c == '1' { y = a; } z = a; }\n\
             entity Top {}\n\
             impl Top { let c: Logic; let a: unsigned[8]; let y: unsigned[8]; let z: unsigned[8];\n\
               let d: L = { .c = c, .a = a, .y = y, .z = z }; }\n",
    );
    let latch: Vec<&String> = diags.iter().filter(|d| d.contains("W-P002")).collect();
    assert_eq!(latch.len(), 1, "exactly one latch warning: {diags:?}");
    assert!(latch[0].contains(".y"), "flags y, not z: {latch:?}");
}

#[test]
/// Enum signals carry their variant symbols, so waveforms show names rather
/// than numbers.
fn enum_signals_carry_symbols() {
    // A Logic-typed signal records its enum type, and the design exports the
    // discriminant -> symbol map (with std's char-variant names) so
    // consumers can print `'X'` instead of `3`.
    let d = lower_src(
        "module m;\n\
             enum Logic { '0', '1', 'Z', 'X' }\n\
             enum State { Idle, Run }\n\
             entity E { a: Logic in, s: State out }\n\
             impl E { s = State::Idle; }\n\
             entity Top {}\n\
             impl Top { let a: Logic; let s: State; let e: E = { .a = a, .s = s }; }\n",
    );
    let sig = |p: &str| d.signals.iter().find(|s| s.path == p).unwrap();
    assert_eq!(sig("Top.e.a").enum_type.as_deref(), Some("Logic"));
    assert_eq!(sig("Top.e.s").enum_type.as_deref(), Some("State"));
    assert_eq!(
        d.enum_syms["Logic"].get(&3).map(String::as_str),
        Some("'X'")
    );
    assert_eq!(
        d.enum_syms["State"].get(&0).map(String::as_str),
        Some("Idle")
    );
}

const COUNTER: &str = "module m;\n\
        entity Counter<W: integer> {\n\
          clk: Bit in,\n\
          rst: Logic in,\n\
          en: Bit in,\n\
          count: unsigned[W] out,\n\
        }\n\
        impl<W: integer> Counter<W> {\n\
          let value: unsigned[W] = 0;\n\
          process update {\n\
            if clk.rising() {\n\
              if rst == '1' {\n\
                value = 0;\n\
              } else if en {\n\
                value = value + 1;\n\
              }\n\
            }\n\
          }\n\
          count = value;\n\
        }\n\
        #[test]\n\
        entity H {}\n\
        impl H {\n\
          let clk: Bit = '0';\n\
          let rst: Logic = '1';\n\
          let en: Bit = '1';\n\
          let count: unsigned[8];\n\
          let dut: Counter<W = 8> = { .clk = clk, .rst = rst, .en = en, .count = count };\n\
        }\n";

#[test]
/// The basic lowering produces the expected signals, driver and event block.
fn lowers_signals_driver_and_event_block() {
    let d = lower_src(COUNTER);
    // Counter signals: clk, rst, en, count, value. The instance's `W = 8`
    // makes the parametric `unsigned[W]` widths concrete.
    let count = d.signals.iter().find(|s| s.path == "H.dut.count").unwrap();
    assert_eq!(count.width, 8);
    assert!(d.signals.iter().any(|s| s.path == "H.dut.value"));
    // One combinational driver: count = value.
    assert_eq!(d.drivers.len(), 1);
    // One event block (clk.rising()) with two next-state updates.
    assert_eq!(d.event_blocks.len(), 1);
    assert_eq!(d.event_blocks[0].updates.len(), 2);
}

#[test]
/// Nested instances lower with their port connections resolved.
fn lowers_nested_instances_with_connections() {
    // Add2 instantiates two Add1s wired through `mid`. Each instance must
    // get its own signals, and every port connection must become a driver.
    let src = "module m;\n\
            entity Add1 { a: unsigned[8] in, y: unsigned[8] out }\n\
            impl Add1 { y = a + 1; }\n\
            entity Add2 { a: unsigned[8] in, y: unsigned[8] out }\n\
            impl Add2 {\n\
              let mid: unsigned[8];\n\
              let s1: Add1 = { .a = a, .y = mid };\n\
              let s2: Add1 = { .a = mid, .y = y };\n\
            }\n\
            #[test] entity T {}\n\
            impl T {\n\
              let a: unsigned[8] = 10;\n\
              let y: unsigned[8];\n\
              let dut: Add2 = { .a = a, .y = y };\n\
            }\n";
    let d = lower_src(src);
    let id = |path: &str| {
        d.signals
            .iter()
            .position(|s| s.path == path)
            .map(|i| SignalId(i as u32))
    };
    // Two distinct Add1 instances, each with its own signals.
    assert!(id("T.dut.s1.a").is_some() && id("T.dut.s1.y").is_some());
    assert!(id("T.dut.s2.a").is_some() && id("T.dut.s2.y").is_some());
    // Every connection is a driver: `in` ports read the parent, `out`
    // ports drive it.
    let wired = |target: &str, source: &str| {
        let (t, s) = (id(target).unwrap(), id(source).unwrap());
        d.drivers
            .iter()
            .any(|dr| dr.target == t && matches!(&dr.expr, Expr::Current(x) if *x == s))
    };
    assert!(wired("T.dut.s1.a", "T.dut.a"), "s1.a <- a");
    assert!(wired("T.dut.mid", "T.dut.s1.y"), "mid <- s1.y");
    assert!(wired("T.dut.s2.a", "T.dut.mid"), "s2.a <- mid");
    assert!(wired("T.dut.y", "T.dut.s2.y"), "y <- s2.y");
}

#[test]
/// An `if` expression lowers to a select rather than a branch.
fn if_expression_lowers_to_select() {
    let d = lower_src(
        "module m;\n\
             entity Mux { sel: Bit in, a: unsigned[8] in, b: unsigned[8] in, y: unsigned[8] out }\n\
             impl Mux { y = if sel { a } else { b }; }\n\
             #[test] entity T {}\n\
             impl T { let sel: Bit; let a: unsigned[8]; let b: unsigned[8]; let y: unsigned[8];\n\
               let dut: Mux = { .sel = sel, .a = a, .b = b, .y = y }; }\n",
    );
    let y = d
        .signals
        .iter()
        .position(|s| s.path == "T.dut.y")
        .map(|i| SignalId(i as u32))
        .unwrap();
    let dr = d.drivers.iter().find(|dr| dr.target == y).unwrap();
    assert!(
        matches!(&dr.expr, Expr::Select { .. }),
        "if-expression must lower to a select"
    );
}

/// An expression that is not a place must be distinguished from supported
/// field/index targets.
#[test]
fn a_bad_assignment_target_says_which_kind_it_is() {
    let not_a_place = lower_diags(
        "module m;
             fn f(x: unsigned[8]) -> unsigned[8] { return x; }
             entity E { a: unsigned[8] in, y: unsigned[8] out }
             impl E { f(a) = a; y = a; }",
    );
    assert!(
        not_a_place
            .iter()
            .any(|d| d.contains("E-P018") && d.contains("`f(a)` cannot be assigned to")),
        "{not_a_place:#?}"
    );
}

/// Nested array indices are independent runtime mux dimensions on reads
/// and a conjunction of match gates on writes.
#[test]
fn chained_runtime_indices_lower_to_muxes_and_gated_writes() {
    let source = "module m;
             entity E {
               a: unsigned[8] in, row: integer in, col: integer in,
               y: unsigned[8] out
             }
             impl E {
               let mm: unsigned[8][2][2];
               mm[row][col] = a;
               y = mm[row][col];
             }";
    let diags = lower_diags(source);
    assert!(
        diags
            .iter()
            .all(|diagnostic| !diagnostic.contains("E-P017")),
        "nested runtime access should no longer be rejected: {diags:#?}"
    );
    let design = lower_src(source);
    assert!(design.validate().is_empty(), "{:#?}", design.validate());
    let matrix_writes: Vec<_> = design
        .drivers
        .iter()
        .filter(|driver| {
            design.signals[driver.target.0 as usize]
                .path
                .contains(".mm[")
        })
        .collect();
    assert_eq!(matrix_writes.len(), 4, "one gated write per scalar leaf");
    assert!(
        matrix_writes.iter().all(|driver| driver.cond.is_some()),
        "every leaf write must test both runtime indices"
    );
    let output = design
        .signals
        .iter()
        .position(|signal| signal.path == "E.y")
        .map(|index| SignalId(index as u32))
        .unwrap();
    let read = design
        .drivers
        .iter()
        .find(|driver| driver.target == output)
        .unwrap();
    assert!(
        matches!(read.expr, Expr::Select { .. }),
        "read is a mux tree"
    );
}

#[test]
/// A runtime index followed by a struct field reaches the right scalar leaf.
fn runtime_index_then_struct_field_reaches_the_scalar_leaf() {
    let source = "module m;
             struct Packet { data: unsigned[8], tag: unsigned[4] }
             entity E {
               a: unsigned[8] in, slot: integer in, y: unsigned[8] out
             }
             impl E {
               let packets: Packet[2];
               packets[slot].data = a;
               y = packets[slot].data;
             }";
    let diagnostics = lower_diags(source);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.contains("E-P017")),
        "{diagnostics:#?}"
    );
    assert!(lower_src(source).validate().is_empty());
}

#[test]
/// Packed vector indices use the declared labels and the corresponding
/// storage offsets, which differ for a descending range.
fn packed_vector_indices_use_declared_labels_and_storage_offsets() {
    let design = lower_src(
        "module m; using std::bits::unsigned; using std::logic::Logic;
             entity E {
               value: unsigned[15..8] in,
               high: Logic out, low: Logic out, whole: unsigned[8] out
             }
             impl E { high = value[15]; low = value[8]; whole = value[15..8]; }",
    );
    let driver = |path: &str| {
        let signal = design
            .signals
            .iter()
            .position(|signal| signal.path == path)
            .map(|index| SignalId(index as u32))
            .unwrap();
        &design
            .drivers
            .iter()
            .find(|driver| driver.target == signal)
            .unwrap()
            .expr
    };
    assert!(matches!(driver("E.high"), Expr::Slice { hi: 7, lo: 7, .. }));
    assert!(matches!(driver("E.low"), Expr::Slice { hi: 0, lo: 0, .. }));
    assert!(matches!(
        driver("E.whole"),
        Expr::Slice { hi: 7, lo: 0, .. }
    ));
    assert!(design.validate().is_empty(), "{:#?}", design.validate());
}

#[test]
/// A runtime packed bit read and write updates both the value plane and the
/// metavalue companion.
fn runtime_packed_bit_read_write_updates_value_and_metavalue_planes() {
    let source = "module m; using std::bits::unsigned; using std::logic::{Bit, Logic};
             entity E {
               clk: Bit in, index: unsigned[5] in, data: Logic in, q: Logic out
             }
             impl E {
               let word: unsigned[15..8] = \"00000000\";
               if clk.rising() { word[index] = data; }
               q = word[index];
             }";
    let diagnostics = lower_diags(source);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.contains("E-P017")),
        "{diagnostics:#?}"
    );
    let design = lower_src(source);
    let word = design
        .signals
        .iter()
        .position(|signal| signal.path == "E.word")
        .unwrap() as u32;
    let meta = *design
        .meta_of
        .get(&word)
        .expect("runtime writes need a companion");
    let event = design.event_blocks.first().expect("clocked bit write");
    assert_eq!(
        event
            .updates
            .iter()
            .filter(|update| update.target.0 == word)
            .count(),
        8,
        "one mutually exclusive value update per declared label"
    );
    assert_eq!(
        event
            .updates
            .iter()
            .filter(|update| update.target.0 == meta)
            .count(),
        8,
        "the metavalue nibble follows every value update"
    );
    let q = design
        .signals
        .iter()
        .position(|signal| signal.path == "E.q")
        .unwrap() as u32;
    let mut reads = Vec::new();
    read_set(
        &design
            .drivers
            .iter()
            .find(|driver| driver.target.0 == q)
            .expect("q driver")
            .expr,
        &mut reads,
    );
    assert!(reads.iter().any(|signal| signal.0 == word));
    assert!(reads.iter().any(|signal| signal.0 == meta));
    assert!(design.validate().is_empty(), "{:#?}", design.validate());
}

/// What reaches the site table, and in what order.
///
/// The two engines cannot disagree on the numbering -- they call this one
/// function -- so reordering or duplicating entries would still line up.
/// What is asserted here is the *contents*: a span only earns an index if
/// some assignment can be blamed through it, which is what leaves index 0
/// free to mean "fall back to the declaration". The order is pinned as the
/// documented shape rather than as a defence against drift.
#[test]
fn range_sites_indexes_only_blamable_assignments() {
    let span = |start: u32| crate::diag::Span::new(crate::diag::FileId(0), start..start + 1);
    let signal = |range: Option<(i64, i64)>| Signal {
        path: "s".into(),
        declaration_span: span(0),
        width: 8,
        real: false,
        integer: true,
        char: false,
        range,
        init: vec![0],
        enum_type: None,
    };
    let driver = |target: u32, at: Option<crate::diag::Span>| Driver {
        target: SignalId(target),
        cond: None,
        expr: Expr::Const(0),
        meta: None,
        ctx: 0,
        span: at,
    };
    let design = Design {
        // 0 is ranged, 1 is not.
        signals: vec![signal(Some((0, 10))), signal(None)],
        drivers: vec![
            driver(0, Some(span(100))),
            // A driver the lowering synthesized -- a port connection has no
            // line of its own, and must not take an index.
            driver(0, None),
            // An unranged target can never fail this way.
            driver(1, Some(span(200))),
            // The same statement lowered twice (one body, two instances)
            // is one site, or the two engines would number differently.
            driver(0, Some(span(100))),
            driver(0, Some(span(300))),
        ],
        event_blocks: vec![EventBlock {
            condition: Expr::Const(1),
            updates: vec![
                NextUpdate {
                    target: SignalId(0),
                    cond: None,
                    expr: Expr::Const(0),
                    meta: None,
                    span: Some(span(400)),
                },
                NextUpdate {
                    target: SignalId(1),
                    cond: None,
                    expr: Expr::Const(0),
                    meta: None,
                    span: Some(span(500)),
                },
            ],
            ctx: 0,
        }],
        ..Design::default()
    };
    let sites = design.range_sites();
    assert_eq!(
        sites,
        vec![span(100), span(300), span(400)],
        "drivers first in lowering order, then event updates"
    );
    // Drivers with no span, and spans on unranged targets, are absent --
    // which is what leaves site 0 free to mean "fall back to the
    // declaration".
    assert!(!sites.contains(&span(200)));
    assert!(!sites.contains(&span(500)));
}

#[test]
/// The storage arena carries the same identity discipline as the process
/// and value arenas: dense self-consistent ids, one declaration per name
/// per root, in-range initializers and bindings, and no repeated binding
/// edge. A `ProcessValueKind::Storage` must name one that exists. Several
/// signals may intentionally share a projection when a testbench value
/// fans out to several DUT ports.
///
/// Worth checking because storage is the one arena reachable without going
/// through a process, so a stale id here would otherwise surface only in a
/// backend.
fn process_storage_identity_is_validated() {
    let span = crate::diag::Span::new(FileId(0), 0..1);
    let storage = |id: u32, name: &str| ProcessStorage {
        id: ProcessStorageId(id),
        owner: crate::elab::InstanceId(0),
        name: name.to_string(),
        source: None,
        span,
        ty: None,
        layout: None,
        initializer: None,
        bindings: Vec::new(),
    };

    let mut ir = ProcessIr {
        storages: vec![storage(0, "a")],
        ..ProcessIr::default()
    };
    assert!(ir.validate(1).is_empty(), "a well-formed arena is accepted");

    // An id that disagrees with its slot.
    ir.storages = vec![storage(3, "a")];
    assert!(ir
        .validate(1)
        .iter()
        .any(|issue| issue.contains("is stored at index 0")));

    // One name declared twice in the same root.
    ir.storages = vec![storage(0, "a"), storage(1, "a")];
    assert!(ir
        .validate(1)
        .iter()
        .any(|issue| issue.contains("declared more than once")));

    // An initializer naming a value that does not exist.
    let mut bad = storage(0, "a");
    bad.initializer = Some(ProcessValueId(7));
    ir.storages = vec![bad];
    assert!(ir
        .validate(1)
        .iter()
        .any(|issue| issue.contains("initializer references invalid value")));

    // A binding to a signal outside the design, and a repeated edge.
    let mut bound = storage(0, "a");
    bound.bindings = vec![
        ProcessStorageBinding {
            projection: String::new(),
            signal: SignalId(9),
            direction: LayoutDirection::In,
        },
        ProcessStorageBinding {
            projection: String::new(),
            signal: SignalId(0),
            direction: LayoutDirection::In,
        },
        ProcessStorageBinding {
            projection: String::new(),
            signal: SignalId(0),
            direction: LayoutDirection::In,
        },
    ];
    ir.storages = vec![bound];
    let issues = ir.validate(1);
    assert!(issues.iter().any(|issue| issue.contains("invalid signal")));
    assert!(issues
        .iter()
        .any(|issue| issue.contains("to SignalId(0) more than once")));

    // A value referencing storage that was never declared.
    ir.storages = Vec::new();
    ir.values = vec![ProcessValue {
        span,
        ty: None,
        bit_width: None,
        kind: ProcessValueKind::Storage(ProcessStorageId(0)),
    }];
    assert!(ir
        .validate(1)
        .iter()
        .any(|issue| issue.contains("references invalid storage")));

    // A scalar width must always be usable as an LLVM integer width.
    ir.values[0].bit_width = Some(0);
    assert!(ir
        .validate(1)
        .iter()
        .any(|issue| issue.contains("zero packed width")));
}

#[test]
/// Validation accepts well-formed IR and flags each malformed shape.
fn validate_accepts_good_and_flags_bad_ir() {
    // A lowered counter is well-formed.
    assert!(lower_src(COUNTER).validate().is_empty());

    let sig = |w: u32| Signal {
        path: "s".into(),
        declaration_span: crate::diag::Span::new(crate::diag::FileId(0), 0..0),
        width: w,
        real: false,
        integer: false,
        char: false,
        range: None,
        init: vec![0],
        enum_type: None,
    };
    // Out-of-range signal id, an Unknown, a bad slice, and a width-0 signal.
    let bad = Design {
        signals: vec![sig(0)], // width 0 -> flagged
        drivers: vec![Driver {
            span: None,
            target: SignalId(9), // out of range
            cond: Some(Expr::Unknown),
            expr: Expr::Slice {
                base: Box::new(Expr::Current(SignalId(0))),
                hi: 1,
                lo: 3,
            },
            meta: None,
            ctx: 0,
        }],
        event_blocks: vec![],
        process_ir: Default::default(),
        process_labels: Default::default(),
        resolved_process_labels: Default::default(),
        enum_bases: HashMap::new(),
        enum_syms: HashMap::new(),
        new_defaults: Default::default(),
        logic_encodings: Default::default(),
        lookup_tables: Default::default(),
        base_dir: Default::default(),
        meta_of: Default::default(),
        metavalue_temps: Default::default(),
        array_element_enums: Default::default(),
        array_element_of_family: Default::default(),
        source_layouts: Default::default(),
    };
    let issues = bad.validate();
    assert!(
        issues.iter().any(|i| i.contains("unknown width")),
        "{issues:?}"
    );
    assert!(
        issues.iter().any(|i| i.contains("out of range")),
        "{issues:?}"
    );
    assert!(issues.iter().any(|i| i.contains("Unknown")), "{issues:?}");
    assert!(
        issues.iter().any(|i| i.contains("slice bounds")),
        "{issues:?}"
    );
    // Each issue names the signal it concerns. A driver's index in the
    // vector is an artefact of lowering: "driver 0 expr" sent the reader
    // to an IR dump to find out which line of their design it meant.
    assert!(
        issues
            .iter()
            .all(|i| i.contains("`s`") || i.contains("signal id")),
        "every issue names a signal: {issues:?}"
    );
}

#[test]
/// Packed logic tables are interned into compact lookups, with identical
/// tables shared.
fn packed_logic_tables_are_interned_as_compact_lookups() {
    let table = HashMap::from([((0, 0), 1), ((0, 1), 0), ((1, 0), 2), ((1, 1), 3)]);
    let lookup =
        |left, right| logic_binary_table_result(Expr::Current(left), Expr::Current(right), &table);
    let mut design = Design {
        drivers: vec![
            Driver {
                target: SignalId(2),
                cond: None,
                expr: lookup(SignalId(0), SignalId(1)),
                meta: None,
                ctx: 0,
                span: None,
            },
            Driver {
                target: SignalId(3),
                cond: None,
                expr: lookup(SignalId(1), SignalId(0)),
                meta: None,
                ctx: 0,
                span: None,
            },
        ],
        ..Design::default()
    };

    compact_lookup_tables(&mut design);

    assert_eq!(design.lookup_tables.len(), 1);
    assert_eq!(design.lookup_tables[0].element_width, 4);
    assert_eq!(design.lookup_tables[0].values, vec![1, 0, 2, 3]);
    for driver in &design.drivers {
        assert!(matches!(
            driver.expr,
            Expr::TableLookup {
                table: LookupTableId(0),
                ..
            }
        ));
    }
}

#[test]
/// The persisted leaf layout, not the source type, is what the backend
/// treats as authoritative for width.
fn persisted_leaf_layout_is_the_backend_width_authority() {
    let mut design = lower_src("module m; entity E { value: unsigned[8] in } impl E {}");
    let id = SignalId(
        design
            .signals
            .iter()
            .position(|signal| signal.path == "E.value")
            .expect("value signal") as u32,
    );
    assert_eq!(design.signal_width(id), Some(8));

    design.signals[id.0 as usize].width = 9;
    assert_eq!(
        design.signal_width(id),
        Some(8),
        "the persisted source layout, not duplicated Signal metadata, owns backend width"
    );
    let issues = design.validate();
    assert!(
        issues
            .iter()
            .any(|issue| issue.contains("disagrees with its source layout width 8")),
        "{issues:#?}"
    );
}

#[test]
/// Scheduled processes carry their sensitivity and write sets.
fn processes_carry_sensitivity_and_write_sets() {
    let d = lower_src(COUNTER);
    let sig = |path: &str| SignalId(d.signals.iter().position(|s| s.path == path).unwrap() as u32);
    let procs = d.processes();
    // A combinational process for `count = value` and one event process.
    let comb = procs
            .iter()
            .find(|p| matches!(&p.kind, ProcessKind::Comb { target, .. } if *target == sig("H.dut.count")))
            .unwrap();
    assert_eq!(comb.reads, vec![sig("H.dut.value")]);
    assert_eq!(comb.writes, vec![sig("H.dut.count")]);

    let event = procs
        .iter()
        .find(|p| matches!(p.kind, ProcessKind::Event { .. }))
        .unwrap();
    assert_eq!(event.labels, vec!["H.dut::update"]);
    // Sensitive to clk (edge condition), rst and en (update guards),
    // value (increment). Writes value.
    for s in ["H.dut.clk", "H.dut.rst", "H.dut.en", "H.dut.value"] {
        assert!(event.reads.contains(&sig(s)), "event not sensitive to {s}");
    }
    assert_eq!(event.writes, vec![sig("H.dut.value")]);
}

#[test]
/// A resolved process keeps every contributing driver's label, so a
/// diagnostic can name all of them.
fn resolved_process_keeps_every_contributing_label() {
    let d = lower_src(
        "module m;\n\
             impl<T: Resolve> Resolve for T[] {\n\
               fn resolve(self, rhs: T[]) -> T[] { return self; }\n\
             }\n\
             impl Resolve for Logic {\n\
               fn resolve(self, rhs: Logic) -> Logic {\n\
                 if self == 'Z' { return rhs; }\n\
                 return self;\n\
               }\n\
             }\n\
             entity E { q: unsigned[8] out }\n\
             impl E {\n\
               process bit_one { q[1] = '1'; }\n\
               process bit_three { q[3] = '1'; }\n\
             }\n",
    );
    let target = SignalId(
        d.signals
            .iter()
            .position(|signal| signal.path == "E.q")
            .unwrap() as u32,
    );
    let process = d
        .processes()
        .into_iter()
        .find(|process| process.writes == [target])
        .unwrap();
    assert_eq!(process.labels, ["E::bit_one", "E::bit_three"]);
    let rendered = d.to_ir_string();
    assert!(
        rendered.contains("[E::bit_one, E::bit_three]"),
        "{rendered}"
    );
}

#[test]
/// Composite and enum signals flatten into leaves with correct widths.
fn composite_and_enum_signals_flatten_with_widths() {
    let d = lower_src(
            "module m;\n\
             enum S { A, B, C }\n\
             struct P { flag: Bit, val: unsigned[8] }\n\
             entity E { p: P in, a: Bit[3] in, s: S out }\n\
             impl E {}\n\
             entity H {}\n\
             impl H { let p: P; let a: Bit[3]; let s: S; let dut: E = { .p = p, .a = a, .s = s }; }\n",
        );
    let width = |path: &str| d.signals.iter().find(|x| x.path == path).map(|x| x.width);
    assert_eq!(width("H.dut.p.flag"), Some(1)); // struct field
    assert_eq!(width("H.dut.p.val"), Some(8));
    assert_eq!(width("H.dut.a[0]"), Some(1)); // array element
    assert_eq!(width("H.dut.a[2]"), Some(1));
    assert_eq!(width("H.dut.s"), Some(2)); // enum repr width
}

#[test]
/// A partial bit-slice write updates only the addressed bits.
fn partial_bit_slice_write() {
    // `y = 0; y[3..0] = a` merges: low nibble = a, high bits held from 0.
    let d = lower_src(
        "module m;\n\
             entity E { a: unsigned[4] in, y: unsigned[8] out }\n\
             impl E { process { y = 0; y[3..0] = a; } }\n\
             entity H {}\n\
             impl H { let a: unsigned[4]; let y: unsigned[8]; let dut: E = { .a = a, .y = y }; }\n",
    );
    // The y driver should be a read-modify-write (an Or of a masked base
    // and a shifted value), not a bare assignment.
    let dr = d
        .drivers
        .iter()
        .find(|dr| d.signals[dr.target.0 as usize].path == "H.dut.y")
        .unwrap();
    assert!(
        matches!(dr.expr, Expr::Binary { op: BinOp::Or, .. }),
        "slice write merges: {:?}",
        dr.expr
    );
}

#[test]
/// Concurrent resolved slices lower without the expression growth that
/// resolution folding used to produce.
fn concurrent_resolved_slices_lower_without_expression_explosion() {
    // Each bare assignment is its own concurrent driver context. Folding
    // three contexts used to repeatedly inline and clone Logic::resolve:
    // two produced about 500 KiB of IR and the third exhausted the host.
    let d = lower_src(
        "module m;\n\
             impl<T: Resolve> Resolve for T[] {\n\
               fn resolve(self, rhs: T[]) -> T[] { return self; }\n\
             }\n\
             impl Resolve for Logic {\n\
               fn resolve(self, rhs: Logic) -> Logic {\n\
                 if self == 'Z' { return rhs; }\n\
                 if rhs == 'Z' { return self; }\n\
                 if self == rhs { return self; }\n\
                 return 'X';\n\
               }\n\
             }\n\
             entity E { q: unsigned[8] out }\n\
             impl E { q[1] = '1'; q[3] = '1'; q[5] = '1'; }\n",
    );
    assert!(d.validate().is_empty());
    let rendered = d.to_ir_string();
    assert!(
        rendered.len() < 250_000,
        "three resolved slice contexts expanded to {} bytes",
        rendered.len()
    );
}

/// A derived enum width has to hold every *value*, not one code per
/// variant. `Hi = 9` in a two-variant enum needs four bits; counting
/// variants alone gave one, silently truncating the value to 1. The old
/// `: unsigned[4]` repr annotation used to paper over this.
#[test]
fn enum_width_covers_explicit_discriminants() {
    let d = lower_src(
        "module m;\n\
             enum Code { Lo = 1, Hi = 9 }\n\
             entity E { c: Code out }\n\
             impl E { c = Code::Hi; }\n\
             entity H {}\n\
             impl H { let c: Code; let dut: E = { .c = c }; }\n",
    );
    let sig = d.signals.iter().find(|s| s.path == "H.dut.c").unwrap();
    assert_eq!(sig.width, 4, "width must hold the largest discriminant");
}

#[test]
/// A newtype enum takes its base's width.
fn newtype_enum_takes_its_base_width() {
    // A derived enum is a newtype (§3.28) — same variants, so the same
    // width. Four variants in the base, two bits in the derived type.
    let d = lower_src(
        "module m;\n\
             enum Base { A, B, C, D }\n\
             enum Ext(Base);\n\
             entity E { x: Ext out }\n\
             impl E { x = Ext::A; }\n\
             entity H {}\n\
             impl H { let x: Ext; let dut: E = { .x = x }; }\n",
    );
    let sig = d.signals.iter().find(|s| s.path == "H.dut.x").unwrap();
    assert_eq!(sig.width, 2, "the newtype carries the base's width");
}

/// `{ ..base, .z = 7 }` copied one value per *top-level* field, so a field
/// holding a struct read as a scalar (Unknown) and every leaf under it was
/// silently dropped — the spread carried over nothing nested.
/// A match naming every variant needs no `_`, but the Select chain still
/// bottomed out in `Unknown` — so the exhaustive spelling, the one the
/// non-exhaustive lint asks for, produced a design no engine would run.
#[test]
fn exhaustive_match_expression_needs_no_wildcard() {
    let d = lower_src(
        "module m;\n\
             enum Base { A, B, C }\n\
             entity E { sel: Base in, y: unsigned[8] out }\n\
             impl E { y = match sel { Base::A => 1, Base::B => 2, Base::C => 3 }; }\n\
             entity H {}\n\
             impl H { let sel: Base; let y: unsigned[8]; let e: E = { .sel = sel, .y = y }; }",
    );
    /// Whether an expression contains an `Unknown`, which validation rejects.
    fn has_unknown(e: &Expr) -> bool {
        match e {
            Expr::Unknown => true,
            Expr::Select { cond, then, els } => {
                has_unknown(cond) || has_unknown(then) || has_unknown(els)
            }
            Expr::Binary { lhs, rhs, .. } => has_unknown(lhs) || has_unknown(rhs),
            Expr::Unary { rhs, .. } => has_unknown(rhs),
            _ => false,
        }
    }
    let dr = d
        .drivers
        .iter()
        .find(|dr| d.signals[dr.target.0 as usize].path.ends_with(".y"))
        .expect("driver for y");
    assert!(!has_unknown(&dr.expr), "no Unknown left: {:?}", dr.expr);
}

/// `Counter<W = 8>` bound its parameter but `Counter<8>` silently did not:
/// only the named form was read, so the positional one left the parameter
/// unbound and every port kept width 0 — surfacing far downstream as
/// "signal has unknown width (0)" with nothing pointing at the instance.
/// A parameter carried its argument's *family* into the body but not its
/// width, so a nested inline — `signed`'s Ord inside `abs` — read
/// `self'length` as 1 and tested the sign bit with `>> 0`. `abs(-5)`
/// returned 251.
/// A call result is a value of its declared return type, so it wraps to
/// that width. Left unmasked, `neg(x)` on a `signed[8]`-returning function
/// carried a full-width `0 - x`, and a nested inline then tested the wrong
/// bit for the sign. Assigning to a signal masked it anyway, which hid it.
///
/// The operator dispatch that goes with this needs std's `<=>` impl, which
/// this harness's minimal prelude does not have; `fn_return_type_test` in
/// the corpus covers that end.
#[test]
fn a_call_result_carries_its_return_type() {
    let d = lower_src(
        "module m;\n\
             fn neg(v: signed[8]) -> signed[8] { return 0 - v; }\n\
             entity E { s: signed[8] in, lt: unsigned[8] out }\n\
             impl E { lt = if neg(s) < 0 { 1 } else { 0 }; }\n\
             entity H {}\n\
             impl H { let s: signed[8]; let lt: unsigned[8]; \
             let e: E = { .s = s, .lt = lt }; }",
    );
    let text = d.to_ir_string();
    assert!(
        text.contains("and 255"),
        "masked to the return width:\n{text}"
    );
}

#[test]
/// An inlined parameter keeps its argument's width rather than the
/// parameter's declared one.
fn an_inlined_parameter_keeps_its_argument_width() {
    let d = lower_src(
        "module m;\n\
             fn width_of(v: integer) -> integer { return v'length; }\n\
             entity E { s: signed[8] in, y: unsigned[8] out }\n\
             impl E { y = width_of(s); }\n\
             entity H {}\n\
             impl H { let s: signed[8]; let y: unsigned[8]; let e: E = { .s = s, .y = y }; }",
    );
    let dr = d
        .drivers
        .iter()
        .find(|dr| d.signals[dr.target.0 as usize].path.ends_with(".y"))
        .expect("driver for y");
    assert!(
        matches!(dr.expr, Expr::Const(8)),
        "the parameter should report the argument's width, got {:?}",
        dr.expr
    );
}

#[test]
/// A positional generic argument binds a value parameter.
fn positional_generic_argument_binds_a_value_parameter() {
    let src = |arg: &str| {
        format!(
            "module m;\n\
                 entity Inc<W: integer> {{ a: unsigned[W] in, y: unsigned[W] out, }}\n\
                 impl<W: integer> Inc<W> {{ y = a + 1; }}\n\
                 entity H {{}}\n\
                 impl H {{ let a: unsigned[4]; let y: unsigned[4]; \
                 let i: Inc<{arg}> = {{ .a = a, .y = y }}; }}"
        )
    };
    for arg in ["W = 4", "4"] {
        let d = lower_src(&src(arg));
        let w = d
            .signals
            .iter()
            .find(|s| s.path.ends_with("i.a"))
            .map(|s| s.width);
        assert_eq!(w, Some(4), "`Inc<{arg}>` should bind W");
    }
}

#[test]
/// A struct spread copies nested leaves, not just top-level fields.
fn spread_copies_nested_leaves() {
    let d = lower_src(
        "module m;\n\
             struct A { x: Bit, y: unsigned[4] }\n\
             struct B { a: A, z: unsigned[4] }\n\
             entity E { oy: unsigned[4] out }\n\
             impl E {\n\
               let base: B;\n\
               base = B { .a = A { .x = '1', .y = 9 }, .z = 2 };\n\
               let upd: B;\n\
               upd = B { ..base, .z = 7 };\n\
               oy = upd.a.y;\n\
             }\n\
             entity H {}\n\
             impl H { let oy: unsigned[4]; let e: E = { .oy = oy }; }",
    );
    let driven = |suffix: &str| {
        d.signals
            .iter()
            .position(|s| s.path.ends_with(suffix))
            .and_then(|i| d.drivers.iter().find(|dr| dr.target.0 as usize == i))
            .is_some()
    };
    assert!(driven(".upd.a.y"), "the spread must carry nested leaves");
    assert!(driven(".upd.a.x"), "every leaf, not just the read one");
    assert!(driven(".upd.z"), "and the explicit override");
}

#[test]
/// A composed struct flattens its nested fields.
fn composed_struct_flattens_nested_fields() {
    // Composition replaced extension (§3.28): a struct field holding
    // another struct flattens to dotted leaf signals.
    let d = lower_src(
        "module m;\n\
             struct Header { valid: Bit, kind: unsigned[4] }\n\
             struct Packet { header: Header, data: unsigned[8] }\n\
             entity E { p: Packet out }\n\
             impl E {}\n\
             entity H {}\n\
             impl H { let p: Packet; let dut: E = { .p = p }; }\n",
    );
    let width = |path: &str| d.signals.iter().find(|x| x.path == path).map(|x| x.width);
    assert_eq!(width("H.dut.p.header.valid"), Some(1), "nested field");
    assert_eq!(width("H.dut.p.header.kind"), Some(4), "nested field");
    assert_eq!(width("H.dut.p.data"), Some(8), "own field");
}

#[test]
/// A derivation that adds no variants is representation-identical to its
/// base.
fn same_variant_enum_derivation_is_representation_identical() {
    // A bodyless derivation keeps the base's width and discriminants.
    let d = lower_src(
        "module m;\n\
             enum Base { A, B, C }\n\
             enum Alias(Base);\n\
             entity E { x: Alias out }\n\
             impl E { x = Alias::B; }\n\
             entity H {}\n\
             impl H { let x: Alias; let dut: E = { .x = x }; }\n",
    );
    let sig = d.signals.iter().find(|s| s.path == "H.dut.x").unwrap();
    assert_eq!(sig.width, 2, "3 variants -> 2 bits, same as base");
}

#[test]
/// A bit string decodes across the full nine-value set.
fn bit_string_decodes_nine_value() {
    // A plain 2-value string is unchanged; a metavalue digit takes its
    // source-defined `LogicEncoding::to_bool` bit rather than a bit of the
    // enum discriminant. X normalizes to a low placeholder in the value
    // plane while the companion retains its exact identity.
    let d = lower_src(
        "module m; entity E { y: unsigned[4] out, z: unsigned[4] out, }\n\
             impl E { y = \"1010\"; z = \"1X10\"; }\n\
             entity T {}\n\
             impl T { let y: unsigned[4]; let z: unsigned[4]; let dut: E = { .y = y, .z = z }; }",
    );
    let s = d.to_ir_string();
    assert!(s.contains("driver T.dut.y = 10"), "2-value unchanged:\n{s}");
    assert!(
        s.contains("driver T.dut.z = 10"),
        "metavalue digit decodes:\n{s}"
    );
}

#[test]
/// A bit-string initializer sets the signal's init pattern.
fn bit_string_initializer_sets_init() {
    // `let v: unsigned[4] = "1010"` seeds the signal init to 10 (was 0 — no
    // string-init arm in const_init_value).
    let d = lower_src(
        "module m; entity E { y: unsigned[4] out, }\n\
             impl E { let v: unsigned[4] = \"1010\"; y = v; }\n\
             entity T {}\n\
             impl T { let y: unsigned[4]; let dut: E = { .y = y }; }",
    );
    let v = d
        .signals
        .iter()
        .find(|s| s.path.ends_with(".v"))
        .expect("no .v");
    assert_eq!(v.init, vec![10], "b\"1010\" -> init 10");
}

#[test]
/// A bit string containing a metavalue creates the companion plane.
fn metavalue_bit_string_creates_companion() {
    // A metavalue init spawns a `$meta` companion recording the X element;
    // a plain 2-value init does not.
    let d = lower_src(
            "module m; entity E { y: unsigned[4] out, z: unsigned[4] out, }\n\
             impl E { let v: unsigned[4] = \"1X10\"; let w: unsigned[4] = \"1010\"; y = v; z = w; }\n\
             entity T {}\n\
             impl T { let y: unsigned[4]; let z: unsigned[4]; let dut: E = { .y = y, .z = z }; }",
        );
    let v = d
        .signals
        .iter()
        .position(|s| s.path.ends_with(".v"))
        .expect("v") as u32;
    let w = d
        .signals
        .iter()
        .position(|s| s.path.ends_with(".w"))
        .expect("w") as u32;
    let cid = *d.meta_of.get(&v).expect("v has a metavalue companion");
    // "1X10": per-element discs, nibble i = element i. pos3=1, pos2=X(3),
    // pos1=1, pos0=0 -> 0x1310. Companion is 4 bits/element wide.
    assert_eq!(
        d.signals[cid as usize].init,
        vec![0x1310],
        "full per-element discs"
    );
    assert_eq!(d.signals[cid as usize].width, 16, "4 bits x 4 elements");
    assert!(d.signals[cid as usize].path.ends_with(".v$meta"));
    assert!(!d.meta_of.contains_key(&w), "clean init gets no companion");
}

#[test]
/// A resolved metavalue companion is terminal: companions never gain
/// companions of their own, which is what bounded the `$meta$meta` chain.
fn resolved_metavalue_companion_is_terminal() {
    // Element-wise resolution builds the discriminant plane from both the
    // value and metavalue planes.  That expression must not make the
    // propagation fixed point infer a companion for the companion itself.
    let d = lower_src(
        "module m;\n\
             impl<T: Resolve> Resolve for T[] {\n\
                 fn resolve(self, rhs: T[]) -> T[] { return self; }\n\
             }\n\
             impl Resolve for Logic {\n\
                 fn resolve(self, rhs: Logic) -> Logic {\n\
                     if self == 'Z' { return rhs; }\n\
                     return self;\n\
                 }\n\
             }\n\
             entity Driver { y: unsigned[2] out }\n\
             impl Driver { y = \"X0\"; }\n\
             impl Driver { y = \"01\"; }\n\
             entity T {}\n\
             impl T { let y: unsigned[2]; let d: Driver = { .y = y }; }",
    );

    assert!(
        d.signals
            .iter()
            .any(|signal| signal.path.ends_with("$meta")),
        "the resolved vector still needs one discriminant plane"
    );
    assert!(
        d.signals
            .iter()
            .all(|signal| !signal.path.contains("$meta$meta")),
        "a discriminant plane must never acquire its own companion"
    );
    assert!(
        d.meta_of
            .values()
            .all(|companion| !d.meta_of.contains_key(companion)),
        "the companion relation must have depth one"
    );
}

#[test]
/// Wide metavalue initializers have no element ceiling.
fn wide_metavalue_initializers_have_no_element_limit() {
    let d = lower_src(
        "module m;\n\
             entity T {}\n\
             impl T { let v: unsigned[17] = \"X0000000000000000\"; }\n",
    );
    let v = d
        .signals
        .iter()
        .position(|s| s.path.ends_with(".v"))
        .expect("v signal");
    let cid = *d.meta_of.get(&(v as u32)).expect("wide companion");
    assert_eq!(d.signals[cid as usize].width, 68);
    assert_eq!(
        d.signals[cid as usize].init,
        vec![0, 3],
        "the top element's discriminant crosses the first ABI word"
    );

    let driven = lower_src(
        "module m;\n\
             entity E { v: unsigned[17] out, }\n\
             impl E { v = \"X0000000000000000\"; }\n",
    );
    let cid = *driven.meta_of.values().next().expect("driven companion");
    assert!(driven.drivers.iter().any(|driver| {
        driver.target == SignalId(cid)
            && matches!(&driver.expr, Expr::WideConst(words) if words == &[0, 3])
    }));
}

#[test]
/// A later clean combinational write clears the companion in order, so a
/// stale `X` does not survive in the discriminant plane.
fn clean_combinational_override_clears_metavalue_companion_in_order() {
    let design = lower_src(
        "module m;\n\
             entity E { clear: Bit in, y: unsigned[4] out, }\n\
             impl E {\n\
                 let dirty: unsigned[4] = \"X000\";\n\
                 y = dirty;\n\
                 if clear { y = \"0000\"; }\n\
             }\n",
    );
    let y = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with(".y"))
        .expect("y") as u32;
    let companion = *design
        .meta_of
        .get(&y)
        .unwrap_or_else(|| panic!("y companion missing:\n{}", design.to_ir_string()));
    let value_drivers: Vec<_> = design
        .drivers
        .iter()
        .filter(|driver| driver.target == SignalId(y))
        .collect();
    let meta_drivers: Vec<_> = design
        .drivers
        .iter()
        .filter(|driver| driver.target == SignalId(companion))
        .collect();
    assert_eq!(value_drivers.len(), 2);
    assert_eq!(meta_drivers.len(), 2, "one companion write per value write");
    assert!(meta_drivers[0].cond.is_none());
    assert!(meta_drivers[1].cond.is_some());
    assert!(matches!(meta_drivers[1].expr, Expr::Const(0)));
}

#[test]
/// The same ordering holds for a clocked override.
fn clean_clocked_override_clears_metavalue_companion_in_order() {
    let design = lower_src(
        "module m;\n\
             entity E { clk: Bit in, clear: Bit in, y: unsigned[4] out, }\n\
             impl E {\n\
                 let dirty: unsigned[4] = \"X000\";\n\
                 if clk.rising() {\n\
                     y = dirty;\n\
                     if clear { y = \"0000\"; }\n\
                 }\n\
             }\n",
    );
    let y = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with(".y"))
        .expect("y") as u32;
    let companion = *design.meta_of.get(&y).expect("y companion");
    let block = design.event_blocks.first().expect("clocked block");
    let value_updates: Vec<_> = block
        .updates
        .iter()
        .filter(|update| update.target == SignalId(y))
        .collect();
    let meta_updates: Vec<_> = block
        .updates
        .iter()
        .filter(|update| update.target == SignalId(companion))
        .collect();
    assert_eq!(value_updates.len(), 2);
    assert_eq!(
        meta_updates.len(),
        2,
        "one companion update per value update"
    );
    assert!(meta_updates[0].cond.is_none());
    assert!(meta_updates[1].cond.is_some());
    assert!(matches!(meta_updates[1].expr, Expr::Const(0)));
}

#[test]
/// An `'old` vector read uses the companion's `'old` value, not its current
/// one.
fn old_vector_read_uses_old_metavalue_companion() {
    let design = lower_src(
        "module m;\n\
             entity E { y: unsigned[4] out, }\n\
             impl E {\n\
                 let v: unsigned[4] = \"X000\";\n\
                 y = v'old;\n\
             }\n",
    );
    let y = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with(".y"))
        .expect("y") as u32;
    let companion = *design.meta_of.get(&y).expect("y companion");
    let meta_driver = design
        .drivers
        .iter()
        .find(|driver| driver.target == SignalId(companion))
        .expect("companion driver");
    assert!(matches!(meta_driver.expr, Expr::Old(_)));
}

#[test]
/// Narrowed arithmetic still scans the full operand for metavalues, so an
/// unknown outside the narrowed range still poisons the result.
fn narrowed_arithmetic_scans_full_operand_for_metavalues() {
    let design = lower_src(
        "module m;\n\
             entity E { y: unsigned[4] out, }\n\
             impl E {\n\
                 let dirty: unsigned[8] = \"X0000000\";\n\
                 let zero: unsigned[8] = 0;\n\
                 y = (dirty + zero)[3..0];\n\
             }\n",
    );
    let y = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with(".y"))
        .expect("y") as u32;
    let companion = *design
        .meta_of
        .get(&y)
        .unwrap_or_else(|| panic!("y companion missing:\n{}", design.to_ir_string()));
    let rendered = render(
        &design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(companion))
            .expect("companion driver")
            .expr,
        &design,
    );
    assert!(
        rendered.contains("[31..28]"),
        "the poison predicate must inspect element 7 of the 8-element operand: {rendered}"
    );
}

/// Constant evaluation is best-effort and must never let host integer
/// overflow abort the compiler. The exact expression remains available to
/// arbitrary-width lowering even when it does not fit the narrow
/// elaboration evaluator.
#[test]
fn overflowing_narrow_constant_evaluation_does_not_panic() {
    let design = lower_src(
        "module m;\n\
             const FAR: integer = 1 << 64;\n\
             entity E { y: unsigned[128] out, }\n\
             impl E { y = FAR; }\n",
    );
    assert_eq!(design.drivers.len(), 1);
}

#[test]
/// Kernel-`integer` operations retain signed semantics through lowering.
fn kernel_integer_operations_retain_signed_semantics() {
    let design = lower_src(
        "module m;\n\
             entity E {\n\
                 a: integer<-16..15> in,\n\
                 b: integer<-16..15> in,\n\
                 lt: Bit out,\n\
                 q: integer<-16..15> out,\n\
                 shr: integer<-16..15> out,\n\
             }\n\
             impl E {\n\
                 lt = if a < b { '1' } else { '0' };\n\
                 q = a / b;\n\
                 shr = a >> 1;\n\
             }\n",
    );
    let driver = |suffix: &str| {
        let target = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(suffix))
            .expect("target signal");
        &design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(target as u32))
            .expect("target driver")
            .expr
    };
    assert!(matches!(
        driver(".lt"),
        Expr::Select { cond, .. }
            if matches!(cond.as_ref(), Expr::Binary { op: BinOp::SLt, .. })
    ));
    assert!(matches!(
        driver(".q"),
        Expr::Binary {
            op: BinOp::SDiv,
            ..
        }
    ));
    assert!(matches!(
        driver(".shr"),
        Expr::Binary {
            op: BinOp::AShr,
            ..
        }
    ));
}

#[test]
/// A real-to-integer conversion is signed in direct comparisons.
fn real_to_integer_conversion_is_signed_in_direct_comparisons() {
    let design = lower_src(
        "module m;\n\
             entity E {\n\
                 r: real in,\n\
                 lt: Bit out,\n\
             }\n\
             impl E {\n\
                 lt = if integer(r) < 0 { '1' } else { '0' };\n\
             }\n",
    );
    let lt = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with(".lt"))
        .expect("lt signal");
    let expr = &design
        .drivers
        .iter()
        .find(|driver| driver.target == SignalId(lt as u32))
        .expect("lt driver")
        .expr;
    assert!(matches!(
        expr,
        Expr::Select { cond, .. }
            if matches!(
                cond.as_ref(),
                Expr::Binary {
                    op: BinOp::SLt,
                    lhs,
                    ..
                } if matches!(lhs.as_ref(), Expr::Unary { op: UnOp::RealToInt, .. })
            )
    ));
}

#[test]
/// Assigning to a ranged integer may change the storage width.
fn ranged_integer_assignment_can_change_storage_width() {
    let src = "module m;\n\
             entity E {\n\
                 narrow: integer<-16..15> in,\n\
                 wide: integer<-128..127> out,\n\
             }\n\
             impl E { wide = narrow; }\n";
    let diagnostics = lower_diags(src);
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("width mismatch")),
        "{diagnostics:?}"
    );
    let design = lower_src(src);
    let wide = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with(".wide"))
        .expect("wide integer signal");
    assert!(matches!(
        design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(wide as u32))
            .map(|driver| &driver.expr),
        Some(Expr::Current(_))
    ));
}

#[test]
/// A chain of aliases retains the terminal signal representation.
fn chained_aliases_retain_terminal_signal_representation() {
    let design = lower_src(
        "module m;\n\
             using Small = integer<-16..15>;\n\
             using Alias = Small;\n\
             using Chars = Char[];\n\
             using Text = Chars;\n\
             entity E { x: Alias in, y: Alias out, }\n\
             impl E { let text: Text[3] = \"abc\"; y = x; }\n",
    );
    for suffix in [".x", ".y"] {
        let signal = design
            .signals
            .iter()
            .find(|signal| signal.path.ends_with(suffix))
            .expect("aliased signal");
        assert_eq!(signal.width, 5);
        assert!(signal.integer);
        assert_eq!(signal.range, Some((-16, 15)));
    }
    let text: Vec<_> = design
        .signals
        .iter()
        .filter(|signal| signal.path.contains(".text["))
        .collect();
    assert_eq!(text.len(), 3);
    assert!(text.iter().all(|signal| signal.char));
}

#[test]
/// Foreign integer calls retain their signed ABI types.
fn foreign_integer_calls_retain_signed_abi_types() {
    let design = lower_src(
        "module m;\n\
             using CWord = integer;\n\
             using CInteger = CWord;\n\
             extern \"C\" { pub fn labs(v: CInteger) -> CInteger; }\n\
             entity E {\n\
                 x: integer<-128..127> in,\n\
                 y: integer<-128..127> out,\n\
             }\n\
             impl E { y = labs(x); }\n",
    );
    assert!(matches!(
        design.drivers.first().map(|driver| &driver.expr),
        Some(Expr::CCall {
            integer_args,
            integer_ret: true,
            ..
        }) if integer_args == &[true]
    ));
}

#[test]
/// A hardware block local does not leak out of its block.
fn a_hardware_block_local_does_not_leak_out_of_its_block() {
    let diagnostics = lower_diags(
        "module m;\n\
             entity E { select: Bit in, y: unsigned[8] out }\n\
             impl E {\n\
                 if select == '1' { let temporary: unsigned[8] = 3; y = temporary; }\n\
                 y = temporary;\n\
             }\n",
    );
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("E-P001")
            && diagnostic.contains("no value named `temporary`")));
    assert!(diagnostics
        .iter()
        .all(|diagnostic| !diagnostic.contains("not lowered to hardware")));
}

#[test]
/// Hardware block locals allocate no signals and leave no `Unknown` in the
/// IR.
fn hardware_block_locals_do_not_allocate_signals_or_leave_unknown_ir() {
    let design = lower_src(
        "module m;\n\
             entity E { select: Bit in, a: unsigned[8] in, y: unsigned[8] out }\n\
             impl E {\n\
                 if select == '1' {\n\
                     let temporary: unsigned[8] = a;\n\
                     temporary = temporary + 1;\n\
                     y = temporary;\n\
                 } else { y = 0; }\n\
             }\n",
    );
    assert!(design
        .signals
        .iter()
        .all(|signal| !signal.path.ends_with(".temporary")));
    assert!(!design.to_ir_string().contains("Unknown"));
}

#[test]
/// Nested runtime access on a block local stays storage-free.
fn nested_runtime_access_on_a_block_local_stays_storage_free() {
    let source = "module m;\n\
             entity E {\n\
                 enable: Bit in, row: integer in, col: integer in,\n\
                 a: unsigned[8] in, y: unsigned[8] out\n\
             }\n\
             impl E {\n\
                 if enable == '1' {\n\
                     let matrix: unsigned[8][2][2] = [[1, 2], [3, 4]];\n\
                     matrix[row][col] = a;\n\
                     y = matrix[row][col];\n\
                 } else { y = 0; }\n\
             }\n";
    let diagnostics = lower_diags(source);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.contains("E-P017")),
        "{diagnostics:#?}"
    );
    let design = lower_src(source);
    assert!(design
        .signals
        .iter()
        .all(|signal| !signal.path.contains("matrix")));
    assert!(!design.to_ir_string().contains("Unknown"));
    assert!(design.validate().is_empty(), "{:#?}", design.validate());
}

#[test]
/// A runtime packed index on a block local stays storage-free.
fn runtime_packed_index_on_a_block_local_stays_storage_free() {
    let source = "module m; using std::bits::unsigned; using std::logic::{Bit, Logic};
             entity E {
               enable: Bit in, index: unsigned[5] in, y: Logic out
             }
             impl E {
               if enable == '1' {
                 let word: unsigned[15..8] = 0;
                 word[index] = '1';
                 y = word[index];
               } else { y = '0'; }
             }";
    let diagnostics = lower_diags(source);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.contains("E-P017")),
        "{diagnostics:#?}"
    );
    let design = lower_src(source);
    assert!(design
        .signals
        .iter()
        .all(|signal| !signal.path.ends_with(".word")));
    assert!(!design.to_ir_string().contains("Unknown"));
    assert!(design.validate().is_empty(), "{:#?}", design.validate());
}

#[test]
/// Nested generic type arguments preserve the recursive layout.
fn nested_generic_type_arguments_preserve_recursive_layout() {
    let design = lower_src(
        "module m;\n\
             struct Box<T> { value: T }\n\
             struct Pair<T, U> { left: T, right: U }\n\
             entity Pass<T> { input: T in, output: T out }\n\
             impl<T> Pass<T> { output = input; }\n\
             entity E {\n\
                 nested: Box<Box<unsigned[8]>> in,\n\
                 named: Pair<U = Box<unsigned[16]>, T = Box<Box<unsigned[8]>>> in,\n\
                 y: unsigned[8] out,\n\
             }\n\
             impl E {\n\
                 let passed: Box<Box<unsigned[8]>>;\n\
                 let pass: Pass<Box<Box<unsigned[8]>>> = {\n\
                     .input = nested, .output = passed,\n\
                 };\n\
                 y = passed.value.value;\n\
             }\n",
    );
    let leaf = design
        .signals
        .iter()
        .find(|signal| signal.path.ends_with(".nested.value.value"))
        .expect("the nested generic field should flatten to one leaf");
    assert_eq!(leaf.width, 8);
    let named_left = design
        .signals
        .iter()
        .find(|signal| signal.path.ends_with(".named.left.value.value"))
        .expect("the named T argument should bind independently of order");
    let named_right = design
        .signals
        .iter()
        .find(|signal| signal.path.ends_with(".named.right.value"))
        .expect("the named U argument should bind independently of order");
    assert_eq!(named_left.width, 8);
    assert_eq!(named_right.width, 16);
    assert!(design.signals.iter().any(|signal| {
        signal.path.contains(".pass.")
            && signal.path.ends_with(".input.value.value")
            && signal.width == 8
    }));
    assert!(!design.to_ir_string().contains("Unknown"));
}

#[test]
/// The design persists recursive concrete source layouts for its values.
fn design_persists_recursive_concrete_source_layouts() {
    let design = lower_src(
        "module m;\n\
             struct Header { flag: Bit, code: unsigned[7..0] }\n\
             struct Packet<T> { header: Header, payload: T }\n\
             entity E {\n\
                 packets: Packet<unsigned[16]>[3..1] in,\n\
                 count: integer<-3..4> in,\n\
             }\n\
             impl E {}\n",
    );

    let packets = design
        .source_layouts
        .get("E.packets")
        .expect("the aggregate root keeps a layout despite having no signal");
    assert_eq!(
        packets.index_range(),
        Some(LayoutRange { left: 3, right: 1 })
    );
    assert_eq!(packets.bit_width(), Some(75));
    assert_eq!(packets.leaf_count(), Some(9));
    let LayoutKind::Array { element, .. } = &packets.kind else {
        panic!("packets should remain a source array: {packets:#?}");
    };
    let LayoutKind::Struct { name, fields, .. } = &element.kind else {
        panic!("the array element should retain Packet: {element:#?}");
    };
    assert_eq!(name, "Packet");
    assert_eq!(
        fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["header", "payload"]
    );
    assert!(matches!(
        fields[1].layout.kind,
        LayoutKind::Packed {
            width: 16,
            range: Some(LayoutRange { left: 0, right: 15 }),
            ..
        }
    ));

    let code = design
        .source_layouts
        .get("E.packets[3].header.code")
        .expect("every flattened leaf also has its own concrete layout");
    assert!(matches!(
        code.kind,
        LayoutKind::Packed {
            width: 8,
            range: Some(LayoutRange { left: 7, right: 0 }),
            ..
        }
    ));

    let count = design
        .source_layouts
        .get("E.count")
        .expect("ranged integer");
    assert!(matches!(
        count.kind,
        LayoutKind::Scalar {
            width: 4,
            domain: ScalarDomain::Integer,
            value_range: Some((-3, 4)),
            ..
        }
    ));
}

#[test]
/// Testbench locals persist layouts without becoming hardware signals.
fn testbench_locals_persist_layouts_without_becoming_hardware_signals() {
    let design = lower_src(
        "module m;\n\
             struct Pair<T> { left: T, right: T }\n\
             #[test] entity T {}\n\
             impl T {\n\
                 let pairs: Pair<unsigned[8]>[2..1] = [\n\
                     { .left = 1, .right = 2 },\n\
                     { .left = 3, .right = 4 },\n\
                 ];\n\
             }\n",
    );

    let root = design
        .source_layouts
        .get("T.pairs")
        .expect("testbench local should retain its concrete layout");
    assert_eq!(root.bit_width(), Some(32));
    assert_eq!(root.leaf_count(), Some(4));
    assert!(matches!(
        root.kind,
        LayoutKind::Array {
            range: Some(LayoutRange { left: 2, right: 1 }),
            ..
        }
    ));
    assert!(matches!(
        design
            .source_layouts
            .get("T.pairs[2].left")
            .map(|layout| &layout.kind),
        Some(LayoutKind::Packed { width: 8, .. })
    ));
    assert!(design
        .signals
        .iter()
        .all(|signal| !signal.path.contains("pairs")));
}

#[test]
/// `clk.rising()` lowers to `Event`/`Old`/`Current` rather than a dedicated
/// edge node.
fn rising_lowers_to_event_old_current() {
    let d = lower_src(COUNTER);
    let rendered = d.to_ir_string();
    // clk.rising() expands into the explicit Event/Old/Current form. The
    // logic literals are resolved to their std positions ('0' -> 0,
    // '1' -> 1), so the IR carries plain constants, no raw chars.
    assert!(rendered.contains("Event(H.dut.clk)"));
    assert!(rendered.contains("Old(H.dut.clk) == 0"));
    assert!(rendered.contains("H.dut.clk == 1"));
    // The combinational driver and the next-state updates are present.
    assert!(rendered.contains("driver H.dut.count = H.dut.value"));
    assert!(rendered.contains("next H.dut.value = 0"));
}

#[test]
/// Priority conditions accumulate down a chain, so a later driver's guard
/// includes the negation of the earlier ones.
fn priority_conditions_accumulate() {
    let d = lower_src(COUNTER);
    let u = &d.event_blocks[0].updates;
    // First update guarded by rst == '1'.
    assert!(matches!(
        &u[0].cond,
        Some(Expr::Binary { op: BinOp::Eq, .. })
    ));
    // Second guarded by the negation AND en.
    assert!(matches!(
        &u[1].cond,
        Some(Expr::Binary { op: BinOp::And, .. })
    ));
}
