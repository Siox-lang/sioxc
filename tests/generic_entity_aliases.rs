#![cfg(feature = "cli")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
use siox::diag::codes;
use std::process::Command;

const PROVIDER: &str = r#"module generic_alias_provider;
const BASE: integer = 6;
const N: integer = 40;
pub attr mark: integer for entity, let = 1;
pub entity Device<T, N: integer> { data: T in, value: T out, count: integer out }
impl<T, N: integer> Device<T, N> { attr mark = 17; value = data; count = N; }
pub type Pipe<T, N: integer> = Device<T, (N + BASE)>;
pub type Route<T, Count: integer> = Pipe<T, Count>;
pub type Group<T, Count: integer> = Route<T, Count>[2];
pub type Select<T> = T;
fn calc_width(n: integer) -> integer { return n + 1; }
pub type PrivateWord = unsigned[calc_width(BASE)];
"#;

#[test]
fn generic_entity_aliases_preserve_type_value_and_named_arguments_natively() {
    let dir =
        std::env::temp_dir().join(format!("siox_generic_alias_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("generic_alias_provider.siox"), PROVIDER).unwrap();
    let source = r#"module generic_alias_test;
use generic_alias_provider::{Route, Group, Select, Pipe, PrivateWord, mark};
fn calc_width(n: integer) -> integer { return n + 20; }
const BASE: integer = 99;
const N: integer = 100;
#[test] entity GenericAliases {}
impl GenericAliases {
    let direct: Route<real, 3> = { .data = 2.5 };
    let group: Group<integer, 4>;
    group[0] = Route<integer, 4> { .data = 11 };
    group[1] = Route<Count = 4, T = integer> { .data = 22 };
    let selected: Select<Pipe<integer, 5>> = { .data = 31 };
    let private_word: PrivateWord = PrivateWord(3);
    await 1ns;
    assert!(direct.value == 2.5 and direct.count == 9);
    assert!(group[0].value == 11 and group[1].value == 22);
    assert!(group[0].count == 10 and group[1].count == 10);
    assert!(selected.value == 31 and selected.count == 11);
    assert!(direct'mark == 17 and selected'mark == 17);
    assert!(integer(private_word) == 3 and private_word'length == 7);
}
"#;
    let binary = dir.join("native");
    let compiler = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"));
    let compiled = compiler.compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("generic_alias_test.siox"), source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compiled.succeeded(),
        "{}{:?}",
        compiled.render_diagnostics(),
        compiled.failure
    );
    let hierarchy = compiled.hierarchy.as_ref().unwrap();
    let root = hierarchy.instance(hierarchy.roots[0]);
    assert_eq!(root.children.len(), 4);
    for &id in &root.children {
        let child = hierarchy.instance(id);
        assert_eq!(
            hierarchy.qualified_entity_name(child.entity_id),
            Some("generic_alias_provider::Device")
        );
        let expected = match child.name.as_str() {
            "direct" => 9,
            "selected" => 11,
            _ => 10,
        };
        assert!(child.params.iter().any(|(name, value)| name == "N" && *value == siox::elab::ParamValue::Int(expected)), "{:?}", child.params);
    }
    let ran = Command::new(binary)
        .arg("-o")
        .arg(dir.join("out.vcd"))
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn missing_or_mismatched_generic_alias_arguments_are_diagnostics() {
    let compiler = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"));
    for ty in [
        "Alias",
        "Alias<integer>",
        "Alias<Other = integer>",
        "Alias<8>",
        "Value<integer>",
        "Pair<Left = integer, Left = real>",
    ] {
        let source = format!("module bad_alias; entity Device<T> {{}} impl<T> Device<T> {{}} type Alias<T> = Device<T>; type Value<N: integer> = Device<integer>; type Pair<Left, Right> = Device<Left>; entity Top {{}} impl Top {{ let bad: {ty} = {{}}; }}");
        let compiled = compiler.compile(CompileRequest::new(
            SourceInput::memory("bad_alias.siox", source),
            Emit::Metadata,
        ));
        if ty == "Alias<integer>" {
            assert!(
                compiled.succeeded(),
                "{}{:?}",
                compiled.render_diagnostics(),
                compiled.failure
            );
        } else {
            assert!(!compiled.succeeded(), "{ty} unexpectedly compiled");
            assert_eq!(
                compiled
                    .diagnostics
                    .diagnostics()
                    .iter()
                    .filter(|d| d.code == Some(codes::TYPE_MISMATCH))
                    .count(),
                1,
                "{}{:?}",
                compiled.render_diagnostics(),
                compiled.failure
            );
        }
    }
}

#[test]
fn generic_data_aliases_and_constructor_chains_share_normalization() {
    let dir = std::env::temp_dir().join(format!("siox_generic_data_alias_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = r#"module generic_data_alias;
struct Packet<T> { pub data: T }
type Message<T> = Packet<T>;
type Pair<T> = (T, T);
type Bits<N: integer> = unsigned[(N + 6)];
type Word = Bits<3>;
trait ScalarKind<Input> {}
impl ScalarKind<integer> for integer {}
type Scalar<T: ScalarKind<integer>> = T;
struct Counter(integer);
impl Add<Scalar<integer>, Scalar<integer>> for Counter {
    fn add(self, rhs: Scalar<integer>) -> Scalar<integer> { return integer(self) + rhs; }
}
entity Wire<N: integer> { data: Bits<N> in, value: Bits<N> out }
impl<N: integer> Wire<N> { value = data; }
#[test] entity AliasData {}
impl AliasData {
    let message: Message<integer> = { .data = 9 };
    let pair: Pair<integer> = (7, 8);
    let (first, second): Pair<integer> = pair;
    let bits: Bits<3> = Bits<3>(5);
    let wider: Bits<4> = Bits<4>(11);
    let word: Word = Word(6);
    let wire: Wire<3> = { .data = word };
    let wide: Wire<4> = { .data = wider };
    let counter: Counter = Counter(4);
    await 1ns;
    assert!(message.data == 9);
    assert!(pair.0 == 7 and pair.1 == 8 and first == 7 and second == 8);
    assert!(integer(bits) == 5 and bits'length == 9);
    assert!(integer(word) == 6 and word'length == 9);
    assert!(integer(wire.value) == 6);
    assert!(integer(wide.value) == 11 and wider'length == 10);
    assert!(counter + 3 == 7);
}

"#;
    let binary = dir.join("native");
    let compiler = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"));
    let compiled = compiler.compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("generic_data_alias.siox"), source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compiled.succeeded(),
        "{}{:?}",
        compiled.render_diagnostics(),
        compiled.failure
    );
    let ran = Command::new(binary)
        .arg("-o")
        .arg(dir.join("out.vcd"))
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn generic_aliases_preserve_unsized_array_alias_indexing() {
    let dir = std::env::temp_dir().join(format!("siox_unsized_alias_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = r#"module unsized_alias;
type Text = Char[];
type Sized<N: integer> = Text[N];
#[test] entity T {}
impl T {
    let chars: Sized<3> = "abc";
    await 1ns;
    assert!(chars'length == 3 and chars[0] == 'a' and chars[2] == 'c');
}
"#;
    let binary = dir.join("native");
    let compiler = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"));
    let compiled = compiler.compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("unsized_alias.siox"), source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compiled.succeeded(),
        "{}{:?}",
        compiled.render_diagnostics(),
        compiled.failure
    );
    let ran = Command::new(binary)
        .arg("-o")
        .arg(dir.join("out.vcd"))
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    std::fs::remove_dir_all(dir).unwrap();
}
