use super::*;

const LEFT: &str = "module providers::left;
    pub struct Config { pub width: integer, pub lanes: integer[-2..-1] }
    pub type Alias = Config;
    const OFFSET: integer = 5;
    pub const WIDTHS: integer[4..6] = [3, 5, 7];
    pub const DESCENDING: integer[6..4] = [31, 37, 41];
    pub const MATRIX: integer[-2..-1][10..11] = [[17, 19], [23, 29]];
    pub const PAIR: (integer, integer) = (17, 19);
    pub const CFG: Alias = Alias { .width = WIDTHS[4] + OFFSET, .lanes = [11, 13] };
    pub fn width() -> integer { return CFG.width; }
    fn foreign() -> integer;
    pub const DIRTY: Alias = Alias { .width = 8, .lanes = [foreign(), 13] };";

const RIGHT: &str = "module providers::right;
    pub struct Config { pub width: integer, pub lanes: integer[-2..-1] }
    pub const CFG: Config = Config { .width = 91, .lanes = [17, 19] };
    pub fn width() -> integer { return CFG.width; }";

const CORE: &str = "module core::primitive;
    pub enum Decision { false = 5, true = 17 }
    attr lang for Decision = \"bool\";";

fn with_sources(client: &str, mut check: impl FnMut(Source<'_, '_>, &[crate::syntax::Module])) {
    for sources in [[client, LEFT, RIGHT, CORE], [CORE, RIGHT, LEFT, client]] {
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
        check(Source::new(&declarations).with_enums(&enums), &modules);
    }
}

fn function<'a>(modules: &'a [crate::syntax::Module], name: &str) -> &'a FnDecl {
    modules
        .iter()
        .flat_map(|module| &module.items)
        .find_map(|item| match item {
            Item::Fn(function) if function.name.text == name => Some(function),
            _ => None,
        })
        .unwrap()
}

