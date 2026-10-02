//! User macros: declaration, the four invocation positions, forms, fragment
//! kinds, hygiene, definition-site names, imports, and the errors.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

/// A module the tests import macros from.
const DEBUG: &str = "module debug;\n\
    pub fn helper(x: integer) -> integer { return x + 100; }\n\
    pub macro bump($x: expr) { helper($x) }\n\
    macro private_one() { 1 }\n\
    pub macro via_private() { private_one!() }\n";

/// Compile `main` beside `debug`; the rendered diagnostics, whether it
/// succeeded, and with `Emit::Expanded` the expanded entry module.
fn compile(name: &str, main: &str, emit: Emit) -> (String, bool, String) {
    let dir = std::env::temp_dir().join(format!("siox_macros_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("debug.siox"), DEBUG).unwrap();
    let entry = dir.join("main.siox");
    std::fs::write(&entry, main).unwrap();
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(SourceInput::path(&entry), emit));
    let _ = std::fs::remove_dir_all(dir);
    let text = match &compilation.artifact {
        Some(siox::compiler::Artifact::Text(text)) => text.clone(),
        _ => String::new(),
    };
    (
        compilation.render_diagnostics(),
        compilation.succeeded(),
        text,
    )
}

fn check(name: &str, main: &str) -> (String, bool) {
    let (rendered, ok, _) = compile(name, main, Emit::Metadata);
    (rendered, ok)
}

fn expanded(name: &str, main: &str) -> String {
    let (rendered, _, text) = compile(name, main, Emit::Expanded);
    assert!(!text.is_empty(), "no expansion:\n{rendered}");
    text
}

#[test]
fn every_position_expands() {
    let text = expanded(
        "positions",
        "module main;\n\
         macro twice($x: expr) { $x + $x }\n\
         macro probe($name: ident, $ty: type) { let $name: $ty; }\n\
         macro make_fn($name: ident) { fn $name() -> integer { return 42; } }\n\
         macro inc($x: ident) { $x = $x + 1; }\n\
         make_fn!(answer);\n\
         entity E { a: unsigned[8] in, y: unsigned[8] out }\n\
         impl E { probe!(mid, unsigned[8]); mid = a; y = mid; }\n\
         fn f() -> integer { let n: integer = twice!(2) * 3; inc!(n); return n; }\n",
    );
    assert!(text.contains("fn answer() -> integer"), "{text}");
    assert!(text.contains("let mid: unsigned[8];"), "{text}");
    assert!(
        text.contains("(2 + 2) * 3"),
        "an expr argument and result keep their grouping:\n{text}"
    );
    assert!(text.contains("n = n + 1;"), "{text}");
    assert!(
        !text.contains("macro "),
        "declarations are removed:\n{text}"
    );
}

#[test]
fn every_position_type_checks() {
    let (rendered, ok) = check(
        "checks",
        "module main;\n\
         use debug::{bump, via_private};\n\
         macro twice($x: expr) { $x + $x }\n\
         macro make_fn($name: ident) { fn $name() -> integer { return twice!(21); } }\n\
         make_fn!(answer);\n\
         fn f() -> integer { return answer() + bump!(1) + via_private!(); }\n",
    );
    assert!(ok, "{rendered}");
    assert!(!rendered.contains("unused"), "{rendered}");
}

/// The form is chosen by argument count, then by fragment kind.
#[test]
fn forms_are_chosen_by_their_parameters() {
    let source = "module main;\n\
        macro pick($x: ident) { 1 }\n\
        macro pick($x: expr) { 2 }\n\
        macro pick($x: expr, $y: expr) { 3 }\n\
        fn f() -> integer { let a: integer = 0; return pick!(a) + pick!(a + 1) + pick!(a, a); }\n";
    let text = expanded("forms", source);
    assert!(text.contains("return 1 + 2 + 3;"), "{text}");

    let (rendered, ok) = check(
        "no_form",
        "module main;\nmacro one($x: type) { 1 }\nfn f() -> integer { return one!(1 +); }\n",
    );
    assert!(!ok);
    assert!(
        rendered.contains("no form of `one!` accepts these arguments"),
        "{rendered}"
    );
    assert!(
        rendered.contains("`one!($x: type)`"),
        "the forms are listed:\n{rendered}"
    );
}

/// A name the body declares is the macro's own; a name the caller passes is
/// the caller's.
#[test]
fn hygiene_separates_the_body_from_the_caller() {
    let text = expanded(
        "hygiene",
        "module main;\n\
         macro swap($a: ident, $b: ident) { let tmp: integer = $a; $a = $b; $b = tmp; }\n\
         fn f() -> integer { let tmp: integer = 1; let x: integer = 2; swap!(tmp, x); return tmp; }\n",
    );
    assert!(text.contains("let tmp#1: integer = tmp;"), "{text}");
    assert!(text.contains("x = tmp#1;"), "{text}");
    assert!(text.contains("return tmp;"), "{text}");
}

/// A free name in the body means what it means where the macro is declared.
#[test]
fn free_names_resolve_at_the_definition() {
    let text = expanded(
        "def_site",
        "module main;\nuse debug::bump;\n\
         fn helper(x: integer) -> integer { return x; }\n\
         fn f() -> integer { return bump!(1); }\n",
    );
    assert!(text.contains("debug::helper(1)"), "{text}");
}

#[test]
fn imported_macros_are_linted_and_checked() {
    let (rendered, ok) = check("unused", "module main;\nuse debug::bump;\n");
    assert!(ok, "{rendered}");
    assert!(rendered.contains("unused import: `bump`"), "{rendered}");

    let (rendered, ok) = check(
        "private",
        "module main;\nfn f() -> integer { return debug::private_one!(); }\nuse debug::helper;\n\
         fn g() -> integer { return helper(1); }\n",
    );
    assert!(!ok);
    assert!(
        rendered.contains("macro `private_one` is private"),
        "{rendered}"
    );

    let (rendered, ok) = check(
        "qualified",
        "module main;\nuse debug::helper;\n\
         fn f() -> integer { return debug::bump!(1) + helper(0); }\n",
    );
    assert!(ok, "a qualified path needs no import:\n{rendered}");
}

#[test]
fn unknown_macros_and_parameters_are_errors() {
    let (rendered, ok) = check("unknown", "module main;\nnothing!(1);\n");
    assert!(!ok);
    assert!(
        rendered.contains("cannot find macro `nothing!`"),
        "{rendered}"
    );

    let (rendered, ok) = check(
        "unknown_expr",
        "module main;\nfn f() -> integer { return nothing!(1); }\n",
    );
    assert!(!ok);
    assert!(
        rendered.contains("cannot find macro `nothing!`"),
        "{rendered}"
    );

    let (rendered, ok) = check(
        "bad_param",
        "module main;\nmacro m($x: expr) { $y }\nfn f() -> integer { return m!(1); }\n",
    );
    assert!(!ok);
    assert!(
        rendered.contains("`$y` is not a parameter of `m!`"),
        "{rendered}"
    );

    let (rendered, ok) = check("bad_kind", "module main;\nmacro m($x: number) { $x }\n");
    assert!(!ok);
    assert!(
        rendered.contains("`number` is not a fragment kind"),
        "{rendered}"
    );
}

#[test]
fn expansion_is_bounded_and_declares_no_macros() {
    let (rendered, ok) = check(
        "recursion",
        "module main;\nmacro forever($x: expr) { forever!($x) }\n\
         fn f() -> integer { return forever!(1); }\n",
    );
    assert!(!ok);
    assert!(
        rendered.contains("macro expansion limit exceeded"),
        "{rendered}"
    );

    let (rendered, ok) = check(
        "generated",
        "module main;\nmacro outer() { macro inner() { 1 } }\nouter!();\n",
    );
    assert!(!ok);
    assert!(rendered.contains("cannot declare a macro"), "{rendered}");
}

/// A mistake in the expanded body says which macro it came from.
#[test]
fn body_errors_name_the_macro() {
    let (rendered, ok) = check(
        "body_error",
        "module main;\nmacro broken($x: expr) { $x + }\nfn f() -> integer { return broken!(1); }\n",
    );
    assert!(!ok);
    assert!(
        rendered.contains("in an expansion of `broken!`"),
        "{rendered}"
    );
}

/// The built-in macros are untouched, and a user macro can wrap them.
#[test]
fn builtin_macros_still_work() {
    let (rendered, ok) = check(
        "builtin",
        "module main;\nmacro check($c: expr) { assert!($c, \"check\"); }\n\
         #[test]\nentity T {}\nimpl T { process { check!(1 == 1); assert!(true); print!(\"{}\", 1); } }\n",
    );
    assert!(ok, "{rendered}");
}

/// `...` takes the remaining arguments; `for macro` repeats over them,
/// `join` separates the repetitions, `$xs` forwards them, and `$xs'length`
/// counts them.
#[test]
fn repetition_unrolls_at_expansion_time() {
    let text = expanded(
        "repetition",
        "module main;\n\
         macro all($cs: expr...) { for macro $c in $cs { assert!($c); } }\n\
         macro both($cs: expr...) { for macro $c in $cs join and { $c } }\n\
         macro count($xs: tokens...) { $xs'length }\n\
         macro sum($xs: expr...) { 0 for macro $x in $xs { + $x } }\n\
         macro trace($fmt: expr, $args: expr...) { print!($fmt, $args); }\n\
         #[test]\nentity T {}\n\
         impl T { process {\n\
             all!(1 == 1, 2 == 2);\n\
             assert!(both!(true, false) or count!(a, b c, (d, e)) == 3);\n\
             assert!(sum!(1, 2) == count!());\n\
             trace!(\"{} {}\", 1, 2);\n\
         } }\n",
    );
    assert!(
        text.contains("assert!(1 == 1);\n") && text.contains("assert!(2 == 2);\n"),
        "{text}"
    );
    assert!(text.contains("true and false or 3 == 3"), "{text}");
    assert!(text.contains("0 + 1 + 2 == 0"), "{text}");
    assert!(text.contains("print!(\"{} {}\", 1, 2);"), "{text}");
}

#[test]
fn repetition_errors() {
    let (rendered, ok) = check(
        "early_variadic",
        "module main;\nmacro m($xs: expr..., $y: expr) { 1 }\n",
    );
    assert!(!ok);
    assert!(
        rendered.contains("only the last macro parameter"),
        "{rendered}"
    );

    let (rendered, ok) = check(
        "malformed_for",
        "module main;\nmacro m($xs: expr...) { for macro $x $xs { $x } }\n\
         fn f() -> integer { return m!(1); }\n",
    );
    assert!(!ok);
    assert!(
        rendered.contains("expected `for macro $x in $xs { … }`"),
        "{rendered}"
    );

    // A variadic form needs only its fixed arguments.
    let (rendered, ok) = check(
        "variadic_count",
        "module main;\nmacro m($a: expr, $xs: expr...) { $a }\n\
         fn f() -> integer { return m!(1) + m!(1, 2, 3); }\nfn g() -> integer { return m!(); }\n",
    );
    assert!(!ok);
    assert!(rendered.contains("no form of `m!`"), "{rendered}");
    assert!(
        rendered.contains("`m!($a: expr, $xs: expr...)`"),
        "{rendered}"
    );
}

/// A type error in expanded body code says which macro it came from.
#[test]
fn later_stage_errors_in_a_body_name_the_macro() {
    let (rendered, ok) = check(
        "type_error",
        "module main;\nmacro bad($x: expr) { $x + undefined_name }\n\
         fn f() -> integer { return bad!(1); }\n",
    );
    assert!(!ok);
    assert!(rendered.contains("in an expansion of `bad!`"), "{rendered}");
}
