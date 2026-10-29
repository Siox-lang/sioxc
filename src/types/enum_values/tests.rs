use super::*;
use crate::diag::FileId;

fn parse(sources: &[&str]) -> (Vec<Module>, Resolved) {
    let mut sink = DiagnosticSink::default();
    let mut modules: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(index, source)| crate::syntax::parse_module(FileId(index as u32), source, &mut sink))
        .collect();
    crate::syntax::macros::expand(&mut modules, &HashMap::new(), &mut sink);
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    (modules, resolved)
}

fn owner(modules: &[Module], resolved: &Resolved, name: &str) -> DefId {
    modules
        .iter()
        .flat_map(|module| &module.items)
        .find_map(|item| match item {
            Item::Enum(declaration) if declaration.name.text == name => {
                resolved.declared(declaration.name.span)
            }
            _ => None,
        })
        .unwrap()
}

fn numbers(values: &EnumValues, owner: DefId) -> Vec<Option<i128>> {
    values
        .members(owner)
        .unwrap()
        .iter()
        .map(|(_, value)| *value)
        .collect()
}

#[test]
fn enum_numbers_execute_source_boolean_operators_and_use_their_source_discriminants() {
    let sources = [
        "module model;
        fn first() -> integer { if (2 < 4) or (3 < 2) { return 9; } return 3; }
        enum Palette { Low = first(), High }",
        "module core::primitive;
        pub enum Bool { false = 5, true = 17 } attr lang for Bool = \"bool\";",
        "module core::ops; use core::primitive::Bool;
        pub trait Or<Rhs, Out> { fn or(self, rhs: Rhs) -> Out; }
        attr lang for Or = \"or\";
        impl Or<Bool, Bool> for Bool { fn or(self, rhs: Bool) -> Self { return rhs; } }",
    ];
    for sources in [sources, [sources[2], sources[1], sources[0]]] {
        let (modules, resolved) = parse(&sources);
        let declarations = DeclarationIndex::collect(&resolved, &modules);
        let values = EnumValues::collect(&declarations);
        assert_eq!(
            numbers(&values, owner(&modules, &resolved, "Palette")),
            [Some(3), Some(4)]
        );
        let unregistered: Vec<_> = sources
            .iter()
            .map(|source| source.replace("attr lang for Or = \"or\";", ""))
            .collect();
        let (modules, resolved) =
            parse(&unregistered.iter().map(String::as_str).collect::<Vec<_>>());
        let declarations = DeclarationIndex::collect(&resolved, &modules);
        let values = EnumValues::collect(&declarations);
        assert_eq!(
            numbers(&values, owner(&modules, &resolved, "Palette")),
            [None, None]
        );
    }
}

#[test]
fn const_context_builtin_and_registered_roles_share_source_dispatch() {
    let (modules, resolved) = parse(&[
        "module model; enum Wire { Low = 3, High = 9 }
         impl And<Wire, Wire> for Wire {
             fn and(self, rhs: Wire) -> Wire { return self; }
         }
         fn first() -> integer {
             if (Wire::Low and Wire::High) == Wire::Low { return 3; }
             return 9;
         }
         enum Computed { First = first(), Next }",
        include_str!("../../../core/primitive.siox"),
        "module core::ops;
         pub trait And<Rhs, Out> { fn and(self, rhs: Rhs) -> Out; }
         attr lang for And = \"and\";",
    ]);
    let Item::Impl(implementation) = &modules[0].items[1] else {
        panic!("impl")
    };
    let fallback = resolved
        .resolved(implementation.trait_.as_ref().unwrap().span)
        .unwrap();
    assert_eq!(resolved.lang_of(fallback), Some("and"));
    assert_ne!(Some(fallback), resolved.lang("and"));
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let values = EnumValues::collect(&declarations);
    assert_eq!(
        numbers(&values, owner(&modules, &resolved, "Computed")),
        [Some(3), Some(4)]
    );
}

#[test]
fn const_context_custom_operator_uses_its_registered_role_and_symbol() {
    let (modules, resolved) = parse(&[
        "module model; enum Wire { Low = 3, High = 9 }
         impl CustomOperator<\"pick\", Wire, Wire> for Wire {
             attr precedence = 35;
             fn apply(self, rhs: Wire) -> Wire { return rhs; }
         }
         fn first() -> integer { return integer(Wire::Low pick Wire::High); }
         enum Computed { First = first(), Next }",
        "module core::ops;
         pub trait CustomOperator<op: string, Rhs, Out> { fn apply(self, rhs: Rhs) -> Out; }
         attr lang for CustomOperator = \"custom_operator\";",
    ]);
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let values = EnumValues::collect(&declarations);
    assert_eq!(
        numbers(&values, owner(&modules, &resolved, "Computed")),
        [Some(9), Some(10)]
    );
}

