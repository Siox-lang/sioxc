//! `core`: the compiler's own declarations, compiled into `sioxc`, found by
//! lang items (proposals/core-std.md).

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

/// Compile `main` with `std` at `std_root`; the rendered diagnostics and
/// whether it succeeded.
fn compile(name: &str, main: &str, std_root: &str) -> (String, bool) {
    let dir = std::env::temp_dir().join(format!("siox_core_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.siox");
    std::fs::write(&entry, main).unwrap();
    let compilation = Compiler::new(std_root).compile(CompileRequest::new(
        SourceInput::path(&entry),
        Emit::Metadata,
    ));
    let _ = std::fs::remove_dir_all(dir);
    (compilation.render_diagnostics(), compilation.succeeded())
}

const STD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/std");

/// A hook re-exported by std is the same declaration as core's: an impl
/// through either path makes a type a condition.
#[test]
fn core_and_std_paths_name_the_same_hooks() {
    let (rendered, ok) = compile(
        "same_hooks",
        "module main;\n\
         use core::ops::Boolean;\n\
         struct A(integer);\n\
         struct B(integer);\n\
         impl Boolean for A { fn as_bool(self) -> Bool { return true; } }\n\
         impl std::ops::Boolean for B { fn as_bool(self) -> Bool { return false; } }\n\
         fn f(a: A, b: B) -> integer { if a { return 1; } if b { return 2; } return 0; }\n\
         fn g(x: core::primitive::Bool) -> std::primitive::Bool { return x; }\n",
        STD,
    );
    assert!(ok, "{rendered}");
}

/// `core` is built in: with no standard library at all, its prelude still
/// gives every module `Bool`, the hooks and the directives.
#[test]
fn core_needs_no_standard_library() {
    let empty = std::env::temp_dir().join(format!("siox_core_nostd_{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    let (rendered, ok) = compile(
        "no_std",
        "module main;\n\
         struct Flag(integer);\n\
         impl Boolean for Flag { fn as_bool(self) -> Bool { return true; } }\n\
         fn f(x: Flag) -> Bool { if x { return true; } return false; }\n\
         #[allow(unused_param)]\n\
         fn g(unused: integer) -> string { return \"core\"; }\n",
        empty.to_str().unwrap(),
    );
    let _ = std::fs::remove_dir_all(&empty);
    assert!(ok, "{rendered}");
}

/// Only `core` and `std` may say what a declaration is to the compiler.
#[test]
fn user_modules_cannot_bind_lang_items() {
    let (rendered, ok) = compile(
        "user_lang",
        "module main;\npub trait Mine {}\nattr lang for Mine = \"operator\";\n",
        STD,
    );
    assert!(!ok);
    assert!(
        rendered.contains("`lang` is reserved to `core` and `std`"),
        "{rendered}"
    );
}

/// A user trait spelled like a hook is still an ordinary trait.
#[test]
fn a_hook_name_alone_grants_nothing() {
    let (rendered, ok) = compile(
        "spelling",
        "module main;\n\
         trait Boolean { fn as_bool(self) -> Bool; }\n\
         struct Flag(integer);\n\
         impl Boolean for Flag { fn as_bool(self) -> Bool { return true; } }\n\
         fn f(x: Flag) -> integer { if x { return 1; } return 0; }\n",
        STD,
    );
    assert!(
        !ok,
        "a namesake trait is not the condition hook:\n{rendered}"
    );
}

/// A type head names a type: a library enum variant spelled like a type
/// (`std::attrs::RomStyle::Logic`) must not capture `Logic` for the
/// compiler's own uses of it. It used to make every `signed` division in
/// `std::bits` fail to type-check as soon as `std::attrs` was imported.
#[test]
fn a_variant_named_like_a_type_does_not_capture_it() {
    let (rendered, ok) = compile(
        "variant_named_logic",
        "module main;\nuse std::attrs::RomStyle;\n\
         enum Style { Logic, Bool }\n\
         fn f(a: signed[8], b: signed[8]) -> signed[8] { return a / b; }\n\
         fn g(s: RomStyle) -> Bool { return s == RomStyle::Logic; }\n",
        STD,
    );
    assert!(ok, "{rendered}");
}
