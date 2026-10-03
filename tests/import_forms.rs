//! The Rust import forms: nested groups, `self`, module aliases, globs,
//! `self::`/`super::` paths, block-level `use`, generic `type` aliases and
//! enum variant imports.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

/// The modules every test imports from.
const SHAPES: &[(&str, &str)] = &[
    (
        "shapes/geometry.siox",
        "module shapes::geometry;\n\
         pub enum Corner { NorthEast, SouthWest }\n\
         pub struct Packet<T> { pub valid: Bit, pub data: T }\n\
         pub type Pair<T> = Packet<T>;\n\
         pub type Hue = super::colors::Color;\n\
         pub fn double(x: integer) -> integer { return x * 2; }\n\
         pub const SIDES: integer = 4;\n",
    ),
    (
        "shapes/colors.siox",
        "module shapes::colors;\n\
         pub enum Color { Red, Green }\n\
         pub fn double(x: integer) -> integer { return x + x; }\n\
         pub const HUES: integer = 3;\n",
    ),
    (
        "shapes/facade.siox",
        "module shapes::facade;\npub use std::math;\npub use super::geometry::*;\n",
    ),
];

/// Compile `main` beside the shapes modules; the rendered diagnostics, and
/// whether the compilation succeeded.
fn compile(name: &str, main: &str) -> (String, bool) {
    let dir = std::env::temp_dir().join(format!("siox_import_forms_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("shapes")).unwrap();
    for (path, source) in SHAPES {
        std::fs::write(dir.join(path), source).unwrap();
    }
    let entry = dir.join("main.siox");
    std::fs::write(&entry, main).unwrap();
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(SourceInput::path(&entry), Emit::Metadata),
    );
    let _ = std::fs::remove_dir_all(dir);
    (compilation.render_diagnostics(), compilation.succeeded())
}

#[test]
fn every_import_form_resolves() {
    let (rendered, ok) = compile(
        "all",
        "module main;\n\
         use std::{math::{self, PI}, numeric::{Word8 = Byte}};\n\
         use shapes::geometry::*;\n\
         use shapes::colors::{self, Color::{self, Red}};\n\
         use self::Mode::{Fast, Slow};\n\
         use shapes::facade;\n\
         pub use shapes::colors::HUES;\n\
         enum Mode { Fast, Slow }\n\
         fn f(m: Mode, k: Corner) -> integer {\n\
             use shapes::colors::double;\n\
             use shapes::geometry::Corner::*;\n\
             let p: Pair<unsigned[8]> = Pair<unsigned[8]> { .valid = '1', .data = 9 };\n\
             let w: Word8 = 200;\n\
             let r: real = PI;\n\
             let c: Color = Red;\n\
             match k { NorthEast => { return 0; } SouthWest => {} }\n\
             match m {\n\
                 Fast => { return double(SIDES) + colors::HUES + math::max(1, 2); }\n\
                 Slow => { return facade::math::max(1, 2); }\n\
             }\n\
         }\n",
    );
    assert!(ok, "every form resolves:\n{rendered}");
    assert!(
        !rendered.contains("unused"),
        "no import is reported unused:\n{rendered}"
    );
}

/// Two globs bringing the same name are fine until the name is used, and the
/// use reports the ambiguity once, without a cascade.
#[test]
fn a_name_from_two_globs_is_ambiguous_only_where_used() {
    let header = "module main;\nuse shapes::geometry::*;\nuse shapes::colors::*;\n";
    let (rendered, ok) = compile(
        "unused_ambiguity",
        &format!("{header}fn f() -> integer {{ return SIDES + HUES; }}\n"),
    );
    assert!(ok, "an unused ambiguity is not an error:\n{rendered}");

    let (rendered, ok) = compile(
        "ambiguity",
        &format!("{header}fn f() -> integer {{ return double(2); }}\n"),
    );
    assert!(!ok);
    assert!(rendered.contains("`double` is ambiguous"), "{rendered}");
    assert!(
        !rendered.contains("unknown function"),
        "no cascade:\n{rendered}"
    );

    // An explicit import is stronger than both globs.
    let (rendered, ok) = compile(
        "explicit_wins",
        &format!(
            "{header}use shapes::colors::double;\nfn f() -> integer {{ return double(2); }}\n"
        ),
    );
    assert!(ok, "{rendered}");
}

#[test]
fn an_import_cannot_pass_through_a_type_alias() {
    let (rendered, ok) = compile(
        "through_alias",
        "module main;\nuse shapes::geometry::Hue::Red;\n",
    );
    assert!(!ok);
    assert!(
        rendered.contains("cannot pass through the type alias `Hue`"),
        "{rendered}"
    );
}

/// The variant's module is loaded even when nothing else imports it, and a
/// name the enum does not have is reported as such.
#[test]
fn a_variant_import_loads_its_module_and_checks_the_variant() {
    let (rendered, ok) = compile(
        "variant_only",
        "module main;\nuse shapes::colors::Color::Red;\n",
    );
    assert!(ok, "{rendered}");

    let (rendered, ok) = compile(
        "bad_variant",
        "module main;\nuse shapes::colors::Color::Blue;\n",
    );
    assert!(!ok);
    assert!(
        rendered.contains("`Blue` is not a variant of enum `Color`"),
        "{rendered}"
    );
}

#[test]
fn self_names_a_module_or_an_enum() {
    let (rendered, ok) = compile(
        "bad_self",
        "module main;\nuse shapes::colors::double::{self};\n",
    );
    assert!(!ok);
    assert!(rendered.contains("names a module or an enum"), "{rendered}");
}

/// Explicit names conflict loudly; a local declaration silently beats a glob.
#[test]
fn explicit_names_conflict_and_locals_beat_globs() {
    let (rendered, ok) = compile(
        "two_explicit",
        "module main;\nuse shapes::colors::double;\nuse shapes::geometry::double;\n",
    );
    assert!(
        !ok,
        "two explicit imports of one name conflict:\n{rendered}"
    );

    let (rendered, ok) = compile(
        "explicit_and_local",
        "module main;\nuse shapes::colors::double;\nfn double(x: integer) -> integer { return x; }\n",
    );
    assert!(
        !ok,
        "an explicit import conflicts with a local declaration:\n{rendered}"
    );

    let (rendered, ok) = compile(
        "local_beats_glob",
        "module main;\nuse shapes::colors::*;\n\
         fn double(x: integer) -> integer { return x; }\n\
         fn f() -> integer { return double(HUES); }\n",
    );
    assert!(ok, "a local declaration shadows a glob:\n{rendered}");
}

#[test]
/// A struct whose parameters shape its base's index range is that family
/// over the range: `F<8, 4>` is `F[3..-4]`, in a type, a constructor and
/// through an alias; the range written by hand is an error naming the
/// parameter form.
fn a_format_struct_is_its_family_over_the_range() {
    let format = "struct F<W: integer, R: integer>(Logic[W - R - 1 .. 0 - R]);\n";
    let (rendered, ok) = compile(
        "format_struct",
        &format!(
            "module main;\n{format}type Q = F<8, 4>;\n\
             #[test] entity T {{}}\n\
             impl T {{\n  let a: F<8, 4>;\n  let b: Q;\n\
             check: process {{ assert!(a'high == 3 and a'low == 0 - 4 and b'length == 8, \"F<8, 4>\"); }}\n}}\n"
        ),
    );
    assert!(ok, "{rendered}");
    let (rendered, ok) = compile(
        "format_struct_range",
        &format!("module main;\n{format}entity E {{ a: F[3..-4] in }}\nimpl E {{}}\n"),
    );
    assert!(
        !ok && rendered.contains("`F` takes its format as parameters"),
        "{rendered}"
    );
}