#[test]
fn string_constants_share_array_contracts_unicode_and_declared_ranges() {
    let client = "module client;
        type Text = Char[];
        const TEXT: Text = \"é🦀\";
        const LABELLED: Char[6..4] = \"Aé🦀\";
        struct Packet { pub text: Char[8..6] }
        fn literal() -> Char { return \"é🦀\"[1]; }
        fn text() -> Text { return TEXT; }
        fn first() -> Char { return TEXT[0]; }
        fn descending() -> Char { return LABELLED[4]; }
        fn length() -> integer { return text()'length; }
        fn combining() -> integer { return \"é\"'length; }
        fn bounds() -> integer { return LABELLED'left + LABELLED'right; }
        fn reindex(value: Char[12..10]) -> Char { return value[11]; }
        fn forwarded() -> Char { return reindex(LABELLED); }
        fn local() -> Char {
            let value: Text = \"Aé🦀\";
            value = \"xλY\";
            return value[1];
        }
        fn record() -> Char { return Packet { .text = \"Aé🦀\" }.text[7]; }
        fn empty() -> Text { return \"\"; }
        fn empty_length() -> integer { return empty()'length; }";
    with_sources(client, |source, modules| {
        for (name, expected) in [
            ("literal", '🦀'),
            ("first", 'é'),
            ("descending", '🦀'),
            ("forwarded", 'é'),
            ("local", 'λ'),
            ("record", 'é'),
        ] {
            assert_eq!(
                evaluate_function(function(modules, name), vec![], &source, 0),
                Ok(Value::Character(expected)),
                "{name}"
            );
        }
        for (name, expected) in [
            ("length", 2),
            ("combining", 2),
            ("bounds", 10),
            ("empty_length", 0),
        ] {
            assert_eq!(
                evaluate_function(function(modules, name), vec![], &source, 0),
                Ok(Value::Integer(expected)),
                "{name}"
            );
        }
        for (name, expected) in [("text", "é🦀"), ("empty", "")] {
            let Value::Array {
                element,
                values,
                left,
                descending,
            } = evaluate_function(function(modules, name), vec![], &source, 0).unwrap()
            else {
                panic!("{name} must use ordinary array storage")
            };
            assert_eq!(*element, Ty::Char);
            assert_eq!(
                values.as_ref(),
                expected.chars().map(Value::Character).collect::<Vec<_>>()
            );
            assert_eq!(left, 0);
            assert!(!descending);
        }
    });
}

#[test]
fn string_constant_elements_obey_context_without_inventing_encodings() {
    let client = "module client;
        enum Glyph { 'é' = 37, '🦀' = 91 }
        type Glyphs = Glyph[];
        type Text = Char[];
        type Numbers = integer[];
        const GLYPHS: Glyph[3..2] = \"é🦀\";
        const TEXT: Text = \"é🦀\";
        fn symbol() -> Glyph { return GLYPHS[2]; }
        fn bad_length() -> Char[2] { return \"Aé🦀\"; }
        fn bad_element() -> integer[3] { return \"123\"; }
        fn bad_empty() -> integer[] { return \"\"; }
        fn bad_glyph() -> Glyphs { return \"é?\"; }
        fn wrong_alias() -> Numbers { return TEXT; }";
    with_sources(client, |source, modules| {
        let declaration = function(modules, "symbol");
        let symbol = evaluate_function(declaration, vec![], &source, 0).unwrap();
        let owner = source
            .enum_type(declaration.ret.as_ref().unwrap(), None)
            .unwrap();
        assert!(matches!(&symbol, Value::Variant { owner: actual, .. } if *actual == owner));
        assert_eq!(symbol, source.enum_literal('🦀', owner).unwrap());
        assert_eq!(source.word(symbol), Some(91));
        for name in [
            "bad_length",
            "bad_element",
            "bad_empty",
            "bad_glyph",
            "wrong_alias",
        ] {
            let error = evaluate_function(function(modules, name), vec![], &source, 0).unwrap_err();
            assert_eq!(error.reason, Reason::Unsupported, "{name}: {error:?}");
        }
    });
}

#[test]
fn aggregate_projections_keep_definition_site_bindings_and_declared_labels() {
    let client = "module client; use providers::left::{self}; use providers::right::{self};
        const OFFSET: integer = 999;
        const WIDTHS: integer[4..6] = [99, 99, 99];
        fn array() -> integer { return left::WIDTHS[4]; }
        fn field() -> integer { return left::CFG.width; }
        fn nested_field() -> integer { return left::CFG.lanes[-1]; }
        fn descending_first() -> integer { return left::DESCENDING[6]; }
        fn descending_last() -> integer { return left::DESCENDING[4]; }
        fn nested_array() -> integer { return left::MATRIX[11][-2]; }
        fn tuple() -> integer { return left::PAIR.0 + left::PAIR.1; }
        fn qualified_helpers() -> integer { return left::width() + right::width(); }
        fn metadata() -> integer { return left::CFG.lanes'left + left::CFG.lanes'right + left::CFG.lanes'length; }
        fn descending_metadata() -> integer { return left::DESCENDING'left + left::DESCENDING'right + left::DESCENDING'low + left::DESCENDING'high; }
        fn direction() -> integer { return if left::DESCENDING'ascending { 99 } else { 7 }; }";
    with_sources(client, |source, modules| {
        for (name, expected) in [
            ("array", 3),
            ("field", 8),
            ("nested_field", 13),
            ("descending_first", 31),
            ("descending_last", 41),
            ("nested_array", 23),
            ("tuple", 36),
            ("qualified_helpers", 99),
            ("metadata", -1),
            ("descending_metadata", 20),
            ("direction", 7),
        ] {
            assert_eq!(
                evaluate_function(function(modules, name), vec![], &source, 0),
                Ok(Value::Integer(expected)),
                "{name}"
            );
        }
    });
}

#[test]
fn aggregate_call_arguments_preserve_nominal_owners_and_array_domains() {
    let client = "module client; use providers::left::{self}; use providers::right::{self};
        fn record(value: left::Alias) -> integer { return value.width + value.lanes[-2]; }
        fn array(value: integer[4..6]) -> integer { return value[5]; }
        fn valid_record() -> integer { return record(left::CFG); }
        fn valid_array() -> integer { return array(left::WIDTHS); }
        fn wrong_owner() -> integer { return record(right::CFG); }";
    with_sources(client, |source, modules| {
        for (name, expected) in [("valid_record", 19), ("valid_array", 5)] {
            assert_eq!(
                evaluate_function(function(modules, name), vec![], &source, 0),
                Ok(Value::Integer(expected)),
                "{name}"
            );
        }
        assert!(
            evaluate_function(function(modules, "wrong_owner"), vec![], &source, 0).is_err(),
            "equal field shapes must not merge unrelated nominal record types"
        );
    });
}

#[test]
fn range_values_keep_registered_owners_and_direction_through_calls_and_aliases() {
    let client = "module core::range_fixture;
        struct Bounds { pub l: integer, pub r: integer }
        attr lang for Bounds = \"range\";
        struct Lookalike { pub l: integer, pub r: integer }
        type Span = range;
        const DESCENDING: Span = 2..0;
        const COPY: Span = DESCENDING;
        const TABLE: integer[COPY] = [11, 13, 17];
        fn bounds() -> Span { let value: Span = COPY; return value; }
        fn left() -> integer { return bounds().l; }
        fn right() -> integer { return bounds().r; }
        fn table() -> integer { return TABLE[2] + TABLE[0]; }
        fn ascending() -> range { return -3..-1; }
        fn single() -> range { return 4..4; }
        fn other() -> Lookalike { return Lookalike { .l = 2, .r = 0 }; }
        fn foreign() -> integer;
        fn impure() -> range { return 0..foreign(); }";
    with_sources(client, |source, modules| {
        for (name, expected) in [
            ("bounds", (2, 0)),
            ("ascending", (-3, -1)),
            ("single", (4, 4)),
        ] {
            let value = evaluate_function(function(modules, name), vec![], &source, 0).unwrap();
            assert_eq!(source.range_bounds(&value), Some(expected), "{name}");
            assert!(source.word(value).is_none(), "a range is not a scalar word");
        }
        for (name, expected) in [("left", 2), ("right", 0), ("table", 28)] {
            assert_eq!(
                evaluate_function(function(modules, name), vec![], &source, 0),
                Ok(Value::Integer(expected)),
                "{name}"
            );
        }
        let other = evaluate_function(function(modules, "other"), vec![], &source, 0).unwrap();
        assert!(source.range_bounds(&other).is_none());
        assert!(evaluate_function(function(modules, "impure"), vec![], &source, 0).is_err());
    });
}

#[test]
fn invalid_or_effectful_aggregate_projections_remain_unknown() {
    let client = "module client; use providers::left::{self};
        fn low() -> integer { return left::WIDTHS[3]; }
        fn high() -> integer { return left::WIDTHS[7]; }
        fn negative() -> integer { return left::CFG.lanes[-3]; }
        fn missing_field() -> integer { return left::CFG.missing; }
        fn effectful() -> integer { return left::DIRTY.width; }";
    with_sources(client, |source, modules| {
        for name in ["low", "high", "negative", "missing_field", "effectful"] {
            let error = evaluate_function(function(modules, name), vec![], &source, 0).unwrap_err();
            assert!(error.span.start < error.span.end, "{name}");
        }
    });
}

#[test]
fn aggregate_storage_and_returns_apply_declared_contracts() {
    let client = "module client; use providers::left::{self}; use providers::right::{self};
        fn copy() -> left::Alias {
            let value: left::Alias = { .width = 6, .lanes = [2, 4] };
            value = left::Alias { ..value, .width = 9 };
            return value;
        }
        fn field() -> integer { return copy().width; }
        fn renamed(value: integer[2..0]) -> integer { return value[2] + value[0]; }
        fn bounds(value: integer[2..0]) -> integer { return value'left + value'right + value'length; }
        fn bound_helper() -> integer { return bounds(left::WIDTHS); }
        fn reindexed() -> integer { return renamed(left::WIDTHS); }
        fn nested(value: integer[-1..-2][11..10]) -> integer { return value[11][-1]; }
        fn nested_reindexed() -> integer { return nested(left::MATRIX); }
        fn tuple_arg(value: (integer[2..0], integer)) -> integer { return value.0[2] + value.1; }
        fn tuple_reindexed() -> integer { return tuple_arg((left::WIDTHS, 2)); }
        fn bad_return() -> left::Alias { return right::CFG; }
        fn bad_assignment() -> integer { let value: left::Alias = left::CFG; value = right::CFG; return value.width; }
        fn bad_length() -> integer { let value: integer[2] = left::WIDTHS; return value[0]; }
        fn bad_member() -> integer { let value: left::Alias = { .width = 'a', .lanes = [2, 4] }; return value.width; }
        fn bad_tuple() -> integer { let value: (integer, integer) = (3, 'a'); return value.0; }
        fn bad_spread() -> integer { return left::Alias { ..right::CFG, .width = 3 }.width; }
        fn dirty_spread() -> integer { return left::Alias { ..left::DIRTY, .width = 3 }.width; }";
    with_sources(client, |source, modules| {
        for (name, expected) in [
            ("field", 9),
            ("reindexed", 10),
            ("bound_helper", 5),
            ("nested_reindexed", 17),
            ("tuple_reindexed", 5),
        ] {
            assert_eq!(
                evaluate_function(function(modules, name), vec![], &source, 0),
                Ok(Value::Integer(expected)),
                "{name}"
            );
        }
        for name in [
            "bad_return",
            "bad_assignment",
            "bad_length",
            "bad_member",
            "bad_tuple",
            "bad_spread",
            "dirty_spread",
        ] {
            assert!(
                evaluate_function(function(modules, name), vec![], &source, 0).is_err(),
                "{name}"
            );
        }
    });
}