#[test]
fn unknown_explicit_discriminants_do_not_restart_implicit_numbering() {
    let (modules, resolved) = parse(&["module model;
        extern \"C\" { fn runtime() -> integer; }
        enum Palette { Unknown = runtime(), Following, Reset = 9, Next }"]);
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let values = EnumValues::collect(&declarations);
    assert_eq!(
        numbers(&values, owner(&modules, &resolved, "Palette")),
        [None, None, Some(9), Some(10)]
    );
}

#[test]
fn const_context_source_ordering_bodies_control_enum_constant_calls() {
    let (modules, resolved) = parse(&[
        "module model; use core::primitive::Bool; use core::cmp::Ord;
        enum Mode { Low = 9, High }
        impl Ord<Mode> for Mode {
            fn lt(self, rhs: Mode) -> Bool { return integer(self) > integer(rhs); }
            fn le(self, rhs: Mode) -> Bool { return integer(self) >= integer(rhs); }
        }
        fn choose() -> integer { if Mode::Low < Mode::High { return 21; } return 13; }
        enum Computed { First = choose(), Next }",
        include_str!("../../../core/primitive.siox"),
        "module core::cmp; use core::primitive::Bool;
        pub trait Ord<Rhs> { fn lt(self, rhs: Rhs) -> Bool; fn le(self, rhs: Rhs) -> Bool; }
        attr lang for Ord = \"ord\";",
    ]);
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let values = EnumValues::collect(&declarations);
    assert_eq!(
        numbers(&values, owner(&modules, &resolved, "Computed")),
        [Some(13), Some(14)]
    );
}

#[test]
fn const_context_enum_constructors_return_kernel_integers() {
    let (modules, resolved) = parse(&["module model;
        enum Mode { Low = 9, High }
        enum Computed { First = integer(Mode::Low), Next }"]);
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let values = EnumValues::collect(&declarations);
    assert_eq!(
        numbers(&values, owner(&modules, &resolved, "Computed")),
        [Some(9), Some(10)]
    );
}

#[test]
fn generated_enum_ordering_uses_each_owners_member_ids() {
    let (modules, resolved) = parse(&[
        "module model;
        macro modes($Name: ident, $start: expr) { enum $Name { Low = $start, High } }
        modes!(Left, 9);
        modes!(Right, 3);
        use core::primitive::Bool;
        use core::cmp::Ord;
        impl Ord<Left> for Left {
            fn lt(self, rhs: Left) -> Bool { return integer(self) < integer(rhs); }
            fn le(self, rhs: Left) -> Bool { return integer(self) <= integer(rhs); }
        }
        fn choose() -> integer { if Left::Low < Left::High { return 21; } return 13; }
        enum Computed { First = choose(), Next }",
        include_str!("../../../core/primitive.siox"),
        "module core::cmp; use core::primitive::Bool;
        pub trait Ord<Rhs> {
            fn lt(self, rhs: Rhs) -> Bool;
            fn le(self, rhs: Rhs) -> Bool;
        }
        attr lang for Ord = \"ord\";",
    ]);
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let values = EnumValues::collect(&declarations);
    for (name, expected) in [("Left", [9, 10]), ("Right", [3, 4]), ("Computed", [21, 22])] {
        assert_eq!(
            numbers(&values, owner(&modules, &resolved, name)),
            expected.map(Some)
        );
    }
}

#[test]
fn generated_enum_diagnostics_do_not_read_a_later_owners_member_ids() {
    let (modules, resolved) = parse(&[
        "module model;
        macro modes($Name: ident, $start: expr) { enum $Name { Low = $start, High } }
        modes!(Negative, -1);
        modes!(Positive, 3);",
        include_str!("../../../core/primitive.siox"),
    ]);
    let mut sink = DiagnosticSink::default();
    let _ = check(&modules, &resolved, &mut sink);
    let errors: Vec<_> = sink
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code == Some(codes::TYPE_MISMATCH))
        .collect();
    assert_eq!(errors.len(), 1, "{:?}", sink.diagnostics());
    assert!(errors[0].message.contains("Negative::Low"));
    assert!(errors[0].message.contains("negative discriminant -1"));
}

#[test]
fn boolean_member_identity_does_not_depend_on_its_own_encoding() {
    let (modules, resolved) = parse(&["module core::primitive;
        fn encoding(n: integer) -> integer {
            if n == 0 { return 17; } return 5;
        }
        pub enum Bool { false = encoding(1), true = encoding(0) }
        attr lang for Bool = \"bool\";"]);
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let values = EnumValues::collect(&declarations);
    assert_eq!(
        numbers(&values, resolved.lang("bool").unwrap()),
        [Some(5), Some(17)]
    );
}

#[test]
fn unknown_enum_encodings_do_not_invent_order_or_boolean_roles() {
    let (modules, resolved) = parse(&[
        "module model;
        extern \"C\" { fn runtime() -> integer; }
        enum Lookalike { false = runtime(), true }
        fn ordered() -> integer {
            if Lookalike::false < Lookalike::true { return 9; } return 3;
        }
        fn mistaken() -> integer { if Lookalike::true { return 9; } return 3; }
        enum Ordered { Low = ordered(), High }
        enum Mistaken { Low = mistaken(), High }",
        include_str!("../../../core/primitive.siox"),
    ]);
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let values = EnumValues::collect(&declarations);
    for name in ["Ordered", "Mistaken"] {
        assert_eq!(
            numbers(&values, owner(&modules, &resolved, name)),
            [None, None]
        );
    }
}

#[test]
fn enum_numbers_keep_kernel_integer_constructors() {
    let (modules, resolved) = parse(&["module model;
        enum Palette { Low = integer(9), High }"]);
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let values = EnumValues::collect(&declarations);
    assert_eq!(
        numbers(&values, owner(&modules, &resolved, "Palette")),
        [Some(9), Some(10)]
    );
}

#[test]
fn a_last_discriminant_at_the_word_boundary_does_not_overflow_an_unused_successor() {
    let (modules, resolved) = parse(&["module model;
        enum Palette { Last = 18446744073709551615 }"]);
    let declarations = DeclarationIndex::collect(&resolved, &modules);
    let values = EnumValues::collect(&declarations);
    let palette = owner(&modules, &resolved, "Palette");
    assert_eq!(numbers(&values, palette), [Some(i128::from(u64::MAX))]);
    assert_eq!(values.width(palette), Some(64));
}

#[test]
fn normalized_helper_lengths_refine_unknown_values_and_report_collisions_once() {
    let (mut modules, resolved) = parse(&[
        "module model;
        fn first() -> integer { return [10, 20]'length; }
        enum Palette { Low = first(), High = 2 }",
        include_str!("../../../core/primitive.siox"),
    ]);
    let mut sink = DiagnosticSink::default();
    let mut typed = check(&modules, &resolved, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    let palette = owner(&modules, &resolved, "Palette");
    assert_eq!(numbers(&typed.enum_values, palette), [None, Some(2)]);
    fold_value_lengths(&mut modules, &typed);
    refresh_enum_values(&modules, &resolved, &mut typed, &mut sink);
    assert_eq!(numbers(&typed.enum_values, palette), [Some(2), Some(2)]);
    assert_eq!(
        sink.diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == Some(codes::DUPLICATE_ITEM))
            .count(),
        1
    );
    refresh_enum_values(&modules, &resolved, &mut typed, &mut sink);
    assert_eq!(sink.diagnostics().len(), 1);
}

#[test]
fn normalized_negative_discriminants_are_diagnosed_at_the_enum_initializer() {
    let source = "module model;
        fn first() -> integer { return 0 - [10]'length; }
        enum Palette { Low = first(), High }";
    let (mut modules, resolved) = parse(&[source, include_str!("../../../core/primitive.siox")]);
    let mut sink = DiagnosticSink::default();
    let mut typed = check(&modules, &resolved, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    fold_value_lengths(&mut modules, &typed);
    refresh_enum_values(&modules, &resolved, &mut typed, &mut sink);
    let errors: Vec<_> = sink
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code == Some(codes::TYPE_MISMATCH))
        .collect();
    assert_eq!(errors.len(), 1, "{:?}", sink.diagnostics());
    assert!(errors[0].message.contains("negative discriminant -1"));
    let Item::Enum(declaration) = &modules[0].items[1] else {
        panic!("enum")
    };
    assert_eq!(
        errors[0].primary,
        declaration.variants[0].value.as_ref().map(expr_span)
    );
}

#[test]
fn normalized_enum_diagnostics_keep_source_order() {
    let (mut modules, resolved) = parse(&[
        "module model;
        fn count() -> integer { return [10, 20]'length; }
        enum Duplicate { First = count(), Second = 2 }
        enum Negative { First = 0 - count() }",
        include_str!("../../../core/primitive.siox"),
    ]);
    let mut sink = DiagnosticSink::default();
    let mut typed = check(&modules, &resolved, &mut sink);
    assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    fold_value_lengths(&mut modules, &typed);
    refresh_enum_values(&modules, &resolved, &mut typed, &mut sink);
    assert_eq!(
        sink.diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>(),
        [Some(codes::DUPLICATE_ITEM), Some(codes::TYPE_MISMATCH)]
    );
}
