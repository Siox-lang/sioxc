//! Constant table evaluation must retain the source type and declaration IDs.

use super::*;

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
